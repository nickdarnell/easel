//! `frametv serve`: a web app for browsing and managing what's on each TV, where guests can
//! send photos to a TV from any browser, plus optional LocalSend targets (one per TV).
//!
//! Guests (anyone who can reach the page) may view the TVs' photos and send new ones. If an
//! admin password is set (in the web app's settings), everything else (deleting, mattes,
//! slideshow, pairing, ...) needs it; without one, everyone on the network is an admin.
//!
//! All settings live in the config file, so a container needs nothing but a volume for it.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path as UrlPath, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, broadcast, mpsc};

use samsung_frame::art::{ArtClient, ArtItem, MatteList, UploadOptions};
use samsung_frame::config::{Config, TvConfig};
use samsung_frame::protocol::category;
use samsung_frame::sync::{self, Framing, TvState};
use samsung_frame::{discovery, image_prep, localsend, pairing, rest, wol};

use crate::api::{self, ServerEvent};
use crate::cache::{self, Snapshot};

// The web UI (`web/`), embedded by build.rs.
include!(concat!(env!("OUT_DIR"), "/web_files.rs"));
const MAX_UPLOAD: usize = 512 * 1024 * 1024;
/// A photo list this recent is served without asking the TV again.
const PHOTOS_FRESH: Duration = Duration::from_secs(10);
/// After failing to connect to a TV, requests fail straight away for this long instead of
/// each waiting for the TV again (waking it clears this).
const RETRY_CONNECT_AFTER: Duration = Duration::from_secs(10);
/// After the TV fails to send a photo's thumbnail, it isn't asked for that one again for this long.
const RETRY_THUMB_AFTER: Duration = Duration::from_secs(10 * 60);
/// A QR code card left on the TV is taken down after this long.
const SHARE_CARD_FOR: Duration = Duration::from_secs(30 * 60);

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
    /// When connecting last failed, and why; cleared once a connection works.
    down: StdMutex<Option<(Instant, String)>>,
    /// The photo lists the TV last sent, by category (loaded from disk on first use).
    photos: StdMutex<HashMap<&'static str, Snapshot<Vec<ArtItem>>>>,
    /// Categories whose list is being refreshed in the background.
    refreshing: StdMutex<HashSet<&'static str>>,
    /// Photos the TV recently sent no thumbnail for, and when it last failed.
    no_thumb: StdMutex<HashMap<String, Instant>>,
}

impl Tv {
    fn new(cfg: TvConfig) -> Arc<Self> {
        Arc::new(Tv {
            name: cfg.name.clone(),
            cfg: StdMutex::new(cfg),
            art: Mutex::new(None),
            xfer: Mutex::new(()),
            down: StdMutex::new(None),
            photos: StdMutex::new(HashMap::new()),
            refreshing: StdMutex::new(HashSet::new()),
            no_thumb: StdMutex::new(HashMap::new()),
        })
    }

    fn config(&self) -> TvConfig {
        self.cfg.lock().unwrap().clone()
    }

    /// Why the TV isn't reachable, if connecting to it failed recently.
    fn down_reason(&self) -> Option<String> {
        let down = self.down.lock().unwrap();
        down.as_ref().filter(|(at, _)| at.elapsed() < RETRY_CONNECT_AFTER).map(|(_, why)| why.clone())
    }

    /// The cached photo lists no longer match the TV: the next listing asks it.
    fn photos_changed(&self) {
        for snap in self.photos.lock().unwrap().values_mut() {
            snap.stale = true;
        }
    }
}

/// A QR code card on the TV, and what to put back when sharing is done.
#[derive(Clone, Serialize, Deserialize)]
struct ShareSession {
    content_id: String,
    /// What the TV was showing before.
    previous: Option<String>,
    /// The slideshow that was running before (paused so it doesn't turn away from the card).
    slideshow: Option<PausedSlideshow>,
}

#[derive(Clone, Serialize, Deserialize)]
struct PausedSlideshow {
    minutes: u32,
    shuffle: bool,
    category: String,
}

