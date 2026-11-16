//! A [LocalSend](https://localsend.org) (protocol v2) receiver, so phones and computers running
//! the free LocalSend app can "AirDrop" photos to this machine. Several virtual devices can be
//! advertised at once (one per TV, say); each gets its own HTTP port and identity.
//!
//! Discovery: multicast UDP announcements on 224.0.0.167:53317, answered with an HTTP
//! `register` call (plus a UDP fallback). Transfer: `prepare-upload` then one `upload` per file.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::Result;

pub const MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 167);
pub const MULTICAST_PORT: u16 = 53317;
const PROTOCOL_VERSION: &str = "2.1";
const MAX_FILE_SIZE: usize = 512 * 1024 * 1024;
const SESSION_TTL: Duration = Duration::from_secs(30 * 60);
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(120);

/// One advertised receiver.
#[derive(Debug, Clone)]
pub struct Device {
    /// Name shown in the sender's device list.
    pub alias: String,
    /// Stable unique id (senders remember devices by it).
    pub fingerprint: String,
    /// HTTP port; 0 picks the next free port from the base.
    pub port: u16,
    pub device_model: String,
    /// Optional PIN senders must enter.
    pub pin: Option<String>,
}

impl Device {
    fn info(&self, port: u16) -> Value {
        json!({
            "alias": self.alias,
            "version": PROTOCOL_VERSION,
            "deviceModel": self.device_model,
            "deviceType": "desktop",
            "fingerprint": self.fingerprint,
            "port": port,
            "protocol": "http",
            "download": false,
        })
    }
}

/// A file someone sent.
#[derive(Debug)]
pub struct Received {
    /// Index into the device list passed to [`serve`].
    pub device: usize,
    pub file_name: String,
    pub mime: Option<String>,
    pub data: Bytes,
    pub sender_alias: String,
    pub sender_ip: IpAddr,
    /// True for the last file of a transfer.
    pub last_in_session: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SenderInfo {
    #[serde(default)]
    alias: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileMeta {
    file_name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    file_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PrepareRequest {
    info: SenderInfo,
    files: HashMap<String, FileMeta>,
}

struct Session {
    sender_ip: IpAddr,
    sender_alias: String,
    /// file id -> (token, metadata)
    files: HashMap<String, (String, FileMeta)>,
    created: Instant,
}

struct DeviceState {
    index: usize,
    device: Device,
    port: u16,
    sessions: Mutex<HashMap<String, Session>>,
    tx: mpsc::Sender<Received>,
    accept: fn(&str, Option<&str>) -> bool,
}

/// Which offered files to accept: by default, anything that looks like an image.
pub fn accept_images(file_name: &str, mime: Option<&str>) -> bool {
    mime.is_some_and(|m| m.starts_with("image/"))
        || crate::image_prep::is_supported_or_heif_path(std::path::Path::new(file_name))
}

/// A running receiver.
pub struct Receiver {
    /// The ports each device actually listens on, in order.
    pub ports: Vec<u16>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for Receiver {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

/// Start advertising `devices` and receiving files into `tx`. Device ports of 0 are assigned
/// counting up from `base_port`.
pub async fn serve(devices: Vec<Device>, base_port: u16, tx: mpsc::Sender<Received>) -> Result<Receiver> {
    let mut tasks = Vec::new();
    let mut states = Vec::new();
    let mut next_port = base_port;
    for (index, device) in devices.into_iter().enumerate() {
        let (listener, port) = if device.port != 0 {
            (tokio::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, device.port)).await?, device.port)
        } else {
            loop {
                match tokio::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, next_port)).await {
                    Ok(l) => break (l, next_port),
                    Err(_) if next_port < base_port.saturating_add(100) => next_port += 1,
                    Err(e) => return Err(e.into()),
                }
            }
        };
        next_port = port + 1;
        let state = Arc::new(DeviceState {
            index,
            device,
            port,
            sessions: Mutex::new(HashMap::new()),
            tx: tx.clone(),
            accept: accept_images,
        });
        let app = router(Arc::clone(&state));
        tracing::info!("LocalSend device `{}` on port {port}", state.device.alias);
        tasks.push(tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await {
                tracing::error!("LocalSend HTTP server stopped: {e}");
            }
        }));
        states.push(state);
    }
    let ports = states.iter().map(|s| s.port).collect();
    // Without multicast (e.g. a container on a bridge network) senders can still reach the
    // targets by IP address, they just won't appear automatically.
    match multicast_socket() {
        Ok(socket) => tasks.push(tokio::spawn(multicast_loop(socket, states))),
        Err(e) => tracing::warn!("LocalSend discovery unavailable ({e}); senders must enter this machine's IP"),
    }
    Ok(Receiver { ports, tasks })
}

