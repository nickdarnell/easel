//! `frametv serve`: a web app for browsing and managing what's on each TV, where guests can
//! send photos to a TV from any browser, plus optional LocalSend targets (one per TV).
//!
//! Guests (anyone who can reach the page) may view the TVs' photos and send new ones. If an
//! admin password is set (in the web app's settings), everything else (deleting, mattes,
//! slideshow, pairing, ...) needs it; without one, everyone on the network is an admin.
//!
//! All settings live in the config file, so a container needs nothing but a volume for it.

use std::collections::HashSet;
use std::convert::Infallible;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, RwLock};
use std::time::Duration;

use anyhow::{Context, anyhow};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path as UrlPath, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, broadcast, mpsc};

use samsung_frame::art::{ArtClient, UploadOptions};
use samsung_frame::config::{Config, TvConfig};
use samsung_frame::protocol::category;
use samsung_frame::sync::{self, Framing, TvState};
use samsung_frame::{discovery, image_prep, localsend, pairing, rest, wol};

const INDEX_HTML: &str = include_str!("web/index.html");
const MAX_UPLOAD: usize = 512 * 1024 * 1024;

pub struct ServeOptions {
    pub bind: IpAddr,
    pub port: u16,
    pub localsend_port: u16,
}

struct Tv {
    name: String,
    cfg: StdMutex<TvConfig>,
    art: Mutex<Option<Arc<ArtClient>>>,
    /// Serializes socket transfers (uploads, thumbnails) to this TV.
    xfer: Mutex<()>,
}

impl Tv {
    fn new(cfg: TvConfig) -> Arc<Self> {
        Arc::new(Tv { name: cfg.name.clone(), cfg: StdMutex::new(cfg), art: Mutex::new(None), xfer: Mutex::new(()) })
    }

    fn config(&self) -> TvConfig {
        self.cfg.lock().unwrap().clone()
    }
}

struct Hub {
    cfg: StdMutex<Config>,
    cfg_path: PathBuf,
    state_dir: PathBuf,
    data_dir: PathBuf,
    tvs: RwLock<Vec<Arc<Tv>>>,
    events: broadcast::Sender<Value>,
    /// Advertised LocalSend targets (alias, port).
    localsend: StdMutex<Vec<Value>>,
    localsend_error: StdMutex<Option<String>>,
    localsend_port: u16,
    /// The running LocalSend receiver; replacing it stops the old one.
    localsend_task: Mutex<Option<localsend::Receiver>>,
}

type Shared = Arc<Hub>;

struct ApiError(StatusCode, anyhow::Error);

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let msg = format!("{:#}", self.1);
        if self.0.is_server_error() {
            tracing::warn!("request failed: {msg}");
        }
        (self.0, Json(json!({"error": msg}))).into_response()
    }
}

fn status_err(status: StatusCode, msg: impl Into<String>) -> ApiError {
    ApiError(status, anyhow!(msg.into()))
}

type ApiResult<T> = Result<T, ApiError>;

fn upload_settings(cfg: &TvConfig) -> sync::UploadSettings {
    let (width, height) = cfg.panel_size();
    sync::UploadSettings {
        prep: image_prep::PrepOptions {
            fit: cfg.fit.as_deref().and_then(|f| f.parse().ok()).unwrap_or_default(),
            width,
            height,
            ..Default::default()
        },
        matte: cfg.matte.clone().unwrap_or_else(|| "auto".into()),
        portrait_matte: cfg.portrait_matte.clone().unwrap_or_else(|| "shadowbox_polar".into()),
    }
}

/// `salt$sha256(salt + password)`, as stored in the config.
fn hash_password(password: &str) -> String {
    let salt = uuid::Uuid::new_v4().simple().to_string();
    let digest = Sha256::digest(format!("{salt}{password}").as_bytes());
    format!("{salt}${}", hex::encode(digest))
}