struct Hub {
    cfg: StdMutex<Config>,
    cfg_path: PathBuf,
    state_dir: PathBuf,
    data_dir: PathBuf,
    tvs: RwLock<Vec<Arc<Tv>>>,
    events: broadcast::Sender<ServerEvent>,
    /// Advertised LocalSend targets.
    localsend: StdMutex<Vec<api::LocalSendTarget>>,
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
        (self.0, Json(api::ErrorBody { error: msg })).into_response()
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

/// A connection that stops answering (the TV dozed off, Wi-Fi dropped) often still looks open.
fn is_unanswered(e: &samsung_frame::Error) -> bool {
    is_disconnect(e) || matches!(e, samsung_frame::Error::Timeout(..))
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

    fn emit(&self, event: ServerEvent) {
        let _ = self.events.send(event);
    }

    /// The TV's photos changed: forget the cached lists and tell the pages.
    fn changed(&self, tv: &Tv) {
        tv.photos_changed();
        self.emit(ServerEvent::Changed { tv: tv.name.clone() });
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

    fn cache_file(&self, tv: &Tv, what: &str) -> PathBuf {
        sync::state_path(&self.data_dir.join("cache"), &tv.name).with_extension("").join(format!("{what}.json"))
    }

    /// The QR code card that's up on the TV, if there is one (kept on disk, so it can still be
    /// taken down after a restart).
    fn share_session(&self, tv: &Tv) -> Option<ShareSession> {
        cache::load(&self.cache_file(tv, "share-card")).map(|s| s.value)
    }

    fn set_share_session(&self, tv: &Tv, session: Option<&ShareSession>) {
        let file = self.cache_file(tv, "share-card");
        match session {
            Some(s) => cache::save(&file, &Snapshot::now(s.clone())),
            None => {
                let _ = std::fs::remove_file(file);
            }
        }
    }

    /// Take the QR code card down: resume the slideshow, put back what was showing (unless
    /// someone has since chosen something else), and delete the card. `false` if there was none.
    async fn end_share(self: &Arc<Self>, tv: &Arc<Tv>) -> anyhow::Result<bool> {
        let Some(session) = self.share_session(tv) else {
            return Ok(false);
        };
        let card = session.content_id.clone();
        let showing = self.read_art(tv, |a| async move { a.current_artwork().await }).await?.content_id;
        if let Some(ss) = &session.slideshow {
            let ss = ss.clone();
            self.with_art(tv, |a| {
                let ss = ss.clone();
                async move { a.set_slideshow(Some(ss.minutes), ss.shuffle, &ss.category).await }
            })
            .await?;
        }
        if showing == card {
            let back = session.previous.filter(|p| *p != card);
            if let Some(id) = back {
                let shown = self
                    .with_art(tv, |a| {
                        let id = id.clone();
                        async move { a.select_image(&id, None, true).await }
                    })
                    .await;
                if let Err(e) = shown {
                    tracing::warn!("{}: putting {id} back after sharing: {e:#}", tv.name);
                }
            }
        }
        if let Err(e) = self.delete_from_tv(tv, vec![card.clone()]).await {
            tracing::warn!("{}: deleting the QR code card {card}: {e:#}", tv.name);
        }
        self.set_share_session(tv, None);
        self.changed(tv);
        Ok(true)
    }

    /// The art connection, reconnecting if the TV dropped it.
    async fn art(self: &Arc<Self>, tv: &Arc<Tv>) -> anyhow::Result<Arc<ArtClient>> {
        let mut slot = tv.art.lock().await;
        if let Some(a) = slot.as_ref()
            && a.is_connected()
        {
            return Ok(Arc::clone(a));
        }
        // It just failed: don't make every request wait for it again.
        if let Some(why) = tv.down_reason() {
            return Err(anyhow!(why));
        }
        let cfg = tv.config();
        let client_name = self.cfg.lock().unwrap().client_name.clone();
        let endpoint = cfg.endpoint(&client_name);
        let connected = match tokio::time::timeout(Duration::from_secs(15), ArtClient::connect(&endpoint)).await {
            Ok(Ok(art)) => Ok(art),
            Ok(Err(e)) => Err(anyhow::Error::new(e).context(format!("connecting to {}", tv.name))),
            Err(_) => Err(anyhow!("{} did not answer (is it on?)", tv.name)),
        };
        let art = match connected {
            Ok(art) => {
                tv.down.lock().unwrap().take();
                art
            }
            Err(e) => {
                *tv.down.lock().unwrap() = Some((Instant::now(), format!("{e:#}")));
                return Err(e);
            }
        };
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
                    Ok(m) => {
                        // Photos added, removed or changed by anyone, including the phone app.
                        let ev = m.event.as_str();
                        let photos = ["added", "deleted", "favorite", "matte"].iter().any(|w| ev.contains(w));
                        if photos && let Ok(tv) = hub.tv(&name) {
                            hub.changed(&tv);
                        }
                        hub.emit(ServerEvent::TvEvent { tv: name.clone(), event: m.event, data: m.data });
                    }
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
        Fut: Future<Output = samsung_frame::Result<T>>,
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

    /// Like [`Self::with_art`], but also retries when the TV doesn't answer at all, which is
    /// only safe for requests that change nothing.
    async fn read_art<T, F, Fut>(self: &Arc<Self>, tv: &Arc<Tv>, f: F) -> anyhow::Result<T>
    where
        F: Fn(Arc<ArtClient>) -> Fut,
        Fut: Future<Output = samsung_frame::Result<T>>,
    {
        let art = self.art(tv).await?;
        match f(art).await {
            Err(e) if is_unanswered(&e) => {
                tracing::info!("{}: {e}; retrying on a new connection", tv.name);
                tv.art.lock().await.take();
                let art = self.art(tv).await?;
                Ok(f(art).await?)
            }
            other => Ok(other?),
        }
    }

    /// The TV's photos in `cat`: from the cache if it's recent, otherwise from the TV, falling
    /// back to the cache when the TV doesn't answer. Also returns, if the TV couldn't be
    /// reached, why.
    async fn photo_list(
        self: &Arc<Self>,
        tv: &Arc<Tv>,
        cat: &'static str,
    ) -> anyhow::Result<(Snapshot<Vec<ArtItem>>, Option<String>)> {
        let cached = {
            let mut lists = tv.photos.lock().unwrap();
            if !lists.contains_key(cat)
                && let Some(snap) = cache::load(&self.cache_file(tv, &format!("photos-{cat}")))
            {
                lists.insert(cat, snap);
            }
            lists.get(cat).cloned()
        };
        // Nothing known to have changed: answer now, and check with the TV in the background.
        if let Some(snap) = cached.clone().filter(|s| !s.stale) {
            if !snap.is_fresh(PHOTOS_FRESH) {
                self.refresh_photos(tv, cat);
            }
            return Ok((snap, tv.down_reason()));
        }
        match self.fetch_photos(tv, cat).await {
            Ok(snap) => Ok((snap, None)),
            Err(e) => match cached {
                Some(snap) => Ok((snap, Some(format!("{e:#}")))),
                None => Err(e),
            },
        }
    }

    /// Ask the TV for its photos in `cat`, and cache them.
    async fn fetch_photos(self: &Arc<Self>, tv: &Arc<Tv>, cat: &'static str) -> anyhow::Result<Snapshot<Vec<ArtItem>>> {
        let items = self.read_art(tv, |a| async move { a.content_list(Some(cat)).await }).await?;
        let snap = Snapshot::now(items);
        cache::save(&self.cache_file(tv, &format!("photos-{cat}")), &snap);
        tv.photos.lock().unwrap().insert(cat, snap.clone());
        Ok(snap)
    }

    /// Update the cached list in the background. Tells the pages if it changed, or if the TV
    /// just stopped answering.
    fn refresh_photos(self: &Arc<Self>, tv: &Arc<Tv>, cat: &'static str) {
        if !tv.refreshing.lock().unwrap().insert(cat) {
            return;
        }
        let (hub, tv) = (Arc::clone(self), Arc::clone(tv));
        tokio::spawn(async move {
            let json = |s: &Snapshot<Vec<ArtItem>>| serde_json::to_string(&s.value).unwrap_or_default();
            let before = tv.photos.lock().unwrap().get(cat).map(json);
            let was_down = tv.down_reason().is_some();
            match hub.fetch_photos(&tv, cat).await {
                Ok(snap) if before.as_deref() != Some(json(&snap).as_str()) => hub.changed(&tv),
                Ok(_) => {}
                Err(e) => {
                    tracing::info!("refreshing {}'s photos: {e:#}", tv.name);
                    if !was_down {
                        hub.emit(ServerEvent::Changed { tv: tv.name.clone() });
                    }
                }
            }
            tv.refreshing.lock().unwrap().remove(cat);
        });
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
    ) -> anyhow::Result<api::Uploaded> {
        let cfg = tv.config();
        let dir = cfg.folder.clone().unwrap_or_else(|| self.data_dir.join("photos").join(&tv.name));
        let hash = hex::encode(Sha256::digest(data));
        let path = save_unique(&dir, &localsend::sanitize_file_name(file_name), data, &hash)?;

        let _guard = tv.xfer.lock().await;
        let state_file = self.state_file(tv);
        let mut state = TvState::load(&state_file)?;
        // Only ask the TV for its photos if this exact file was sent before.
        let existing = match state.content_for_hash(&hash).map(str::to_owned) {
            Some(id) => {
                let on_tv =
                    self.read_art(tv, |a| async move { a.content_list(Some(category::MY_PHOTOS)).await }).await?;
                on_tv.iter().any(|i| i.content_id == id).then_some(id)
            }
            None => None,
        };
        // Same photo, and either no explicit framing asked for or the same one as before.
        let same = existing.as_ref().filter(|id| {
            framing.is_none() || state.uploads.get(*id).and_then(|r| r.framing.as_ref()) == framing.as_ref()
        });
        let (content_id, duplicate) = if let Some(id) = same {
            (id.clone(), true)
        } else {
            self.emit(ServerEvent::Uploading { tv: tv.name.clone(), name: file_name.to_owned() });
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
        };
        if show {
            let id = content_id.clone();
            self.with_art(tv, |a| {
                let id = id.clone();
                async move { a.select_image(&id, None, true).await }
            })
            .await?;
        }
        let photo = api::Uploaded { content_id, name: file_name.to_owned(), duplicate, shown: show };
        tv.photos_changed();
        self.emit(ServerEvent::Uploaded { tv: tv.name.clone(), photo: photo.clone() });
        Ok(photo)
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
        self.emit(ServerEvent::SettingsChanged);
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
    let data_dir = state_dir.parent().map_or_else(|| state_dir.clone(), Path::to_path_buf);
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
        .route("/assets/{*path}", get(asset))
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
        .route("/api/tvs/{tv}/photos/delete", post(delete_many))
        .route("/api/tvs/{tv}/photos/{id}", axum::routing::delete(delete_photo))
        .route("/api/tvs/{tv}/mattes", get(mattes))
        .route("/api/tvs/{tv}/artmode", post(artmode))
        .route("/api/tvs/{tv}/slideshow", post(slideshow))
        .route("/api/tvs/{tv}/sync", post(sync_folder))
        .route("/api/tvs/{tv}/wake", post(wake))
        .route("/api/tvs/{tv}/share-card", post(share_card).delete(end_share))
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
        println!("LocalSend target:  \"{}\" (port {})", d.alias, d.port);
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
            tokio::select! { () = ctrl_c => {}, () = term => {} }
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
    *hub.localsend.lock().unwrap() = devices
        .iter()
        .zip(&receiver.ports)
        .map(|(d, p)| api::LocalSendTarget { alias: d.alias.clone(), port: *p })
        .collect();

    let hub = Arc::clone(hub);
    tokio::spawn(async move {
        while let Some(file) = rx.recv().await {
            let Some((_, tvs)) = targets.get(file.device) else { continue };
            let show = hub.server_settings().show_on_receive;
            tracing::info!("received {} from {} via LocalSend", file.file_name, file.sender_alias);
            for tv in tvs {
                let (hub, tv, file_name, data) =
                    (Arc::clone(&hub), Arc::clone(tv), file.file_name.clone(), file.data.clone());
                let show = show && file.last_in_session;
                let from = file.sender_alias.clone();
                // TVs work in parallel; each TV's transfers are serialized by its own lock.
                tokio::spawn(async move {
                    if let Err(e) = hub.ingest(&tv, &file_name, &data, show, None).await {
                        hub.emit(ServerEvent::Error {
                            tv: tv.name.clone(),
                            message: format!("{file_name} from {from}: {e:#}"),
                        });
                    }
                });
            }
        }
    });
    Ok(receiver)
}

fn web_file(path: &str) -> Option<&'static [u8]> {
    WEB_FILES.iter().find(|(name, _)| *name == path).map(|(_, data)| *data)
}

async fn index() -> impl IntoResponse {
    let page = web_file("index.html").unwrap_or_default();
    ([(header::CACHE_CONTROL, "no-cache")], Html(page))
}

/// Built scripts and styles. Their names carry a content hash, so they can be cached for good.
async fn asset(UrlPath(path): UrlPath<String>) -> Response {
    let Some(data) = web_file(&format!("assets/{path}")) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let kind = match path.rsplit_once('.').map_or("", |(_, ext)| ext) {
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "woff2" => "font/woff2",
        "map" | "json" => "application/json",
        _ => "application/octet-stream",
    };
    ([(header::CONTENT_TYPE, kind), (header::CACHE_CONTROL, "public, max-age=31536000, immutable")], data)
        .into_response()
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

async fn info(State(hub): State<Shared>, headers: HeaderMap) -> Json<api::Info> {
    let settings = hub.server_settings();
    Json(api::Info {
        admin: hub.is_admin(&headers),
        password_set: settings.admin_password.is_some(),
        public_url: settings.public_url,
        localsend: hub.localsend.lock().unwrap().clone(),
        heif: image_prep::heif_supported(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    })
}

fn settings(hub: &Hub) -> api::Settings {
    let s = hub.server_settings();
    api::Settings {
        password_set: s.admin_password.is_some(),
        public_url: s.public_url,
        localsend: s.localsend,
        localsend_pin: s.localsend_pin,
        show_on_receive: s.show_on_receive,
        localsend_targets: hub.localsend.lock().unwrap().clone(),
        localsend_error: hub.localsend_error.lock().unwrap().clone(),
        in_docker: Path::new("/.dockerenv").exists(),
    }
}

async fn get_settings(State(hub): State<Shared>, headers: HeaderMap) -> ApiResult<Json<api::Settings>> {
    hub.require_admin(&headers)?;
    Ok(Json(settings(&hub)))
}

async fn put_settings(
    State(hub): State<Shared>,
    headers: HeaderMap,
    Json(b): Json<api::SettingsUpdate>,
) -> ApiResult<Json<api::Settings>> {
    hub.require_admin(&headers)?;
    let public_url = match b.public_url.as_deref().map(str::trim) {
        Some("") => Some(None),
        Some(u) if u.starts_with("http://") || u.starts_with("https://") => {
            Some(Some(u.trim_end_matches('/').to_string()))
        }
        Some(u) => {
            return Err(status_err(StatusCode::BAD_REQUEST, format!("`{u}` should start with http:// or https://")));
        }
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
    if (before.localsend, &before.localsend_pin) == (after.localsend, &after.localsend_pin) {
        hub.emit(ServerEvent::SettingsChanged);
    } else {
        hub.restart_localsend().await;
    }
    Ok(Json(settings(&hub)))
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
                Ok(ev) => {
                    let json = serde_json::to_string(&ev).unwrap_or_default();
                    return Some((Ok(Event::default().data(json)), rx));
                }
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
    let code =
        qrcode::QrCode::new(q.text.as_bytes()).map_err(|e| status_err(StatusCode::BAD_REQUEST, format!("{e:?}")))?;
    let svg = code
        .render::<svg::Color>()
        .min_dimensions(256, 256)
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .build();
    Ok(([(header::CONTENT_TYPE, "image/svg+xml"), (header::CACHE_CONTROL, "max-age=3600")], svg).into_response())
}

async fn discover(State(hub): State<Shared>, headers: HeaderMap) -> ApiResult<Json<Vec<api::Discovered>>> {
    hub.require_admin(&headers)?;
    let (ssdp, scan) = tokio::join!(discovery::ssdp(Duration::from_secs(2)), discovery::scan_subnet());
    let known: HashSet<String> = hub.all_tvs().iter().map(|t| t.config().host).collect();
    let mut seen = HashSet::new();
    let found = ssdp
        .unwrap_or_default()
        .into_iter()
        .chain(scan.unwrap_or_default())
        .filter(|d| seen.insert(d.ip.to_string()))
        .map(|d| {
            let host = d.ip.to_string();
            api::Discovered {
                name: pairing::friendly_name(&d.info.display_name()),
                frame: d.info.is_frame(),
                power: api::Power::from_tv(d.info.power_state()),
                paired: known.contains(&host),
                model: d.info.device.model_name,
                host,
            }
        })
        .collect();
    Ok(Json(found))
}

async fn list_tvs(State(hub): State<Shared>) -> Json<Vec<api::Tv>> {
    let checks = hub.all_tvs().into_iter().map(|tv| async move {
        let cfg = tv.config();
        let power = match rest::device_info(&cfg.host, Duration::from_millis(1500)).await {
            Ok(i) => api::Power::from_tv(i.power_state()),
            Err(_) => api::Power::Off,
        };
        api::Tv {
            resolution: cfg.panel_size(),
            size_inches: cfg.size_inches(),
            folder: cfg.folder.as_ref().map(|f| f.display().to_string()),
            fit: cfg.fit.as_deref().and_then(|f| f.parse().ok()),
            can_wake: cfg.mac.is_some(),
            power,
            name: cfg.name,
            host: cfg.host,
            model: cfg.model,
            matte: cfg.matte,
            portrait_matte: cfg.portrait_matte,
        }
    });
    Json(futures_util::future::join_all(checks).await)
}

/// Pair a TV. Takes up to a minute: someone has to press Allow on the TV.
async fn add_tv(
    State(hub): State<Shared>,
    headers: HeaderMap,
    Json(b): Json<api::AddTv>,
) -> ApiResult<Json<api::Paired>> {
    hub.require_admin(&headers)?;
    let host = b.host.trim();
    if host.is_empty() {
        return Err(status_err(StatusCode::BAD_REQUEST, "enter the TV's IP address"));
    }
    let name = b.name.as_deref().map(str::trim).filter(|n| !n.is_empty());
    let cfg = hub.cfg.lock().unwrap().clone();
    let port = cfg.find(host).map_or(samsung_frame::protocol::SECURE_PORT, |t| t.port);
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
    hub.emit(ServerEvent::TvsChanged);
    let h = Arc::clone(&hub);
    tokio::spawn(async move { h.restart_localsend().await });
    Ok(Json(api::Paired {
        name: tv_cfg.name,
        api_version: paired.api_version,
        photos: paired.photo_count.map(|n| n as u32),
        frame: paired.is_frame,
    }))
}

async fn update_tv(
    State(hub): State<Shared>,
    headers: HeaderMap,
    UrlPath(name): UrlPath<String>,
    Json(b): Json<api::TvUpdate>,
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
    hub.emit(ServerEvent::TvsChanged);
    Ok(StatusCode::NO_CONTENT)
}

async fn forget_tv(
    State(hub): State<Shared>,
    headers: HeaderMap,
    UrlPath(name): UrlPath<String>,
) -> ApiResult<StatusCode> {
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
    hub.emit(ServerEvent::TvsChanged);
    let h = Arc::clone(&hub);
    tokio::spawn(async move { h.restart_localsend().await });
    Ok(StatusCode::NO_CONTENT)
}

async fn tv_status(State(hub): State<Shared>, UrlPath(name): UrlPath<String>) -> ApiResult<Json<api::Status>> {
    let tv = hub.tv(&name)?;
    let art = hub.art(&tv).await?;
    let (artmode, current, slideshow) = tokio::join!(art.artmode(), art.current_artwork(), art.slideshow_status());
    Ok(Json(api::Status {
        artmode: artmode.ok(),
        current: current.ok().map(|c| api::Current { content_id: c.content_id }),
        slideshow: slideshow.ok().map(|s| api::Slideshow {
            minutes: s.minutes,
            shuffle: s.shuffle,
            favorites: s.category_id.as_deref() == Some(category::FAVORITES),
        }),
        sharing: hub.share_session(&tv).is_some(),
    }))
}

#[derive(Deserialize)]
struct PhotosQuery {
    category: Option<String>,
}

async fn photos(
    State(hub): State<Shared>,
    UrlPath(name): UrlPath<String>,
    Query(q): Query<PhotosQuery>,
) -> ApiResult<Json<api::PhotoList>> {
    let tv = hub.tv(&name)?;
    let cat = match q.category.as_deref() {
        Some("favorites") => category::FAVORITES,
        _ => category::MY_PHOTOS,
    };
    let (snap, offline) = hub.photo_list(&tv, cat).await?;
    let mut items = snap.value;
    // The QR code card is only there while sharing; it isn't one of the photos.
    if let Some(card) = hub.share_session(&tv) {
        items.retain(|i| i.content_id != card.content_id);
    }
    // Newest first, like a camera roll.
    items.sort_by(|a, b| b.image_date.cmp(&a.image_date).then_with(|| b.content_id.cmp(&a.content_id)));
    let state = TvState::load(&hub.state_file(&tv)).unwrap_or_default();
    let dir = hub.thumb_dir(&tv);
    let out: Vec<api::Photo> = items
        .iter()
        .map(|i| {
            let rec = state.uploads.get(&i.content_id);
            api::Photo {
                id: i.content_id.clone(),
                width: i.width,
                height: i.height,
                date: i.image_date.clone(),
                matte: i.matte_id.clone(),
                portrait_matte: i.portrait_matte_id.clone(),
                mine: rec.is_some(),
                source: rec.and_then(|r| r.source.file_name()).map(|n| n.to_string_lossy().to_string()),
                framing: rec.and_then(|r| r.framing.clone()),
                // Reframing re-prepares from the original file, so it has to still be there.
                reframe: rec.is_some_and(|r| r.source.is_file()),
                // Cached thumbnails load instantly; others arrive later via a `thumbs` event.
                thumb: find_thumb(&dir, &i.content_id).is_some(),
            }
        })
        .collect();

    // Warm the thumbnail cache in the background so the grid fills in quickly.
    let missing: Vec<String> =
        items.iter().map(|i| i.content_id.clone()).filter(|id| find_thumb(&dir, id).is_none()).collect();
    if !missing.is_empty() && offline.is_none() {
        let (hub, tv) = (Arc::clone(&hub), Arc::clone(&tv));
        tokio::spawn(async move {
            for chunk in missing.chunks(12) {
                match fetch_thumbs(&hub, &tv, chunk).await {
                    Ok(ids) if ids.is_empty() => {}
                    Ok(ids) => hub.emit(ServerEvent::Thumbs { tv: tv.name.clone(), ids }),
                    Err(e) => {
                        tracing::warn!("thumbnail prefetch for {}: {e:#}", tv.name);
                        break;
                    }
                }
            }
        });
    }
    Ok(Json(api::PhotoList { photos: out, as_of: snap.at, offline }))
}

fn safe_id(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

fn find_thumb(dir: &Path, id: &str) -> Option<PathBuf> {
    let base = safe_id(id);
    // An empty file is a failed write, not a thumbnail.
    ["jpg", "png"].iter().map(|e| dir.join(format!("{base}.{e}"))).find(|p| p.metadata().is_ok_and(|m| m.len() > 0))
}

/// The thumbnail as a file the browser can show, with its extension: as sent if it's a JPEG or
/// PNG, converted if it's some other image, `None` if it isn't one.
fn browser_image(data: Vec<u8>) -> Option<(Vec<u8>, &'static str)> {
    match image::guess_format(&data).ok()? {
        image::ImageFormat::Jpeg => Some((data, "jpg")),
        image::ImageFormat::Png => Some((data, "png")),
        _ => image_prep::preview(&data, 1024).ok().map(|jpeg| (jpeg, "jpg")),
    }
}

/// Fetch and cache thumbnails, returning the ids that are now cached. The TV sometimes skips a
/// photo in a batch, so those are asked for one by one, and if the TV still sends nothing, a
/// photo sent from here gets a thumbnail made from its original. Photos that end up without
/// one aren't asked for again for a while.
async fn fetch_thumbs(hub: &Shared, tv: &Arc<Tv>, ids: &[String]) -> anyhow::Result<Vec<String>> {
    let dir = hub.thumb_dir(tv);
    let _guard = tv.xfer.lock().await;
    let mut cached = Vec::new();
    let mut want: Vec<String> = Vec::new();
    {
        let no_thumb = tv.no_thumb.lock().unwrap();
        for id in ids {
            if find_thumb(&dir, id).is_some() {
                cached.push(id.clone());
            } else if no_thumb.get(id).is_none_or(|at| at.elapsed() >= RETRY_THUMB_AFTER) {
                want.push(id.clone());
            }
        }
    }
    if want.is_empty() {
        return Ok(cached);
    }
    std::fs::create_dir_all(&dir)?;
    let save = |id: &str, data: Vec<u8>| -> bool {
        let Some((data, ext)) = browser_image(data) else {
            tracing::warn!("{}: the thumbnail for {id} isn't an image", tv.name);
            return false;
        };
        for old in ["jpg", "png"] {
            let _ = std::fs::remove_file(dir.join(format!("{}.{old}", safe_id(id))));
        }
        match std::fs::write(dir.join(format!("{}.{ext}", safe_id(id))), data) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("caching the thumbnail for {id}: {e}");
                false
            }
        }
    };
    // Which photo each thumbnail is for: the TV labels them, but not always exactly as asked.
    let assign = |label: &str, asked: &[String]| -> Option<String> {
        let stem = label.rsplit_once('.').map_or(label, |(s, _)| s);
        asked
            .iter()
            .find(|id| id.as_str() == label || id.eq_ignore_ascii_case(stem))
            .or(if asked.len() == 1 { asked.first() } else { None })
            .cloned()
    };

    let batch = hub
        .with_art(tv, |a| {
            let want = want.clone();
            async move { a.thumbnails(&want).await }
        })
        .await;
    match batch {
        Ok(thumbs) => {
            for t in thumbs {
                match assign(&t.content_id, &want) {
                    Some(id) if save(&id, t.data) => cached.push(id),
                    Some(_) => {}
                    None => {
                        tracing::warn!("{}: got a thumbnail for {:?}, which wasn't asked for", tv.name, t.content_id);
                    }
                }
            }
        }
        // Couldn't reach the TV: nothing more to try now.
        Err(e) if tv.down_reason().is_some() || e.downcast_ref::<samsung_frame::Error>().is_some_and(is_unanswered) => {
            return if cached.is_empty() { Err(e) } else { Ok(cached) };
        }
        Err(e) => tracing::warn!("{}: thumbnails for {want:?}: {e}", tv.name),
    }

    let mut missing: Vec<String> = want.into_iter().filter(|id| !cached.contains(id)).collect();
    if !missing.is_empty() {
        for id in missing.clone() {
            let one = hub
                .with_art(tv, |a| {
                    let id = id.clone();
                    async move { a.thumbnails_one_by_one(std::slice::from_ref(&id)).await }
                })
                .await;
            match one {
                Ok(thumbs) => {
                    if let Some(t) = thumbs.into_iter().next()
                        && save(&id, t.data)
                    {
                        cached.push(id.clone());
                    }
                }
                Err(e) => tracing::info!("{}: no thumbnail for {id} from the TV: {e}", tv.name),
            }
        }
        missing.retain(|id| !cached.contains(id));
    }

    if !missing.is_empty() {
        let state = TvState::load(&hub.state_file(tv)).unwrap_or_default();
        for id in &missing {
            let Some(source) = state.uploads.get(id).map(|r| r.source.clone()).filter(|p| p.is_file()) else {
                continue;
            };
            let made = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
                Ok(image_prep::preview(&std::fs::read(source)?, 512)?)
            })
            .await?;
            match made {
                Ok(jpeg) => {
                    if save(id, jpeg) {
                        cached.push(id.clone());
                    }
                }
                Err(e) => tracing::warn!("{}: making a thumbnail for {id} from its original: {e:#}", tv.name),
            }
        }
        missing.retain(|id| !cached.contains(id));
    }

    let mut no_thumb = tv.no_thumb.lock().unwrap();
    for id in &cached {
        no_thumb.remove(id);
    }
    for id in missing {
        tracing::warn!("{}: no thumbnail for {id}", tv.name);
        no_thumb.insert(id, Instant::now());
    }
    Ok(cached)
}

async fn thumb(State(hub): State<Shared>, UrlPath((name, id)): UrlPath<(String, String)>) -> ApiResult<Response> {
    let tv = hub.tv(&name)?;
    let dir = hub.thumb_dir(&tv);
    if find_thumb(&dir, &id).is_none() {
        fetch_thumbs(&hub, &tv, std::slice::from_ref(&id)).await?;
    }
    let path = find_thumb(&dir, &id)
        .ok_or_else(|| status_err(StatusCode::NOT_FOUND, format!("the TV has no preview of {id}")))?;
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
) -> ApiResult<Json<api::Uploaded>> {
    let tv = hub.tv(&name)?;
    if body.is_empty() {
        return Err(status_err(StatusCode::BAD_REQUEST, "empty upload"));
    }
    let framing = q.framing()?;
    let file_name = q.name.unwrap_or_else(|| format!("photo-{}.jpg", chrono::Local::now().format("%Y%m%d-%H%M%S")));
    match hub.ingest(&tv, &file_name, &body, q.show.unwrap_or(false), framing).await {
        Ok(v) => Ok(Json(v)),
        Err(e) => {
            hub.emit(ServerEvent::Error { tv: tv.name.clone(), message: format!("{file_name}: {e:#}") });
            Err(e.into())
        }
    }
}

async fn show(
    State(hub): State<Shared>,
    headers: HeaderMap,
    UrlPath((name, id)): UrlPath<(String, String)>,
) -> ApiResult<StatusCode> {
    hub.require_admin(&headers)?;
    let tv = hub.tv(&name)?;
    hub.with_art(&tv, |a| {
        let id = id.clone();
        async move { a.select_image(&id, None, true).await }
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn set_matte(
    State(hub): State<Shared>,
    headers: HeaderMap,
    UrlPath((name, id)): UrlPath<(String, String)>,
    Json(b): Json<api::MatteUpdate>,
) -> ApiResult<StatusCode> {
    hub.require_admin(&headers)?;
    let tv = hub.tv(&name)?;
    hub.with_art(&tv, |a| {
        let (id, matte, portrait) = (id.clone(), b.matte.clone(), b.portrait.clone());
        async move { a.change_matte(&id, &matte, portrait.as_deref()).await }
    })
    .await?;
    hub.changed(&tv);
    Ok(StatusCode::NO_CONTENT)
}

async fn favorite(
    State(hub): State<Shared>,
    headers: HeaderMap,
    UrlPath((name, id)): UrlPath<(String, String)>,
    Json(b): Json<api::Toggle>,
) -> ApiResult<StatusCode> {
    hub.require_admin(&headers)?;
    let tv = hub.tv(&name)?;
    hub.with_art(&tv, |a| {
        let id = id.clone();
        async move { a.set_favorite(&id, b.on).await }
    })
    .await?;
    hub.changed(&tv);
    Ok(StatusCode::NO_CONTENT)
}

/// The original photo behind one of our uploads, as a JPEG preview for the framing editor.
async fn original(
    State(hub): State<Shared>,
    headers: HeaderMap,
    UrlPath((name, id)): UrlPath<(String, String)>,
) -> ApiResult<Response> {
    hub.require_admin(&headers)?;
    let tv = hub.tv(&name)?;
    let state = TvState::load(&hub.state_file(&tv))?;
    let rec = state
        .uploads
        .get(&id)
        .ok_or_else(|| status_err(StatusCode::NOT_FOUND, "only photos sent from here can be reframed"))?;
    let bytes = tokio::fs::read(&rec.source).await.map_err(|e| {
        status_err(StatusCode::NOT_FOUND, format!("the original ({}) is gone: {e}", rec.source.display()))
    })?;
    let jpeg = tokio::task::spawn_blocking(move || image_prep::preview(&bytes, 2048)).await??;
    Ok(([(header::CONTENT_TYPE, "image/jpeg"), (header::CACHE_CONTROL, "private, max-age=3600")], jpeg).into_response())
}

/// Re-prepare one of our uploads from its original with new framing, replacing it on the TV.
async fn reframe(
    State(hub): State<Shared>,
    headers: HeaderMap,
    UrlPath((name, id)): UrlPath<(String, String)>,
    Json(framing): Json<Framing>,
) -> ApiResult<Json<api::Reframed>> {
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
    let was_current =
        hub.with_art(&tv, |a| async move { a.current_artwork().await }).await.is_ok_and(|c| c.content_id == id);
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
    hub.changed(&tv);
    Ok(Json(api::Reframed { content_id: result.content_id, replaced: id, matte: result.matte }))
}

/// Delete from the TV. If the photo came from the TV's sync folder, its file is moved to
/// `.removed/` there, so the next sync doesn't put it back (and it can be recovered).
async fn delete_photo(
    State(hub): State<Shared>,
    headers: HeaderMap,
    UrlPath((name, id)): UrlPath<(String, String)>,
) -> ApiResult<Json<api::Deleted>> {
    hub.require_admin(&headers)?;
    let tv = hub.tv(&name)?;
    Ok(Json(delete_photos(&hub, &tv, vec![id]).await?.remove(0)))
}

/// `POST /api/tvs/{tv}/photos/delete` (admin): delete several photos at once.
async fn delete_many(
    State(hub): State<Shared>,
    headers: HeaderMap,
    UrlPath(name): UrlPath<String>,
    Json(b): Json<api::PhotoIds>,
) -> ApiResult<Json<Vec<api::Deleted>>> {
    hub.require_admin(&headers)?;
    let tv = hub.tv(&name)?;
    if b.ids.is_empty() {
        return Ok(Json(vec![]));
    }
    Ok(Json(delete_photos(&hub, &tv, b.ids).await?))
}

/// Delete photos from the TV. Originals that came from the TV's sync folder are moved aside, to
/// `.removed`, so the next sync doesn't put them back.
async fn delete_photos(hub: &Shared, tv: &Arc<Tv>, ids: Vec<String>) -> anyhow::Result<Vec<api::Deleted>> {
    hub.delete_from_tv(tv, ids.clone()).await?;
    let state_file = hub.state_file(tv);
    let mut state = TvState::load(&state_file)?;
    let folder = tv.config().folder;
    let mut out = Vec::new();
    let mut changed = false;
    for id in ids {
        let mut moved = None;
        if let Some(rec) = state.uploads.remove(&id) {
            changed = true;
            if let Some(folder) = &folder
                && rec.source.starts_with(folder)
                && rec.source.exists()
                && let Some(file) = rec.source.file_name()
            {
                let bin = folder.join(".removed");
                std::fs::create_dir_all(&bin)?;
                std::fs::rename(&rec.source, bin.join(file))?;
                moved = Some(bin.join(file));
            }
        }
        out.push(api::Deleted { deleted: id, moved_source_to: moved.map(|p| p.display().to_string()) });
    }
    if changed {
        state.save(&state_file)?;
    }
    hub.changed(tv);
    Ok(out)
}

async fn mattes(State(hub): State<Shared>, UrlPath(name): UrlPath<String>) -> ApiResult<Json<MatteList>> {
    let tv = hub.tv(&name)?;
    let file = hub.cache_file(&tv, "mattes");
    match hub.read_art(&tv, |a| async move { a.matte_list().await }).await {
        Ok(list) => {
            cache::save(&file, &Snapshot::now(list.clone()));
            Ok(Json(list))
        }
        // They hardly ever change, so the last list the TV sent will do.
        Err(e) => cache::load(&file).map(|s| Json(s.value)).ok_or_else(|| e.into()),
    }
}

async fn artmode(
    State(hub): State<Shared>,
    headers: HeaderMap,
    UrlPath(name): UrlPath<String>,
    Json(b): Json<api::Toggle>,
) -> ApiResult<StatusCode> {
    hub.require_admin(&headers)?;
    let tv = hub.tv(&name)?;
    hub.with_art(&tv, |a| async move { a.set_artmode(b.on).await }).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn slideshow(
    State(hub): State<Shared>,
    headers: HeaderMap,
    UrlPath(name): UrlPath<String>,
    Json(b): Json<api::SlideshowUpdate>,
) -> ApiResult<StatusCode> {
    hub.require_admin(&headers)?;
    let tv = hub.tv(&name)?;
    let (minutes, shuffle) = (b.minutes.filter(|m| *m > 0), b.shuffle.unwrap_or(true));
    let cat = if b.favorites.unwrap_or(false) { category::FAVORITES } else { category::MY_PHOTOS };
    hub.with_art(&tv, |a| async move { a.set_slideshow(minutes, shuffle, cat).await }).await?;
    hub.changed(&tv);
    Ok(StatusCode::NO_CONTENT)
}

/// Start syncing the TV's folder; progress arrives as events.
async fn sync_folder(
    State(hub): State<Shared>,
    headers: HeaderMap,
    UrlPath(name): UrlPath<String>,
    body: Option<Json<api::SyncRequest>>,
) -> ApiResult<StatusCode> {
    hub.require_admin(&headers)?;
    let tv = hub.tv(&name)?;
    let delete = body.is_some_and(|Json(b)| b.delete.unwrap_or(false));
    let folder = tv
        .config()
        .folder
        .ok_or_else(|| status_err(StatusCode::BAD_REQUEST, format!("no folder configured for {name}")))?;
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
            hub2.emit(ServerEvent::SyncStart {
                tv: tv.name.clone(),
                upload: plan.upload.len() as u32,
                delete: plan.delete.len() as u32,
            });
            let settings = upload_settings(&tv.config());
            let name = tv.name.clone();
            let outcome = sync::execute(&art, &mut state, &state_file, plan, &settings, |ev| match ev {
                sync::SyncEvent::Uploading { index, total, path } => hub2.emit(ServerEvent::SyncProgress {
                    tv: name.clone(),
                    index: index as u32,
                    total: total as u32,
                    name: path.file_name().map(|n| n.to_string_lossy().to_string()),
                }),
                sync::SyncEvent::UploadFailed { path, error } => {
                    hub2.emit(ServerEvent::Error { tv: name.clone(), message: format!("{}: {error}", path.display()) });
                }
                _ => {}
            })
            .await?;
            hub2.emit(ServerEvent::SyncDone {
                tv: tv.name.clone(),
                uploaded: outcome.uploaded.len() as u32,
                deleted: outcome.deleted as u32,
                failed: outcome.failed as u32,
            });
            Ok(())
        }
        .await;
        if let Err(e) = result {
            hub2.emit(ServerEvent::Error { tv: tv.name.clone(), message: format!("sync: {e:#}") });
        }
        hub2.changed(&tv);
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
    tv.down.lock().unwrap().take();
    Ok(StatusCode::ACCEPTED)
}

/// Put a "scan me" QR code on the TV that opens this TV's guest upload page. The slideshow is
/// paused meanwhile; `DELETE` (or [`SHARE_CARD_FOR`] passing) puts things back as they were.
async fn share_card(
    State(hub): State<Shared>,
    headers: HeaderMap,
    UrlPath(name): UrlPath<String>,
    Json(b): Json<api::ShareCardRequest>,
) -> ApiResult<Json<api::ShareCard>> {
    hub.require_admin(&headers)?;
    let tv = hub.tv(&name)?;
    let old = hub.share_session(&tv);
    // What to put back afterwards. If a card is already up, that's whatever was there before it.
    let (previous, slideshow) = if let Some(s) = &old {
        (s.previous.clone(), s.slideshow.clone())
    } else {
        let (current, ss) = hub
            .read_art(&tv, |a| async move {
                let (c, s) = tokio::join!(a.current_artwork(), a.slideshow_status());
                Ok((c.ok().map(|c| c.content_id), s.ok()))
            })
            .await?;
        let ss = ss.and_then(|s| {
            Some(PausedSlideshow {
                minutes: s.minutes?,
                shuffle: s.shuffle,
                category: s.category_id.unwrap_or_else(|| category::MY_PHOTOS.to_string()),
            })
        });
        (current, ss)
    };

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
    let session = ShareSession { content_id: id.clone(), previous, slideshow };
    hub.set_share_session(&tv, Some(&session));
    if let Some(ss) = &session.slideshow {
        let ss = ss.clone();
        hub.with_art(&tv, |a| {
            let ss = ss.clone();
            async move { a.set_slideshow(None, ss.shuffle, &ss.category).await }
        })
        .await?;
    }
    let shown = id.clone();
    hub.with_art(&tv, |a| {
        let id = shown.clone();
        async move { a.select_image(&id, None, true).await }
    })
    .await?;
    if let Some(old) = old
        && let Err(e) = hub.delete_from_tv(&tv, vec![old.content_id.clone()]).await
    {
        tracing::warn!("{}: deleting the old QR code card {}: {e:#}", tv.name, old.content_id);
    }
    hub.changed(&tv);

    // Don't leave it up forever if nobody says they're done.
    let (hub2, tv2, card) = (Arc::clone(&hub), Arc::clone(&tv), id.clone());
    tokio::spawn(async move {
        tokio::time::sleep(SHARE_CARD_FOR).await;
        if hub2.share_session(&tv2).is_some_and(|s| s.content_id == card)
            && let Err(e) = hub2.end_share(&tv2).await
        {
            tracing::warn!("{}: taking the QR code card down: {e:#}", tv2.name);
        }
    });
    Ok(Json(api::ShareCard { content_id: id }))
}

/// `DELETE /api/tvs/{tv}/share-card` (admin): done sharing; back to the photos.
async fn end_share(
    State(hub): State<Shared>,
    headers: HeaderMap,
    UrlPath(name): UrlPath<String>,
) -> ApiResult<StatusCode> {
    hub.require_admin(&headers)?;
    let tv = hub.tv(&name)?;
    hub.end_share(&tv).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A full-screen image with a QR code on a white card filling most of the screen, for the TV.
fn render_share_card(text: &str, w: u32, h: u32) -> anyhow::Result<Vec<u8>> {
    use image::{Rgb, RgbImage};
    let code = qrcode::QrCode::new(text.as_bytes()).map_err(|e| anyhow!("QR code: {e:?}"))?;
    let n = code.width() as u32;
    let quiet = 4;
    let total = n + 2 * quiet;
    // As big as the screen allows, so any phone camera can read it from across the room.
    let module = (h.min(w) * 90 / 100 / total).max(1);
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
    let border = (module / 3).max(2);
    fill(
        x0.saturating_sub(border),
        y0.saturating_sub(border),
        side + 2 * border,
        side + 2 * border,
        Rgb([201, 169, 110]),
    );
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
            matte: Some(String::new()),
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
        let jpeg = render_share_card("http://192.168.0.5:3273/tv/Living%20Room", 1920, 1080).unwrap();
        let img = image::load_from_memory(&jpeg).unwrap();
        assert_eq!((img.width(), img.height()), (1920, 1080));
    }
}