fn router(state: Arc<DeviceState>) -> Router {
    Router::new()
        .route("/api/localsend/v2/register", post(register))
        .route("/api/localsend/v1/register", post(register))
        .route("/api/localsend/v2/info", get(info))
        .route("/api/localsend/v1/info", get(info))
        .route("/api/localsend/v2/prepare-upload", post(prepare_upload))
        .route("/api/localsend/v2/upload", post(upload))
        .route("/api/localsend/v2/cancel", post(cancel))
        .layer(DefaultBodyLimit::max(MAX_FILE_SIZE))
        .with_state(state)
}

async fn register(State(s): State<Arc<DeviceState>>, body: Option<Json<Value>>) -> Json<Value> {
    if let Some(Json(b)) = body {
        let alias = b.get("alias").and_then(Value::as_str).unwrap_or("?").to_string();
        tracing::debug!("LocalSend register from {alias}");
    }
    Json(s.device.info(s.port))
}

async fn info(State(s): State<Arc<DeviceState>>) -> Json<Value> {
    Json(s.device.info(s.port))
}

#[derive(Deserialize)]
struct PinQuery {
    pin: Option<String>,
}

fn new_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

async fn prepare_upload(
    State(s): State<Arc<DeviceState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(q): Query<PinQuery>,
    Json(req): Json<PrepareRequest>,
) -> Response {
    if let Some(pin) = &s.device.pin
        && q.pin.as_deref() != Some(pin.as_str())
    {
        return (StatusCode::UNAUTHORIZED, "PIN required").into_response();
    }
    let mut files = HashMap::new();
    let mut tokens = serde_json::Map::new();
    for (id, meta) in req.files {
        if meta.size as usize > MAX_FILE_SIZE || !(s.accept)(&meta.file_name, meta.file_type.as_deref()) {
            tracing::info!("LocalSend: skipping {} ({:?})", meta.file_name, meta.file_type);
            continue;
        }
        let token = new_token();
        tokens.insert(id.clone(), json!(token));
        files.insert(id, (token, meta));
    }
    if files.is_empty() {
        return (StatusCode::FORBIDDEN, "only images can be sent to a Frame TV").into_response();
    }
    let session_id = new_token();
    tracing::info!("LocalSend: {} wants to send {} image(s) to {}", req.info.alias, files.len(), s.device.alias);
    let mut sessions = s.sessions.lock().unwrap();
    sessions.retain(|_, sess| sess.created.elapsed() < SESSION_TTL);
    sessions.insert(
        session_id.clone(),
        Session { sender_ip: addr.ip(), sender_alias: req.info.alias, files, created: Instant::now() },
    );
    Json(json!({"sessionId": session_id, "files": tokens})).into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadQuery {
    session_id: String,
    file_id: String,
    token: String,
}

async fn upload(
    State(s): State<Arc<DeviceState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(q): Query<UploadQuery>,
    body: Bytes,
) -> Response {
    let (meta, sender_alias, last) = {
        let mut sessions = s.sessions.lock().unwrap();
        let Some(sess) = sessions.get_mut(&q.session_id) else {
            return (StatusCode::FORBIDDEN, "unknown session").into_response();
        };
        if sess.sender_ip != addr.ip() {
            return (StatusCode::FORBIDDEN, "wrong sender").into_response();
        }
        match sess.files.get(&q.file_id) {
            Some((token, _)) if *token == q.token => {}
            _ => return (StatusCode::FORBIDDEN, "invalid token").into_response(),
        }
        let (_, meta) = sess.files.remove(&q.file_id).expect("checked above");
        let alias = sess.sender_alias.clone();
        let last = sess.files.is_empty();
        if last {
            sessions.remove(&q.session_id);
        }
        (meta, alias, last)
    };
    let received = Received {
        device: s.index,
        file_name: sanitize_file_name(&meta.file_name),
        mime: meta.file_type,
        data: body,
        sender_alias,
        sender_ip: addr.ip(),
        last_in_session: last,
    };
    if s.tx.send(received).await.is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, "receiver stopped").into_response();
    }
    StatusCode::OK.into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelQuery {
    session_id: String,
}

async fn cancel(State(s): State<Arc<DeviceState>>, Query(q): Query<CancelQuery>) -> StatusCode {
    s.sessions.lock().unwrap().remove(&q.session_id);
    StatusCode::OK
}

/// Keep only the final path component and strip characters Windows rejects.
pub fn sanitize_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let clean: String =
        base.chars().map(|c| if c.is_control() || r#"<>:"|?*"#.contains(c) { '_' } else { c }).collect();
    let clean = clean.trim().trim_matches('.').to_string();
    if clean.is_empty() { "image".into() } else { clean }
}