fn check_password(stored: &str, given: &str) -> bool {
    let Some((salt, expected)) = stored.split_once('$') else { return false };
    let Ok(expected) = hex::decode(expected) else { return false };
    let given = Sha256::digest(format!("{salt}{given}").as_bytes());
    // Compare without early exit.
    expected.len() == given.len() && given.iter().zip(&expected).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

fn blank_to_none(s: &str) -> Option<String> {
    Some(s.trim().to_string()).filter(|s| !s.is_empty())
}

fn is_disconnect(e: &samsung_frame::Error) -> bool {
    matches!(e, samsung_frame::Error::Disconnected | samsung_frame::Error::WebSocket(_))
}

impl Hub {
    fn tv(&self, name: &str) -> ApiResult<Arc<Tv>> {
        self.tvs
            .read()
            .unwrap()
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(name))
            .cloned()
            .ok_or_else(|| status_err(StatusCode::NOT_FOUND, format!("no TV named `{name}`")))
    }

    fn all_tvs(&self) -> Vec<Arc<Tv>> {
        self.tvs.read().unwrap().clone()
    }

    fn emit(&self, v: Value) {
        let _ = self.events.send(v);
    }

    fn server_settings(&self) -> samsung_frame::config::ServerSettings {
        self.cfg.lock().unwrap().server.clone()
    }

    fn is_admin(&self, headers: &HeaderMap) -> bool {
        let Some(stored) = self.server_settings().admin_password else { return true };
        let given = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("");
        check_password(&stored, given)
    }

    fn require_admin(&self, headers: &HeaderMap) -> ApiResult<()> {
        if self.is_admin(headers) {
            Ok(())
        } else {
            Err(status_err(StatusCode::UNAUTHORIZED, "this needs the admin password"))
        }
    }

    fn save_config(&self, update: impl FnOnce(&mut Config)) -> anyhow::Result<()> {
        let mut cfg = self.cfg.lock().unwrap();
        update(&mut cfg);
        cfg.save(&self.cfg_path).with_context(|| format!("saving {}", self.cfg_path.display()))?;
        Ok(())
    }

    fn state_file(&self, tv: &Tv) -> PathBuf {
        sync::state_path(&self.state_dir, &tv.name)
    }

    fn thumb_dir(&self, tv: &Tv) -> PathBuf {
        sync::state_path(&self.data_dir.join("thumbs"), &tv.name).with_extension("")
    }

    /// The art connection, reconnecting if the TV dropped it.
    async fn art(self: &Arc<Self>, tv: &Arc<Tv>) -> anyhow::Result<Arc<ArtClient>> {
        let mut slot = tv.art.lock().await;
        if let Some(a) = slot.as_ref()
            && a.is_connected()
        {
            return Ok(Arc::clone(a));
        }
        let cfg = tv.config();
        let client_name = self.cfg.lock().unwrap().client_name.clone();
        let endpoint = cfg.endpoint(&client_name);
        let art = tokio::time::timeout(Duration::from_secs(15), ArtClient::connect(&endpoint))
            .await
            .map_err(|_| anyhow!("{} did not answer (is it on?)", tv.name))?
            .with_context(|| format!("connecting to {}", tv.name))?;
        if let Some(token) = art.issued_token()
            && cfg.token.as_deref() != Some(token)
        {
            tv.cfg.lock().unwrap().token = Some(token.to_string());
            let name = tv.name.clone();
            let token = token.to_string();
            self.save_config(|c| {
                if let Some(stored) = c.find_mut(&name) {
                    stored.token = Some(token);
                }
            })?;
        }
        let art = Arc::new(art);
        // Relay the TV's own events (selection changes, art mode, slideshow) to the UI.
        let mut rx = art.subscribe();
        let (hub, name) = (Arc::clone(self), tv.name.clone());
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(m) => hub.emit(json!({"type": "tv-event", "tv": name, "event": m.event, "data": m.data})),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        });
        *slot = Some(Arc::clone(&art));
        Ok(art)
    }

    /// Run `f` against the TV, retrying once on a fresh connection if the old one had died.
    async fn with_art<T, F, Fut>(self: &Arc<Self>, tv: &Arc<Tv>, f: F) -> anyhow::Result<T>
    where
        F: Fn(Arc<ArtClient>) -> Fut,
        Fut: std::future::Future<Output = samsung_frame::Result<T>>,
    {
        let art = self.art(tv).await?;
        match f(art).await {
            Err(e) if is_disconnect(&e) => {
                tv.art.lock().await.take();
                let art = self.art(tv).await?;
                Ok(f(art).await?)
            }
            other => Ok(other?),
        }
    }

    /// Prepare and upload a file, retrying once on a fresh connection.
    async fn upload_with_retry(
        self: &Arc<Self>,
        tv: &Arc<Tv>,
        path: &Path,
        settings: &sync::UploadSettings,
    ) -> anyhow::Result<sync::UploadResult> {
        let art = self.art(tv).await?;
        Ok(match sync::upload_file(&art, path, settings).await {
            Err(e) if is_disconnect(&e) => {
                tv.art.lock().await.take();
                let art = self.art(tv).await?;
                sync::upload_file(&art, path, settings).await?
            }
            other => other?,
        })
    }

    /// Remove photos from the TV along with their cached thumbnails.
    async fn delete_from_tv(self: &Arc<Self>, tv: &Arc<Tv>, ids: Vec<String>) -> anyhow::Result<()> {
        self.with_art(tv, |a| {
            let ids = ids.clone();
            async move { a.delete(&ids).await }
        })
        .await?;
        for id in &ids {
            if let Some(p) = find_thumb(&self.thumb_dir(tv), id) {
                let _ = std::fs::remove_file(p);
            }
        }
        Ok(())
    }

    /// Save an incoming photo next to the TV's synced photos (or under `photos/<tv>`), upload
    /// it unless it is already there with the same framing, and optionally display it.
    async fn ingest(
        self: &Arc<Self>,
        tv: &Arc<Tv>,
        file_name: &str,
        data: &[u8],
        show: bool,
        framing: Option<Framing>,
    ) -> anyhow::Result<Value> {
        let cfg = tv.config();
        let dir = cfg.folder.clone().unwrap_or_else(|| self.data_dir.join("photos").join(&tv.name));
        let hash = hex::encode(Sha256::digest(data));
        let path = save_unique(&dir, &localsend::sanitize_file_name(file_name), data, &hash)?;

        let _guard = tv.xfer.lock().await;
        let state_file = self.state_file(tv);
        let mut state = TvState::load(&state_file)?;
        let on_tv: HashSet<String> = self
            .with_art(tv, |a| async move { a.content_list(Some(category::MY_PHOTOS)).await })
            .await?
            .into_iter()
            .map(|i| i.content_id)
            .collect();
        let existing = state.content_for_hash(&hash).filter(|id| on_tv.contains(*id)).map(str::to_owned);
        // Same photo, and either no explicit framing asked for or the same one as before.
        let same = existing
            .as_ref()
            .filter(|id| framing.is_none() || state.uploads.get(*id).and_then(|r| r.framing.as_ref()) == framing.as_ref());
        let (content_id, duplicate) = match same {
            Some(id) => (id.clone(), true),
            None => {
                self.emit(json!({"type": "uploading", "tv": tv.name, "name": file_name}));
                let mut settings = upload_settings(&cfg);
                if let Some(f) = &framing {
                    f.apply(&mut settings);
                }
                let result = self.upload_with_retry(tv, &path, &settings).await?;
                state.record_framed(&result.content_id, &hash, &path, framing.clone());
                // Re-sent with different framing: the new version replaces the old one.
                if let Some(old) = existing {
                    state.uploads.remove(&old);
                    if let Err(e) = self.delete_from_tv(tv, vec![old]).await {
                        tracing::warn!("removing the previous version from {}: {e:#}", tv.name);
                    }
                }
                state.save(&state_file)?;
                (result.content_id, false)
            }
        };
        if show {
            let id = content_id.clone();
            self.with_art(tv, |a| {
                let id = id.clone();
                async move { a.select_image(&id, None, true).await }
            })
            .await?;
        }
        let out = json!({
            "type": "uploaded", "tv": tv.name, "content_id": content_id, "name": file_name,
            "duplicate": duplicate, "shown": show,
        });
        self.emit(out.clone());
        Ok(out)
    }

    /// (Re)start the LocalSend targets to match the current settings and TVs.
    async fn restart_localsend(self: &Arc<Self>) {
        let mut slot = self.localsend_task.lock().await;
        if slot.take().is_some() {
            // Give the aborted tasks a moment to release their ports.
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        self.localsend.lock().unwrap().clear();
        *self.localsend_error.lock().unwrap() = None;
        let settings = self.server_settings();
        if settings.localsend && !self.all_tvs().is_empty() {
            match start_localsend(self, settings.localsend_pin).await {
                Ok(r) => *slot = Some(r),
                Err(e) => {
                    tracing::warn!("LocalSend disabled: {e:#}");
                    *self.localsend_error.lock().unwrap() = Some(format!("{e:#}"));
                }
            }
        }
        self.emit(json!({"type": "settings-changed"}));
    }
}

/// Write `data` into `dir` as `name`, adding ` (2)` etc. on collisions, unless an identical
/// file is already there.
fn save_unique(dir: &Path, name: &str, data: &[u8], hash: &str) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let stem = Path::new(name).file_stem().and_then(|s| s.to_str()).unwrap_or("image").to_string();
    let ext = Path::new(name).extension().and_then(|s| s.to_str()).map(|e| format!(".{e}")).unwrap_or_default();
    for n in 1..1000 {
        let candidate = if n == 1 { dir.join(name) } else { dir.join(format!("{stem} ({n}){ext}")) };
        if candidate.exists() {
            if sync::hash_file(&candidate).is_ok_and(|h| h == hash) {
                return Ok(candidate);
            }
            continue;
        }
        std::fs::write(&candidate, data)?;
        return Ok(candidate);
    }
    Err(anyhow!("too many files named {name}"))
}

pub async fn run(cfg: Config, cfg_path: PathBuf, state_dir: PathBuf, opts: ServeOptions) -> anyhow::Result<()> {
    let data_dir = state_dir.parent().map(Path::to_path_buf).unwrap_or_else(|| state_dir.clone());
    let tvs = cfg.tvs.iter().cloned().map(Tv::new).collect::<Vec<_>>();
    let (events, _) = broadcast::channel(256);
    let hub = Arc::new(Hub {
        cfg: StdMutex::new(cfg),
        cfg_path,
        state_dir,
        data_dir,
        tvs: RwLock::new(tvs),
        events,
        localsend: StdMutex::new(Vec::new()),
        localsend_error: StdMutex::new(None),
        localsend_port: opts.localsend_port,
        localsend_task: Mutex::new(None),
    });
    hub.restart_localsend().await;

    let app = Router::new()
        .route("/", get(index))
        .route("/tv/{tv}", get(index))
        .route("/manifest.webmanifest", get(manifest))
        .route("/api/info", get(info))
        .route("/api/settings", get(get_settings).put(put_settings))
        .route("/api/preview", post(preview))
        .route("/api/events", get(events_stream))
        .route("/api/qr.svg", get(qr))
        .route("/api/discover", post(discover))
        .route("/api/tvs", get(list_tvs).post(add_tv))
        .route("/api/tvs/{tv}", axum::routing::patch(update_tv).delete(forget_tv))
        .route("/api/tvs/{tv}/status", get(tv_status))
        .route("/api/tvs/{tv}/photos", get(photos))
        .route("/api/tvs/{tv}/thumb/{id}", get(thumb))
        .route("/api/tvs/{tv}/upload", post(upload))
        .route("/api/tvs/{tv}/photos/{id}/show", post(show))
        .route("/api/tvs/{tv}/photos/{id}/matte", post(set_matte))
        .route("/api/tvs/{tv}/photos/{id}/favorite", post(favorite))
        .route("/api/tvs/{tv}/photos/{id}/original", get(original))
        .route("/api/tvs/{tv}/photos/{id}/reframe", post(reframe))
        .route("/api/tvs/{tv}/photos/{id}", axum::routing::delete(delete_photo))
        .route("/api/tvs/{tv}/mattes", get(mattes))
        .route("/api/tvs/{tv}/artmode", post(artmode))
        .route("/api/tvs/{tv}/slideshow", post(slideshow))
        .route("/api/tvs/{tv}/sync", post(sync_folder))
        .route("/api/tvs/{tv}/wake", post(wake))
        .route("/api/tvs/{tv}/share-card", post(share_card))
        .layer(DefaultBodyLimit::max(MAX_UPLOAD))
        .with_state(Arc::clone(&hub));

    let listener = tokio::net::TcpListener::bind((opts.bind, opts.port))
        .await
        .with_context(|| format!("could not listen on port {} (busy? try --port)", opts.port))?;
    let settings = hub.server_settings();
    println!("Frame TV web app:  http://localhost:{}", opts.port);
    if let Some(url) = &settings.public_url {
        println!("  for guests:      {url}");
    } else if !opts.bind.is_loopback()
        && let Some(ip) = discovery::local_ipv4()
    {
        // Inside a bridged container this is the container's private address; still worth
        // printing (it's right with host networking) but say what to do if it looks wrong.
        println!("  on your network: http://{ip}:{}", opts.port);
        if Path::new("/.dockerenv").exists() {
            println!("  (in Docker: if that's not this machine's LAN address, set the share address in Settings)");
        }
    }
    if settings.admin_password.is_none() {
        println!("  no admin password yet: set one in the web app's settings (gear button)");
    }
    for d in hub.localsend.lock().unwrap().iter() {
        println!("LocalSend target:  \"{}\" (port {})", d["alias"].as_str().unwrap_or("?"), d["port"]);
    }
    if hub.all_tvs().is_empty() {
        println!("No TVs paired yet: open the web app and choose \"Add a TV\".");
    }
    println!("Ctrl-C to stop.");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let ctrl_c = async {
                let _ = tokio::signal::ctrl_c().await;
            };
            #[cfg(unix)]
            let term = async {
                if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                    s.recv().await;
                }
            };
            #[cfg(not(unix))]
            let term = std::future::pending::<()>();
            tokio::select! { _ = ctrl_c => {}, _ = term => {} }
        })
        .await?;
    Ok(())
}

async fn start_localsend(hub: &Shared, pin: Option<String>) -> anyhow::Result<localsend::Receiver> {
    // One target per TV, plus "All Frames" when there are several.
    let tvs = hub.all_tvs();
    let mut targets: Vec<(String, Vec<Arc<Tv>>)> = tvs.iter().map(|t| (t.name.clone(), vec![Arc::clone(t)])).collect();
    if tvs.len() > 1 {
        targets.push(("All Frames".into(), tvs.clone()));
    }
    let devices = targets
        .iter()
        .map(|(alias, _)| localsend::Device {
            alias: alias.clone(),
            fingerprint: localsend::fingerprint_for(alias),
            port: 0,
            device_model: "Samsung Frame".into(),
            pin: pin.clone(),
        })
        .collect::<Vec<_>>();
    let (tx, mut rx) = mpsc::channel::<localsend::Received>(32);
    let receiver = localsend::serve(devices.clone(), hub.localsend_port, tx).await?;
    *hub.localsend.lock().unwrap() =
        devices.iter().zip(&receiver.ports).map(|(d, p)| json!({"alias": d.alias, "port": p})).collect();

    let hub = Arc::clone(hub);
    tokio::spawn(async move {
        while let Some(file) = rx.recv().await {
            let Some((_, tvs)) = targets.get(file.device) else { continue };
            let show = hub.server_settings().show_on_receive;
            tracing::info!("received {} from {} via LocalSend", file.file_name, file.sender_alias);
            for tv in tvs {
                let (hub, tv, file_name, data) = (Arc::clone(&hub), Arc::clone(tv), file.file_name.clone(), file.data.clone());
                let show = show && file.last_in_session;
                let from = file.sender_alias.clone();
                // TVs work in parallel; each TV's transfers are serialized by its own lock.
                tokio::spawn(async move {
                    if let Err(e) = hub.ingest(&tv, &file_name, &data, show, None).await {
                        hub.emit(json!({"type": "error", "tv": tv.name, "message": format!("{file_name} from {from}: {e:#}")}));
                    }
                });
            }
        }
    });
    Ok(receiver)
}