fn multicast_socket() -> Result<tokio::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    // Share the port with a LocalSend app that may be running on this machine.
    socket.set_reuse_address(true)?;
    socket.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, MULTICAST_PORT).into())?;
    let iface = match crate::discovery::local_ipv4() {
        Some(IpAddr::V4(ip)) => ip,
        _ => Ipv4Addr::UNSPECIFIED,
    };
    socket
        .join_multicast_v4(&MULTICAST_ADDR, &iface)
        .or_else(|_| socket.join_multicast_v4(&MULTICAST_ADDR, &Ipv4Addr::UNSPECIFIED))?;
    if iface != Ipv4Addr::UNSPECIFIED {
        let _ = socket.set_multicast_if_v4(&iface);
    }
    socket.set_multicast_loop_v4(true)?;
    socket.set_nonblocking(true)?;
    Ok(tokio::net::UdpSocket::from_std(socket.into())?)
}

#[derive(Deserialize)]
struct Announcement {
    #[serde(default)]
    alias: String,
    #[serde(default)]
    fingerprint: String,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    protocol: Option<String>,
    #[serde(default)]
    announce: Option<bool>,
    /// v1 name for `announce`.
    #[serde(default)]
    announcement: Option<bool>,
}

async fn multicast_loop(socket: tokio::net::UdpSocket, states: Vec<Arc<DeviceState>>) {
    let target = SocketAddr::from((MULTICAST_ADDR, MULTICAST_PORT));
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .danger_accept_invalid_certs(true)
        .build()
        .expect("reqwest client");
    let ours: Vec<String> = states.iter().map(|s| s.device.fingerprint.clone()).collect();
    let mut ticker = tokio::time::interval(ANNOUNCE_INTERVAL);
    let mut buf = vec![0u8; 8192];
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                for s in &states {
                    let mut msg = s.device.info(s.port);
                    msg["announce"] = json!(true);
                    msg["announcement"] = json!(true);
                    let _ = socket.send_to(msg.to_string().as_bytes(), target).await;
                }
            }
            recv = socket.recv_from(&mut buf) => {
                let Ok((n, from)) = recv else { continue };
                let Ok(a) = serde_json::from_slice::<Announcement>(&buf[..n]) else { continue };
                if ours.contains(&a.fingerprint) || !(a.announce.unwrap_or(false) || a.announcement.unwrap_or(false)) {
                    continue;
                }
                tracing::debug!("LocalSend announcement from {} at {}", a.alias, from.ip());
                for s in &states {
                    let info = s.device.info(s.port);
                    // Preferred: tell the sender about us over HTTP.
                    let url = format!(
                        "{}://{}:{}/api/localsend/v2/register",
                        a.protocol.as_deref().unwrap_or("https"),
                        from.ip(),
                        a.port.unwrap_or(MULTICAST_PORT)
                    );
                    let registered = http.post(&url).json(&info).send().await.is_ok_and(|r| r.status().is_success());
                    if !registered {
                        // Fallback: multicast reply.
                        let mut msg = info;
                        msg["announce"] = json!(false);
                        msg["announcement"] = json!(false);
                        let _ = socket.send_to(msg.to_string().as_bytes(), target).await;
                    }
                }
            }
        }
    }
}

/// A stable fingerprint for a device name on this machine.
pub fn fingerprint_for(name: &str) -> String {
    use sha2::{Digest, Sha256};
    let host = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default();
    hex::encode(Sha256::digest(format!("samsung-frame/{host}/{name}").as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_sanitized() {
        assert_eq!(sanitize_file_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_file_name(r"C:\x\IMG:1.jpg"), "IMG_1.jpg");
        assert_eq!(sanitize_file_name(".."), "image");
    }

    #[test]
    fn accepts_images_only() {
        assert!(accept_images("IMG_0001.HEIC", None));
        assert!(accept_images("x.bin", Some("image/jpeg")));
        assert!(!accept_images("notes.pdf", Some("application/pdf")));
    }

    #[test]
    fn prepare_request_parses() {
        let body = r#"{"info":{"alias":"Phone","version":"2.0","deviceModel":"iPhone","deviceType":"mobile","fingerprint":"abc","port":53317,"protocol":"https","download":false},
            "files":{"f1":{"id":"f1","fileName":"a.jpg","size":10,"fileType":"image/jpeg","sha256":null,"preview":null}}}"#;
        let r: PrepareRequest = serde_json::from_str(body).unwrap();
        assert_eq!(r.files["f1"].file_name, "a.jpg");
        assert_eq!(fingerprint_for("x").len(), 64);
    }
}