async fn index() -> impl IntoResponse {
    ([(header::CACHE_CONTROL, "no-cache")], Html(INDEX_HTML))
}

async fn manifest() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/manifest+json")],
        json!({
            "name": "Easel", "short_name": "Easel", "start_url": "/", "display": "standalone",
            "background_color": "#15171c", "theme_color": "#15171c",
            "icons": [{"src": "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 64 64'%3E%3Crect x='4' y='10' width='56' height='40' rx='3' fill='%23c9a96e'/%3E%3Crect x='10' y='16' width='44' height='28' fill='%23345'/%3E%3C/svg%3E", "sizes": "any", "type": "image/svg+xml"}]
        })
        .to_string(),
    )
}

async fn info(State(hub): State<Shared>, headers: HeaderMap) -> Json<Value> {
    let settings = hub.server_settings();
    Json(json!({
        "admin": hub.is_admin(&headers),
        "password_set": settings.admin_password.is_some(),
        "public_url": settings.public_url,
        "localsend": *hub.localsend.lock().unwrap(),
        "heif": image_prep::heif_supported(),
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

fn settings_json(hub: &Hub) -> Value {
    let s = hub.server_settings();
    json!({
        "password_set": s.admin_password.is_some(),
        "public_url": s.public_url,
        "localsend": s.localsend,
        "localsend_pin": s.localsend_pin,
        "show_on_receive": s.show_on_receive,
        "localsend_targets": *hub.localsend.lock().unwrap(),
        "localsend_error": *hub.localsend_error.lock().unwrap(),
        "in_docker": Path::new("/.dockerenv").exists(),
    })
}

async fn get_settings(State(hub): State<Shared>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    hub.require_admin(&headers)?;
    Ok(Json(settings_json(&hub)))
}

/// Fields left out stay as they are; empty strings clear.
#[derive(Deserialize)]
struct SettingsBody {
    password: Option<String>,
    public_url: Option<String>,
    localsend: Option<bool>,
    localsend_pin: Option<String>,
    show_on_receive: Option<bool>,
}

async fn put_settings(State(hub): State<Shared>, headers: HeaderMap, Json(b): Json<SettingsBody>) -> ApiResult<Json<Value>> {
    hub.require_admin(&headers)?;
    let public_url = match b.public_url.as_deref().map(str::trim) {
        Some("") => Some(None),
        Some(u) if u.starts_with("http://") || u.starts_with("https://") => Some(Some(u.trim_end_matches('/').to_string())),
        Some(u) => return Err(status_err(StatusCode::BAD_REQUEST, format!("`{u}` should start with http:// or https://"))),
        None => None,
    };
    if let Some(pin) = b.localsend_pin.as_deref().map(str::trim)
        && pin.len() > 32
    {
        return Err(status_err(StatusCode::BAD_REQUEST, "that PIN is too long"));
    }
    let before = hub.server_settings();
    hub.save_config(|c| {
        let s = &mut c.server;
        if let Some(p) = &b.password {
            s.admin_password = (!p.is_empty()).then(|| hash_password(p));
        }
        if let Some(u) = public_url {
            s.public_url = u;
        }
        if let Some(v) = b.localsend {
            s.localsend = v;
        }
        if let Some(p) = &b.localsend_pin {
            s.localsend_pin = blank_to_none(p);
        }
        if let Some(v) = b.show_on_receive {
            s.show_on_receive = v;
        }
    })?;
    let after = hub.server_settings();
    if (before.localsend, &before.localsend_pin) != (after.localsend, &after.localsend_pin) {
        hub.restart_localsend().await;
    } else {
        hub.emit(json!({"type": "settings-changed"}));
    }
    Ok(Json(settings_json(&hub)))
}

/// Turn any photo the server can read (HEIC included) into a browser-friendly JPEG preview.
async fn preview(body: Bytes) -> ApiResult<Response> {
    if body.is_empty() {
        return Err(status_err(StatusCode::BAD_REQUEST, "empty image"));
    }
    let jpeg = tokio::task::spawn_blocking(move || image_prep::preview(&body, 1600))
        .await?
        .map_err(|e| status_err(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    Ok(([(header::CONTENT_TYPE, "image/jpeg")], jpeg).into_response())
}

async fn events_stream(State(hub): State<Shared>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = hub.events.subscribe();
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(v) => return Some((Ok(Event::default().data(v.to_string())), rx)),
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return None,
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(Deserialize)]
struct QrQuery {
    text: String,
}

async fn qr(Query(q): Query<QrQuery>) -> ApiResult<Response> {
    use qrcode::render::svg;
    let code = qrcode::QrCode::new(q.text.as_bytes()).map_err(|e| status_err(StatusCode::BAD_REQUEST, format!("{e:?}")))?;
    let svg = code
        .render::<svg::Color>()
        .min_dimensions(256, 256)
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .build();
    Ok(([(header::CONTENT_TYPE, "image/svg+xml"), (header::CACHE_CONTROL, "max-age=3600")], svg).into_response())
}

async fn discover(State(hub): State<Shared>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    hub.require_admin(&headers)?;
    let (ssdp, scan) = tokio::join!(discovery::ssdp(Duration::from_secs(2)), discovery::scan_subnet());
    let known: HashSet<String> = hub.all_tvs().iter().map(|t| t.config().host).collect();
    let mut seen = HashSet::new();
    let found: Vec<Value> = ssdp
        .unwrap_or_default()
        .into_iter()
        .chain(scan.unwrap_or_default())
        .filter(|d| seen.insert(d.ip.to_string()))
        .map(|d| {
            let ip = d.ip.to_string();
            json!({
                "host": ip, "name": pairing::friendly_name(&d.info.display_name()), "model": d.info.device.model_name,
                "frame": d.info.is_frame(), "power": d.info.power_state(), "paired": known.contains(&ip),
            })
        })
        .collect();
    Ok(Json(Value::Array(found)))
}

async fn list_tvs(State(hub): State<Shared>) -> Json<Value> {
    let checks = hub.all_tvs().into_iter().map(|tv| async move {
        let cfg = tv.config();
        let power = match rest::device_info(&cfg.host, Duration::from_millis(1500)).await {
            Ok(i) => i.power_state().to_string(),
            Err(_) => "off".into(),
        };
        json!({
            "name": cfg.name, "host": cfg.host, "model": cfg.model, "resolution": cfg.panel_size(),
            "folder": cfg.folder, "matte": cfg.matte, "portrait_matte": cfg.portrait_matte, "fit": cfg.fit,
            "power": power, "can_wake": cfg.mac.is_some(),
        })
    });
    Json(Value::Array(futures_util::future::join_all(checks).await))
}

#[derive(Deserialize)]
struct AddTv {
    host: String,
    name: Option<String>,
}

/// Pair a TV. Takes up to a minute: someone has to press Allow on the TV.
async fn add_tv(State(hub): State<Shared>, headers: HeaderMap, Json(b): Json<AddTv>) -> ApiResult<Json<Value>> {
    hub.require_admin(&headers)?;
    let host = b.host.trim();
    if host.is_empty() {
        return Err(status_err(StatusCode::BAD_REQUEST, "enter the TV's IP address"));
    }
    let name = b.name.as_deref().map(str::trim).filter(|n| !n.is_empty());
    let cfg = hub.cfg.lock().unwrap().clone();
    let port = cfg.find(host).map(|t| t.port).unwrap_or(samsung_frame::protocol::SECURE_PORT);
    let paired = pairing::pair(&cfg, host, name, port, Duration::from_secs(60)).await?;
    let tv_cfg = paired.tv.clone();
    hub.save_config(|c| {
        if c.tvs.is_empty() {
            c.default_tv = Some(tv_cfg.name.clone());
        }
        c.upsert(tv_cfg.clone());
    })?;
    {
        let mut tvs = hub.tvs.write().unwrap();
        tvs.retain(|t| !t.name.eq_ignore_ascii_case(&tv_cfg.name) && t.config().host != tv_cfg.host);
        tvs.push(Tv::new(tv_cfg.clone()));
    }
    hub.emit(json!({"type": "tvs-changed"}));
    let h = Arc::clone(&hub);
    tokio::spawn(async move { h.restart_localsend().await });
    Ok(Json(json!({
        "name": tv_cfg.name, "api_version": paired.api_version, "photos": paired.photo_count, "frame": paired.is_frame,
    })))
}

#[derive(Deserialize)]
struct UpdateTv {
    matte: Option<String>,
    portrait_matte: Option<String>,
    fit: Option<String>,
    /// Folder (on the machine running the server) to sync from and to save received photos in.
    /// Empty string clears it.
    folder: Option<String>,
}

async fn update_tv(
    State(hub): State<Shared>,
    headers: HeaderMap,
    UrlPath(name): UrlPath<String>,
    Json(b): Json<UpdateTv>,
) -> ApiResult<StatusCode> {
    hub.require_admin(&headers)?;
    let tv = hub.tv(&name)?;
    if let Some(f) = b.fit.as_deref().filter(|f| !f.is_empty()) {
        f.parse::<image_prep::Fit>().map_err(|e| status_err(StatusCode::BAD_REQUEST, e))?;
    }
    let folder = match b.folder.as_deref().map(str::trim) {
        Some("") => Some(None),
        Some(f) => {
            let p = PathBuf::from(f);
            if !p.is_dir() {
                return Err(status_err(StatusCode::BAD_REQUEST, format!("{f} is not a folder the server can see")));
            }
            Some(Some(p))
        }
        None => None,
    };
    let updated = {
        let mut c = tv.cfg.lock().unwrap();
        let blank = |s: String| Some(s).filter(|s| !s.is_empty());
        if let Some(m) = b.matte {
            c.matte = blank(m);
        }
        if let Some(m) = b.portrait_matte {
            c.portrait_matte = blank(m);
        }
        if let Some(f) = b.fit {
            c.fit = blank(f);
        }
        if let Some(f) = folder {
            c.folder = f;
        }
        c.clone()
    };
    hub.save_config(|c| c.upsert(updated))?;
    hub.emit(json!({"type": "tvs-changed"}));
    Ok(StatusCode::NO_CONTENT)
}

async fn forget_tv(State(hub): State<Shared>, headers: HeaderMap, UrlPath(name): UrlPath<String>) -> ApiResult<StatusCode> {
    hub.require_admin(&headers)?;
    let tv = hub.tv(&name)?;
    hub.tvs.write().unwrap().retain(|t| !Arc::ptr_eq(t, &tv));
    if let Some(art) = tv.art.lock().await.take()
        && let Ok(art) = Arc::try_unwrap(art)
    {
        art.close().await;
    }
    hub.save_config(|c| {
        c.remove(&tv.name);
        if c.default_tv.as_deref().is_some_and(|d| d.eq_ignore_ascii_case(&tv.name)) {
            c.default_tv = c.tvs.first().map(|t| t.name.clone());
        }
    })?;
    hub.emit(json!({"type": "tvs-changed"}));
    let h = Arc::clone(&hub);
    tokio::spawn(async move { h.restart_localsend().await });
    Ok(StatusCode::NO_CONTENT)
}

async fn tv_status(State(hub): State<Shared>, UrlPath(name): UrlPath<String>) -> ApiResult<Json<Value>> {
    let tv = hub.tv(&name)?;
    let art = hub.art(&tv).await?;
    let (artmode, current, slideshow) = tokio::join!(art.artmode(), art.current_artwork(), art.slideshow_status());
    Ok(Json(json!({
        "artmode": artmode.ok(),
        "current": current.ok(),
        "slideshow": slideshow.ok().map(|s| json!({"minutes": s.minutes, "shuffle": s.shuffle})),
    })))
}

#[derive(Deserialize)]
struct PhotosQuery {
    category: Option<String>,
}

async fn photos(State(hub): State<Shared>, UrlPath(name): UrlPath<String>, Query(q): Query<PhotosQuery>) -> ApiResult<Json<Value>> {
    let tv = hub.tv(&name)?;
    let cat = match q.category.as_deref() {
        Some("favorites") => category::FAVORITES,
        _ => category::MY_PHOTOS,
    };
    let mut items = hub.with_art(&tv, |a| async move { a.content_list(Some(cat)).await }).await?;
    // Newest first, like a camera roll.
    items.sort_by(|a, b| b.image_date.cmp(&a.image_date).then_with(|| b.content_id.cmp(&a.content_id)));
    let state = TvState::load(&hub.state_file(&tv)).unwrap_or_default();
    let dir = hub.thumb_dir(&tv);
    let out: Vec<Value> = items
        .iter()
        .map(|i| {
            let rec = state.uploads.get(&i.content_id);
            json!({
                "id": i.content_id, "width": i.width, "height": i.height, "date": i.image_date,
                "matte": i.matte_id, "portrait_matte": i.portrait_matte_id,
                "mine": rec.is_some(),
                "source": rec.and_then(|r| r.source.file_name()).map(|n| n.to_string_lossy().to_string()),
                "framing": rec.and_then(|r| r.framing.as_ref()),
                // Reframing re-prepares from the original file, so it has to still be there.
                "reframe": rec.is_some_and(|r| r.source.is_file()),
                // Cached thumbnails load instantly; others arrive later via a `thumbs` event.
                "thumb": find_thumb(&dir, &i.content_id).is_some(),
            })
        })
        .collect();

    // Warm the thumbnail cache in the background so the grid fills in quickly.
    let missing: Vec<String> =
        items.iter().map(|i| i.content_id.clone()).filter(|id| find_thumb(&dir, id).is_none()).collect();
    if !missing.is_empty() {
        let (hub, tv) = (Arc::clone(&hub), Arc::clone(&tv));
        tokio::spawn(async move {
            for chunk in missing.chunks(12) {
                if let Err(e) = fetch_thumbs(&hub, &tv, chunk).await {
                    tracing::warn!("thumbnail prefetch for {}: {e:#}", tv.name);
                    break;
                }
                hub.emit(json!({"type": "thumbs", "tv": tv.name, "ids": chunk}));
            }
        });
    }
    Ok(Json(Value::Array(out)))
}

fn safe_id(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

fn find_thumb(dir: &Path, id: &str) -> Option<PathBuf> {
    let base = safe_id(id);
    ["jpg", "png"].iter().map(|e| dir.join(format!("{base}.{e}"))).find(|p| p.exists())
}

async fn fetch_thumbs(hub: &Shared, tv: &Arc<Tv>, ids: &[String]) -> anyhow::Result<()> {
    let dir = hub.thumb_dir(tv);
    let _guard = tv.xfer.lock().await;
    let ids: Vec<String> = ids.iter().filter(|id| find_thumb(&dir, id).is_none()).cloned().collect();
    if ids.is_empty() {
        return Ok(());
    }
    let thumbs = hub
        .with_art(tv, |a| {
            let ids = ids.clone();
            async move { a.thumbnails(&ids).await }
        })
        .await?;
    std::fs::create_dir_all(&dir)?;
    for t in thumbs {
        let ext = if t.file_type.eq_ignore_ascii_case("png") { "png" } else { "jpg" };
        std::fs::write(dir.join(format!("{}.{ext}", safe_id(&t.content_id))), &t.data)?;
    }
    Ok(())
}

async fn thumb(State(hub): State<Shared>, UrlPath((name, id)): UrlPath<(String, String)>) -> ApiResult<Response> {
    let tv = hub.tv(&name)?;
    let dir = hub.thumb_dir(&tv);
    if find_thumb(&dir, &id).is_none() {
        fetch_thumbs(&hub, &tv, std::slice::from_ref(&id)).await?;
    }
    let path = find_thumb(&dir, &id).ok_or_else(|| status_err(StatusCode::NOT_FOUND, format!("the TV sent no thumbnail for {id}")))?;
    let mime = if path.extension().is_some_and(|e| e == "png") { "image/png" } else { "image/jpeg" };
    Ok((
        [(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, "private, max-age=86400")],
        tokio::fs::read(path).await?,
    )
        .into_response())
}

#[derive(Deserialize)]
struct UploadQuery {
    name: Option<String>,
    show: Option<bool>,
    /// Explicit framing: `cover` (fill the screen) or `contain` (whole photo with a matte).
    fit: Option<String>,
    /// `x,y,w,h` fractions of the photo to keep.
    crop: Option<String>,
    matte: Option<String>,
}

impl UploadQuery {
    fn framing(&self) -> ApiResult<Option<Framing>> {
        let bad = |e: String| status_err(StatusCode::BAD_REQUEST, e);
        let Some(fit) = self.fit.as_deref().filter(|f| !f.is_empty()) else { return Ok(None) };
        Ok(Some(Framing {
            fit: fit.parse().map_err(bad)?,
            crop: self.crop.as_deref().filter(|c| !c.is_empty()).map(str::parse).transpose().map_err(bad)?,
            matte: self.matte.as_deref().and_then(blank_to_none),
        }))
    }
}

/// Anyone may send photos. The body is the raw file; `?name=` is its file name.
async fn upload(
    State(hub): State<Shared>,
    UrlPath(name): UrlPath<String>,
    Query(q): Query<UploadQuery>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let tv = hub.tv(&name)?;
    if body.is_empty() {
        return Err(status_err(StatusCode::BAD_REQUEST, "empty upload"));
    }
    let framing = q.framing()?;
    let file_name = q.name.unwrap_or_else(|| format!("photo-{}.jpg", chrono::Local::now().format("%Y%m%d-%H%M%S")));
    match hub.ingest(&tv, &file_name, &body, q.show.unwrap_or(false), framing).await {
        Ok(v) => Ok(Json(v)),
        Err(e) => {
            hub.emit(json!({"type": "error", "tv": tv.name, "message": format!("{file_name}: {e:#}")}));
            Err(e.into())
        }
    }
}

async fn show(State(hub): State<Shared>, headers: HeaderMap, UrlPath((name, id)): UrlPath<(String, String)>) -> ApiResult<StatusCode> {
    hub.require_admin(&headers)?;
    let tv = hub.tv(&name)?;
    hub.with_art(&tv, |a| {
        let id = id.clone();
        async move { a.select_image(&id, None, true).await }
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct MatteBody {
    matte: String,
    portrait: Option<String>,
}

async fn set_matte(
    State(hub): State<Shared>,
    headers: HeaderMap,
    UrlPath((name, id)): UrlPath<(String, String)>,
    Json(b): Json<MatteBody>,
) -> ApiResult<StatusCode> {
    hub.require_admin(&headers)?;
    let tv = hub.tv(&name)?;
    hub.with_art(&tv, |a| {
        let (id, matte, portrait) = (id.clone(), b.matte.clone(), b.portrait.clone());
        async move { a.change_matte(&id, &matte, portrait.as_deref()).await }
    })
    .await?;
    hub.emit(json!({"type": "changed", "tv": tv.name}));
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct OnBody {
    on: bool,
}

async fn favorite(
    State(hub): State<Shared>,
    headers: HeaderMap,
    UrlPath((name, id)): UrlPath<(String, String)>,
    Json(b): Json<OnBody>,
) -> ApiResult<StatusCode> {
    hub.require_admin(&headers)?;
    let tv = hub.tv(&name)?;
    hub.with_art(&tv, |a| {
        let id = id.clone();
        async move { a.set_favorite(&id, b.on).await }
    })
    .await?;
    hub.emit(json!({"type": "changed", "tv": tv.name}));
    Ok(StatusCode::NO_CONTENT)
}

/// The original photo behind one of our uploads, as a JPEG preview for the framing editor.
async fn original(State(hub): State<Shared>, headers: HeaderMap, UrlPath((name, id)): UrlPath<(String, String)>) -> ApiResult<Response> {
    hub.require_admin(&headers)?;
    let tv = hub.tv(&name)?;
    let state = TvState::load(&hub.state_file(&tv))?;
    let rec = state
        .uploads
        .get(&id)
        .ok_or_else(|| status_err(StatusCode::NOT_FOUND, "only photos sent from here can be reframed"))?;
    let bytes = tokio::fs::read(&rec.source)
        .await
        .map_err(|e| status_err(StatusCode::NOT_FOUND, format!("the original ({}) is gone: {e}", rec.source.display())))?;
    let jpeg = tokio::task::spawn_blocking(move || image_prep::preview(&bytes, 2048)).await??;
    Ok(([(header::CONTENT_TYPE, "image/jpeg"), (header::CACHE_CONTROL, "private, max-age=3600")], jpeg).into_response())
}

/// Re-prepare one of our uploads from its original with new framing, replacing it on the TV.
async fn reframe(
    State(hub): State<Shared>,
    headers: HeaderMap,
    UrlPath((name, id)): UrlPath<(String, String)>,
    Json(framing): Json<Framing>,
) -> ApiResult<Json<Value>> {
    hub.require_admin(&headers)?;
    let tv = hub.tv(&name)?;
    let _guard = tv.xfer.lock().await;
    let state_file = hub.state_file(&tv);
    let mut state = TvState::load(&state_file)?;
    let rec = state
        .uploads
        .get(&id)
        .cloned()
        .ok_or_else(|| status_err(StatusCode::NOT_FOUND, "only photos sent from here can be reframed"))?;
    if !rec.source.is_file() {
        return Err(status_err(StatusCode::NOT_FOUND, format!("the original ({}) is gone", rec.source.display())));
    }
    let was_current = hub
        .with_art(&tv, |a| async move { a.current_artwork().await })
        .await
        .is_ok_and(|c| c.content_id == id);
    let mut settings = upload_settings(&tv.config());
    framing.apply(&mut settings);
    let result = hub.upload_with_retry(&tv, &rec.source, &settings).await?;
    state.uploads.remove(&id);
    state.record_framed(&result.content_id, &rec.hash, &rec.source, Some(framing));
    state.save(&state_file)?;
    if was_current {
        let new_id = result.content_id.clone();
        hub.with_art(&tv, |a| {
            let id = new_id.clone();
            async move { a.select_image(&id, None, true).await }
        })
        .await?;
    }
    if let Err(e) = hub.delete_from_tv(&tv, vec![id.clone()]).await {
        tracing::warn!("removing the old version of {id} from {}: {e:#}", tv.name);
    }
    hub.emit(json!({"type": "changed", "tv": tv.name}));
    Ok(Json(json!({"content_id": result.content_id, "replaced": id, "matte": result.matte})))
}

/// Delete from the TV. If the photo came from the TV's sync folder, its file is moved to
/// `.removed/` there, so the next sync doesn't put it back (and it can be recovered).
async fn delete_photo(
    State(hub): State<Shared>,
    headers: HeaderMap,
    UrlPath((name, id)): UrlPath<(String, String)>,
) -> ApiResult<Json<Value>> {
    hub.require_admin(&headers)?;
    let tv = hub.tv(&name)?;
    hub.delete_from_tv(&tv, vec![id.clone()]).await?;
    let state_file = hub.state_file(&tv);
    let mut state = TvState::load(&state_file)?;
    let mut moved = None;
    if let Some(rec) = state.uploads.remove(&id) {
        if let Some(folder) = tv.config().folder
            && rec.source.starts_with(&folder)
            && rec.source.exists()
            && let Some(file) = rec.source.file_name()
        {
            let bin = folder.join(".removed");
            std::fs::create_dir_all(&bin)?;
            std::fs::rename(&rec.source, bin.join(file))?;
            moved = Some(bin.join(file));
        }
        state.save(&state_file)?;
    }
    hub.emit(json!({"type": "changed", "tv": tv.name}));
    Ok(Json(json!({"deleted": id, "moved_source_to": moved})))
}

async fn mattes(State(hub): State<Shared>, UrlPath(name): UrlPath<String>) -> ApiResult<Json<Value>> {
    let tv = hub.tv(&name)?;
    let m = hub.with_art(&tv, |a| async move { a.matte_list().await }).await?;
    Ok(Json(serde_json::to_value(m)?))
}

async fn artmode(State(hub): State<Shared>, headers: HeaderMap, UrlPath(name): UrlPath<String>, Json(b): Json<OnBody>) -> ApiResult<StatusCode> {
    hub.require_admin(&headers)?;
    let tv = hub.tv(&name)?;
    hub.with_art(&tv, |a| async move { a.set_artmode(b.on).await }).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct SlideshowBody {
    /// None or 0 turns the slideshow off.
    minutes: Option<u32>,
    #[serde(default = "yes")]
    shuffle: bool,
}

fn yes() -> bool {
    true
}

async fn slideshow(
    State(hub): State<Shared>,
    headers: HeaderMap,
    UrlPath(name): UrlPath<String>,
    Json(b): Json<SlideshowBody>,
) -> ApiResult<StatusCode> {
    hub.require_admin(&headers)?;
    let tv = hub.tv(&name)?;
    let minutes = b.minutes.filter(|m| *m > 0);
    hub.with_art(&tv, |a| async move { a.set_slideshow(minutes, b.shuffle, category::MY_PHOTOS).await }).await?;
    hub.emit(json!({"type": "changed", "tv": tv.name}));
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Default)]
struct SyncBody {
    #[serde(default)]
    delete: bool,
}

/// Start syncing the TV's folder; progress arrives as events.
async fn sync_folder(
    State(hub): State<Shared>,
    headers: HeaderMap,
    UrlPath(name): UrlPath<String>,
    body: Option<Json<SyncBody>>,
) -> ApiResult<StatusCode> {
    hub.require_admin(&headers)?;
    let tv = hub.tv(&name)?;
    let delete = body.map(|Json(b)| b.delete).unwrap_or(false);
    let folder = tv.config().folder.ok_or_else(|| status_err(StatusCode::BAD_REQUEST, format!("no folder configured for {name}")))?;
    let hub2 = Arc::clone(&hub);
    tokio::spawn(async move {
        let result: anyhow::Result<()> = async {
            let f = folder.clone();
            let local = tokio::task::spawn_blocking(move || sync::scan_folder(&f)).await??;
            let _guard = tv.xfer.lock().await;
            let art = hub2.art(&tv).await?;
            let on_tv: HashSet<String> =
                art.content_list(Some(category::MY_PHOTOS)).await?.into_iter().map(|i| i.content_id).collect();
            let state_file = hub2.state_file(&tv);
            let mut state = TvState::load(&state_file)?;
            let plan = sync::plan(&local, &state, &on_tv, delete);
            hub2.emit(json!({"type": "sync-start", "tv": tv.name, "upload": plan.upload.len(), "delete": plan.delete.len()}));
            let settings = upload_settings(&tv.config());
            let name = tv.name.clone();
            let outcome = sync::execute(&art, &mut state, &state_file, plan, &settings, |ev| match ev {
                sync::SyncEvent::Uploading { index, total, path } => hub2.emit(json!({
                    "type": "sync-progress", "tv": name, "index": index, "total": total,
                    "name": path.file_name().map(|n| n.to_string_lossy().to_string()),
                })),
                sync::SyncEvent::UploadFailed { path, error } => hub2.emit(json!({
                    "type": "error", "tv": name, "message": format!("{}: {error}", path.display()),
                })),
                _ => {}
            })
            .await?;
            hub2.emit(json!({
                "type": "sync-done", "tv": tv.name, "uploaded": outcome.uploaded.len(),
                "deleted": outcome.deleted, "failed": outcome.failed,
            }));
            Ok(())
        }
        .await;
        if let Err(e) = result {
            hub2.emit(json!({"type": "error", "tv": tv.name, "message": format!("sync: {e:#}")}));
        }
        hub2.emit(json!({"type": "changed", "tv": tv.name}));
    });
    Ok(StatusCode::ACCEPTED)
}

async fn wake(State(hub): State<Shared>, headers: HeaderMap, UrlPath(name): UrlPath<String>) -> ApiResult<StatusCode> {
    hub.require_admin(&headers)?;
    let tv = hub.tv(&name)?;
    let cfg = tv.config();
    let mac = cfg.mac.ok_or_else(|| status_err(StatusCode::BAD_REQUEST, format!("no MAC address known for {name}")))?;
    for _ in 0..3 {
        wol::wake(&mac, Some(&cfg.host))?;
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
struct ShareCardBody {
    url: String,
}

/// Put a "scan me" QR code on the TV that opens this TV's guest upload page.
async fn share_card(
    State(hub): State<Shared>,
    headers: HeaderMap,
    UrlPath(name): UrlPath<String>,
    Json(b): Json<ShareCardBody>,
) -> ApiResult<Json<Value>> {
    hub.require_admin(&headers)?;
    let tv = hub.tv(&name)?;
    let (w, h) = tv.config().panel_size();
    let jpeg = render_share_card(&b.url, w, h)?;
    let id = {
        let _guard = tv.xfer.lock().await;
        let opts = UploadOptions { matte: "none".into(), portrait_matte: "none".into(), ..Default::default() };
        hub.with_art(&tv, |a| {
            let (jpeg, opts) = (jpeg.clone(), opts.clone());
            async move { a.upload(&jpeg, &opts).await }
        })
        .await?
    };
    let shown = id.clone();
    hub.with_art(&tv, |a| {
        let id = shown.clone();
        async move { a.select_image(&id, None, true).await }
    })
    .await?;
    hub.emit(json!({"type": "changed", "tv": tv.name}));
    Ok(Json(json!({"content_id": id})))
}

/// A full-screen image with a large QR code on a white card, for the TV.
fn render_share_card(text: &str, w: u32, h: u32) -> anyhow::Result<Vec<u8>> {
    use image::{Rgb, RgbImage};
    let code = qrcode::QrCode::new(text.as_bytes()).map_err(|e| anyhow!("QR code: {e:?}"))?;
    let n = code.width() as u32;
    let quiet = 4;
    let total = n + 2 * quiet;
    let module = (h.min(w) * 58 / 100 / total).max(1);
    let side = module * total;
    let (x0, y0) = (w.saturating_sub(side) / 2, h.saturating_sub(side) / 2);
    let mut img = RgbImage::from_pixel(w, h, Rgb([24, 26, 31]));
    let mut fill = |x: u32, y: u32, cw: u32, ch: u32, c: Rgb<u8>| {
        for yy in y..(y + ch).min(h) {
            for xx in x..(x + cw).min(w) {
                img.put_pixel(xx, yy, c);
            }
        }
    };
    let border = module * 2;
    fill(x0.saturating_sub(border), y0.saturating_sub(border), side + 2 * border, side + 2 * border, Rgb([201, 169, 110]));
    fill(x0, y0, side, side, Rgb([255, 255, 255]));
    for (i, c) in code.to_colors().iter().enumerate() {
        if *c == qrcode::Color::Dark {
            let (cx, cy) = (i as u32 % n, i as u32 / n);
            fill(x0 + (quiet + cx) * module, y0 + (quiet + cy) * module, module, module, Rgb([0, 0, 0]));
        }
    }
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92).encode_image(&img)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_unique_dedupes_and_renames() {
        let dir = std::env::temp_dir().join(format!("frametv-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let hash = |d: &[u8]| hex::encode(Sha256::digest(d));
        let a = save_unique(&dir, "x.jpg", b"one", &hash(b"one")).unwrap();
        let again = save_unique(&dir, "x.jpg", b"one", &hash(b"one")).unwrap();
        let b = save_unique(&dir, "x.jpg", b"two", &hash(b"two")).unwrap();
        assert_eq!(a, again);
        assert_eq!(b.file_name().unwrap(), "x (2).jpg");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn passwords_are_salted_and_checked() {
        let a = hash_password("hunter2");
        assert_ne!(a, hash_password("hunter2"), "salted");
        assert!(check_password(&a, "hunter2"));
        assert!(!check_password(&a, "hunter3"));
        assert!(!check_password("garbage", "hunter2"));
    }

    #[test]
    fn upload_query_framing() {
        let q = UploadQuery {
            name: None,
            show: None,
            fit: Some("cover".into()),
            crop: Some("0.1,0,0.8,1".into()),
            matte: Some("".into()),
        };
        let f = q.framing().ok().flatten().unwrap();
        assert_eq!(f.fit, image_prep::Fit::Cover);
        assert_eq!(f.crop.unwrap().w, 0.8);
        assert_eq!(f.matte, None);
        let q = UploadQuery { fit: None, crop: None, matte: None, name: None, show: None };
        assert!(q.framing().ok().flatten().is_none());
    }

    #[test]
    fn share_card_renders() {
        let jpeg = render_share_card("http://192.168.0.5:8780/tv/Living%20Room", 1920, 1080).unwrap();
        let img = image::load_from_memory(&jpeg).unwrap();
        assert_eq!((img.width(), img.height()), (1920, 1080));
    }
}
