//! Art mode client (`com.samsung.art-app` channel).
//!
//! Requests are sent as `ms.channel.emit` / `art_app_request`; replies come back as
//! `d2d_service_message` events whose `data` is a JSON string carrying `event` and
//! `request_id`. Large payloads (uploads, thumbnails) travel over a separate TCP socket
//! ("d2d socket") that the TV opens on request and describes in `conn_info`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use futures_util::SinkExt;
use futures_util::stream::{SplitSink, SplitStream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

use crate::connection::{self, Incoming, Ws};
use crate::protocol::{self, events, json_field, str_field};
use crate::{Endpoint, Error, Result};

/// Default wait for an art request's reply. A TV with another client on the art channel
/// (Home Assistant, SmartThings) can take a few seconds.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Wait for the TV to confirm an upload (`image_added`) after the bytes are sent.
pub const UPLOAD_CONFIRM_TIMEOUT: Duration = Duration::from_secs(60);
/// 2022 Frames reset a secured transfer socket if we connect immediately.
const SECURED_CONNECT_DELAY: Duration = Duration::from_secs(1);
const THUMBNAIL_BATCH: usize = 20;
/// How long to spend taking up a thumbnail offer that came too late.
const LATE_TRANSFER_TIMEOUT: Duration = Duration::from_secs(30);

/// One `d2d_service_message` from the art app.
#[derive(Debug, Clone, Serialize)]
pub struct ArtMessage {
    /// Sub-event, e.g. `ready_to_use`, `image_added`, `artmode_status`, `error`.
    pub event: String,
    pub request_id: Option<String>,
    /// The decoded inner payload.
    pub data: Value,
    /// Inline file bytes (only older art APIs).
    #[serde(skip)]
    pub binary: Option<Vec<u8>>,
    /// A reply to one of this client's own requests, rather than something the TV is telling
    /// everyone (a photo was added, the slideshow moved on, ...).
    #[serde(skip)]
    pub reply: bool,
}

/// An artwork entry from `get_content_list` / `get_current_artwork`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ArtItem {
    #[serde(default)]
    pub content_id: String,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub category_id: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub matte_id: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub portrait_matte_id: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_u32")]
    pub width: Option<u32>,
    #[serde(default, deserialize_with = "lenient::opt_u32")]
    pub height: Option<u32>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub image_date: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub content_type: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// A thumbnail fetched from the TV.
#[derive(Debug, Clone)]
pub struct Thumbnail {
    pub content_id: String,
    /// File extension reported by the TV (`jpg`, `png`).
    pub file_type: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct MatteList {
    pub types: Vec<String>,
    pub colors: Vec<String>,
}

/// An art mode setting such as `brightness` or `color_temperature`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtSetting {
    pub item: String,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub value: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub min: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    pub max: Option<String>,
    #[serde(default)]
    pub valid_values: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct UploadOptions {
    /// `jpg` or `png`.
    pub file_type: String,
    /// Landscape matte, e.g. `none`, `shadowbox_polar`, `flexible_apricot`.
    pub matte: String,
    /// Matte used when the image is shown on a portrait-oriented layout.
    pub portrait_matte: String,
    /// `YYYY:MM:DD HH:MM:SS`; the TV sorts My Photos by this. Defaults to now.
    pub image_date: Option<String>,
    pub confirm_timeout: Duration,
}

impl Default for UploadOptions {
    fn default() -> Self {
        Self {
            file_type: "jpg".into(),
            matte: "none".into(),
            portrait_matte: "shadowbox_polar".into(),
            image_date: None,
            confirm_timeout: UPLOAD_CONFIRM_TIMEOUT,
        }
    }
}

/// Slideshow configuration as reported by the TV.
#[derive(Debug, Clone, Serialize)]
pub struct SlideshowStatus {
    /// Minutes between images, or `None` when off.
    pub minutes: Option<u32>,
    pub shuffle: bool,
    pub category_id: Option<String>,
    pub raw: Value,
}

// ---------------------------------------------------------------------------------------------
// Reply routing
// ---------------------------------------------------------------------------------------------

struct Waiter {
    serial: u64,
    id: Option<String>,
    event: Option<String>,
    tx: oneshot::Sender<ArtMessage>,
}

impl Waiter {
    fn matches(&self, sub_event: &str, rid: Option<&str>) -> bool {
        let id_is = |id: &String| rid == Some(id.as_str());
        match (&self.event, &self.id) {
            (None, Some(id)) => id_is(id),
            (Some(ev), None) => ev == sub_event,
            // Some TVs (2022, art API 4.x) send `image_added` without a request id.
            (Some(ev), Some(id)) => {
                (ev == sub_event && rid.is_none_or(|r| r == id)) || (sub_event == "error" && id_is(id))
            }
            (None, None) => false,
        }
    }
}

/// A recent request, to tell its reply from the TV's own news.
struct Sent {
    id: String,
    request: String,
    content_id: Option<String>,
}

struct Router {
    waiters: StdMutex<Vec<Waiter>>,
    sent: StdMutex<std::collections::VecDeque<Sent>>,
    events: broadcast::Sender<ArtMessage>,
    /// Thumbnails that came after their request gave up waiting.
    late: broadcast::Sender<Thumbnail>,
    serial: AtomicU64,
    closed: AtomicBool,
}

impl Router {
    fn register(self: &Arc<Self>, id: Option<String>, event: Option<String>) -> Result<Pending> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(Error::Disconnected);
        }
        let serial = self.serial.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.waiters.lock().unwrap().push(Waiter { serial, id, event, tx });
        Ok(Pending { rx, serial, router: Arc::clone(self) })
    }

    fn dispatch(&self, incoming: Incoming) {
        tracing::trace!(target: "samsung_frame::wire", "<- {}", incoming.json);
        let outer_event = incoming.json.get("event").and_then(Value::as_str).unwrap_or("*");
        match outer_event {
            events::D2D_SERVICE_MESSAGE => {}
            events::MS_ERROR => {
                tracing::warn!("TV reported an error: {}", incoming.json);
                return;
            }
            other => {
                tracing::debug!("ignoring channel event {other}");
                return;
            }
        }
        let Some(data) = json_field(&incoming.json, "data") else {
            tracing::debug!("d2d message without data: {}", incoming.json);
            return;
        };
        let event = data.get("event").and_then(Value::as_str).unwrap_or("*").to_string();
        let request_id = str_field(&data, "request_id").or_else(|| str_field(&data, "id"));
        let asked = request_id.as_deref().and_then(|id| {
            self.sent.lock().unwrap().iter().find(|s| s.id == id).map(|s| (s.request.clone(), s.content_id.clone()))
        });
        let msg = ArtMessage { event, request_id, data, binary: incoming.binary, reply: asked.is_some() };
        tracing::debug!("art event `{}` (request {:?})", msg.event, msg.request_id);

        let _ = self.events.send(msg.clone());

        let mut waiters = self.waiters.lock().unwrap();
        if let Some(pos) = waiters.iter().position(|w| w.matches(&msg.event, msg.request_id.as_deref())) {
            let w = waiters.remove(pos);
            let _ = w.tx.send(msg);
            return;
        }
        drop(waiters);
        // Nobody's waiting any more. A thumbnail offer is still taken up: the TV holds its transfer
        // socket open for us (and can stop answering meanwhile), and the thumbnail is worth having.
        if let Some((request, content_id)) = asked
            && request.starts_with("get_thumbnail")
            && msg.event != "error"
        {
            tokio::spawn(collect_late(msg, request, content_id, self.late.clone()));
        }
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.waiters.lock().unwrap().clear(); // drops senders -> waiters see Disconnected
    }
}

/// A registered wait for a reply; unregisters itself if dropped early (timeout, cancel).
struct Pending {
    rx: oneshot::Receiver<ArtMessage>,
    serial: u64,
    router: Arc<Router>,
}

impl Pending {
    async fn wait(mut self, timeout: Duration, what: &str) -> Result<ArtMessage> {
        match tokio::time::timeout(timeout, &mut self.rx).await {
            Ok(Ok(msg)) => Ok(msg),
            Ok(Err(_)) => Err(Error::Disconnected),
            Err(_) => Err(Error::Timeout(timeout, what.to_string())),
        }
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if let Ok(mut w) = self.router.waiters.lock() {
            w.retain(|w| w.serial != self.serial);
        }
    }
}

fn check_error(msg: ArtMessage, request: &str) -> Result<ArtMessage> {
    if msg.event != "error" {
        return Ok(msg);
    }
    let request = json_field(&msg.data, "request_data")
        .and_then(|r| str_field(&r, "request"))
        .unwrap_or_else(|| request.to_string());
    let code = str_field(&msg.data, "error_code").unwrap_or_else(|| "?".into());
    Err(Error::Art { request, code })
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn connection_id() -> u64 {
    (uuid::Uuid::new_v4().as_u128() & 0xFFFF_FFFF) as u64
}

// ---------------------------------------------------------------------------------------------
// d2d transfer sockets
// ---------------------------------------------------------------------------------------------

trait D2dStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> D2dStream for T {}

#[derive(Debug)]
struct ConnInfo {
    ip: String,
    port: u16,
    key: Option<String>,
    secured: bool,
}

fn parse_conn_info(data: &Value) -> Result<ConnInfo> {
    let ci = json_field(data, "conn_info").ok_or_else(|| Error::protocol(format!("reply has no conn_info: {data}")))?;
    let ip = str_field(&ci, "ip").ok_or_else(|| Error::protocol("conn_info has no ip"))?;
    let port = str_field(&ci, "port")
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| Error::protocol("conn_info has no valid port"))?;
    let secured = match ci.get("secured") {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s.eq_ignore_ascii_case("true"),
        _ => false,
    };
    Ok(ConnInfo { ip, port, key: str_field(&ci, "key"), secured })
}

async fn d2d_connect(ci: &ConnInfo, secured: bool) -> Result<Box<dyn D2dStream>> {
    if secured {
        tokio::time::sleep(SECURED_CONNECT_DELAY).await;
    }
    tracing::debug!("opening d2d socket {}:{} (secured: {secured})", ci.ip, ci.port);
    let tcp = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect((ci.ip.as_str(), ci.port)))
        .await
        .map_err(|_| Error::Timeout(Duration::from_secs(10), format!("d2d socket {}:{}", ci.ip, ci.port)))??;
    tcp.set_nodelay(true).ok();
    if secured {
        let tls = tokio_native_tls::TlsConnector::from(connection::insecure_tls()?);
        Ok(Box::new(tls.connect(&ci.ip, tcp).await?))
    } else {
        Ok(Box::new(tcp))
    }
}

/// Take up a thumbnail offer that came after its request gave up.
async fn collect_late(msg: ArtMessage, request: String, asked: Option<String>, tx: broadcast::Sender<Thumbnail>) {
    let label = asked.clone().unwrap_or_else(|| request.clone());
    let files = if let Some(bytes) = msg.binary {
        let file_type = if bytes.starts_with(b"\x89PNG") { "png" } else { "jpg" };
        Ok(vec![Thumbnail { content_id: String::new(), file_type: file_type.into(), data: bytes }])
    } else {
        tokio::time::timeout(LATE_TRANSFER_TIMEOUT, async {
            let ci = parse_conn_info(&msg.data)?;
            // As when asked in time: `get_thumbnail` sockets are plain whatever they say.
            let secured = ci.secured && request != "get_thumbnail";
            let mut stream = d2d_connect(&ci, secured).await?;
            read_d2d_files(stream.as_mut()).await
        })
        .await
        .unwrap_or_else(|_| Err(Error::Timeout(LATE_TRANSFER_TIMEOUT, format!("late thumbnail ({label})"))))
    };
    match files {
        Ok(files) => {
            for mut f in files {
                if f.content_id.is_empty()
                    && let Some(id) = &asked
                {
                    f.content_id.clone_from(id);
                }
                tracing::info!("a late thumbnail came for {}", f.content_id);
                let _ = tx.send(f);
            }
        }
        Err(e) => tracing::debug!("late thumbnail ({label}): {e}"),
    }
}

/// Read `[u32 BE header length][JSON header][fileLength bytes]` records until `num + 1 == total`.
async fn read_d2d_files(stream: &mut dyn D2dStream) -> Result<Vec<Thumbnail>> {
    let mut out = Vec::new();
    loop {
        let mut len = [0u8; 4];
        if let Err(e) = stream.read_exact(&mut len).await {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                // The TV skips items it has no thumbnail for and closes early.
                if out.is_empty() {
                    return Err(Error::Other("the TV closed the transfer without sending a thumbnail".into()));
                }
                break;
            }
            return Err(e.into());
        }
        let header_len = u32::from_be_bytes(len) as usize;
        if header_len > 1 << 20 {
            return Err(Error::protocol(format!("implausible d2d header length {header_len}")));
        }
        let mut header = vec![0u8; header_len];
        stream.read_exact(&mut header).await?;
        let header: Value = serde_json::from_slice(&header)?;
        let file_len: usize = str_field(&header, "fileLength")
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| Error::protocol(format!("d2d header without fileLength: {header}")))?;
        let num: usize = str_field(&header, "num").and_then(|v| v.parse().ok()).unwrap_or(0);
        let total: usize = str_field(&header, "total").and_then(|v| v.parse().ok()).unwrap_or(1);
        let mut data = vec![0u8; file_len];
        stream.read_exact(&mut data).await?;
        out.push(Thumbnail {
            content_id: str_field(&header, "fileID").unwrap_or_default(),
            file_type: str_field(&header, "fileType").unwrap_or_else(|| "jpg".into()),
            data,
        });
        if num + 1 >= total {
            break;
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------------------------

/// A connected art mode session with one TV.
pub struct ArtClient {
    host: String,
    sink: Mutex<SplitSink<Ws, Message>>,
    router: Arc<Router>,
    reader: JoinHandle<()>,
    api_version: StdMutex<Option<String>>,
    slideshow_name: StdMutex<Option<&'static str>>,
    issued_token: Option<String>,
    request_timeout: Duration,
}

impl Drop for ArtClient {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl ArtClient {
    /// Open the art channel. Without a token the TV may show an "Allow" prompt (some models
    /// only prompt on the remote channel; use [`crate::remote::pair`] for those).
    pub async fn connect(endpoint: &Endpoint) -> Result<Self> {
        let timeout = if endpoint.token.is_some() { connection::OPEN_TIMEOUT } else { connection::PAIRING_TIMEOUT };
        let opened = connection::open_channel(
            &endpoint.host,
            endpoint.port,
            protocol::ART_CHANNEL,
            &endpoint.client_name,
            endpoint.token.as_deref(),
            timeout,
        )
        .await?;
        let mut ws = opened.ws;

        // The art channel follows ms.channel.connect with ms.channel.ready.
        let ready = tokio::time::timeout(connection::OPEN_TIMEOUT, async {
            loop {
                match connection::next_message(&mut ws).await? {
                    None => return Err(Error::Disconnected),
                    Some(m) if m.json.get("event").and_then(Value::as_str) == Some(events::MS_CHANNEL_READY) => {
                        return Ok(());
                    }
                    Some(m) => tracing::debug!("pre-ready message: {}", m.json),
                }
            }
        })
        .await;
        if let Ok(r) = ready {
            r?;
        } else {
            tracing::warn!("{}: art channel never sent ms.channel.ready; continuing anyway", endpoint.host);
        }

        let (sink, stream) = ws.split();
        let (events_tx, _) = broadcast::channel(64);
        let (late_tx, _) = broadcast::channel(64);
        let router = Arc::new(Router {
            waiters: StdMutex::new(Vec::new()),
            sent: StdMutex::new(std::collections::VecDeque::new()),
            events: events_tx,
            late: late_tx,
            serial: AtomicU64::new(0),
            closed: AtomicBool::new(false),
        });
        let reader = tokio::spawn(read_loop(stream, Arc::clone(&router)));

        Ok(Self {
            host: endpoint.host.clone(),
            sink: Mutex::new(sink),
            router,
            reader,
            api_version: StdMutex::new(None),
            slideshow_name: StdMutex::new(None),
            issued_token: opened.token,
            request_timeout: REQUEST_TIMEOUT,
        })
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    /// False once the TV has closed the art channel (reconnect to continue).
    pub fn is_connected(&self) -> bool {
        !self.router.closed.load(Ordering::SeqCst) && !self.reader.is_finished()
    }

    /// Token the TV issued during this connection, if it sent one.
    pub fn issued_token(&self) -> Option<&str> {
        self.issued_token.as_deref()
    }

    pub fn set_request_timeout(&mut self, timeout: Duration) {
        self.request_timeout = timeout;
    }

    /// Every art message the TV sends: replies to requests (marked [`ArtMessage::reply`]) and its
    /// own news (art mode changes, slideshow advances, photos added from the phone app, ...).
    pub fn subscribe(&self) -> broadcast::Receiver<ArtMessage> {
        self.router.events.subscribe()
    }

    /// Thumbnails the TV sent after their request had given up waiting for them.
    pub fn late_thumbnails(&self) -> broadcast::Receiver<Thumbnail> {
        self.router.late.subscribe()
    }

    pub async fn close(self) {
        let _ = self.sink.lock().await.close().await;
    }

    async fn send_ws(&self, msg: Message) -> Result<()> {
        self.sink.lock().await.send(msg).await?;
        Ok(())
    }

    /// Send a raw art request and wait for its reply. `id`/`request_id` are filled in.
    /// With `wait_event`, waits for that sub-event instead of the first reply.
    pub async fn request_with(
        &self,
        mut request: Value,
        wait_event: Option<&str>,
        timeout: Duration,
    ) -> Result<ArtMessage> {
        let obj = request.as_object_mut().ok_or_else(|| Error::Other("art request must be a JSON object".into()))?;
        let id = obj.get("id").and_then(Value::as_str).map_or_else(new_id, str::to_owned);
        obj.insert("id".into(), json!(id));
        obj.insert("request_id".into(), json!(id));
        let name = obj.get("request").and_then(Value::as_str).unwrap_or("?").to_string();

        {
            let mut sent = self.router.sent.lock().unwrap();
            if sent.len() >= 256 {
                sent.pop_front();
            }
            sent.push_back(Sent {
                id: id.clone(),
                request: name.clone(),
                content_id: request.get("content_id").and_then(Value::as_str).map(str::to_owned),
            });
        }
        let pending = self.router.register(Some(id), wait_event.map(str::to_owned))?;
        let envelope = protocol::art_request_envelope(&request);
        tracing::trace!(target: "samsung_frame::wire", "-> {envelope}");
        self.send_ws(Message::Text(envelope.to_string().into())).await?;
        let msg = pending.wait(timeout, &format!("reply to `{name}`")).await?;
        check_error(msg, &name)
    }

    pub async fn request(&self, request: Value) -> Result<ArtMessage> {
        self.request_with(request, None, self.request_timeout).await
    }

    /// For commands whose reply some TVs never send: a timeout is not an error.
    async fn request_lenient(&self, request: Value) -> Result<Option<ArtMessage>> {
        match self.request(request).await {
            Ok(m) => Ok(Some(m)),
            Err(Error::Timeout(..)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    // ---- version / capabilities --------------------------------------------------------------

    /// Art API version, e.g. `4.3.4.0` (2022+) or `2.03` (2021). Cached.
    pub async fn api_version(&self) -> Result<String> {
        if let Some(v) = self.api_version.lock().unwrap().clone() {
            return Ok(v);
        }
        let first = match self.request_with(json!({"request": "api_version"}), None, Duration::from_secs(3)).await {
            Ok(m) => str_field(&m.data, "version"),
            Err(Error::Art { .. } | Error::Timeout(..)) => None,
            Err(e) => return Err(e),
        };
        let version = if let Some(v) = first {
            v
        } else {
            let m = self.request(json!({"request": "get_api_version"})).await?;
            str_field(&m.data, "version").ok_or_else(|| Error::protocol("api version reply has no version"))?
        };
        *self.api_version.lock().unwrap() = Some(version.clone());
        Ok(version)
    }

    async fn api_major(&self) -> Option<u32> {
        match self.api_version().await {
            Ok(v) => protocol::major_version(&v),
            Err(e) => {
                tracing::debug!("could not read art API version: {e}");
                None
            }
        }
    }

    /// 2017/2018 Frames (art API < 2) upload and send thumbnails inline over the WebSocket.
    pub async fn is_legacy_api(&self) -> bool {
        matches!(self.api_major().await, Some(m) if m < 2)
    }

    pub async fn device_info(&self) -> Result<Value> {
        Ok(self.request(json!({"request": "get_device_info"})).await?.data)
    }

    // ---- content -----------------------------------------------------------------------------

    /// List artwork. `category`: e.g. [`protocol::category::MY_PHOTOS`]; `None` for everything.
    pub async fn content_list(&self, category: Option<&str>) -> Result<Vec<ArtItem>> {
        let msg = self
            .request_with(json!({"request": "get_content_list", "category": category}), None, Duration::from_secs(45))
            .await?;
        let list = json_field(&msg.data, "content_list").unwrap_or(Value::Array(vec![]));
        let items: Vec<ArtItem> = serde_json::from_value(list)?;
        Ok(match category {
            Some(c) => items.into_iter().filter(|i| i.category_id.as_deref() == Some(c)).collect(),
            None => items,
        })
    }

    pub async fn current_artwork(&self) -> Result<ArtItem> {
        let msg = self.request(json!({"request": "get_current_artwork"})).await?;
        Ok(serde_json::from_value(msg.data)?)
    }

    /// Display an artwork. `show: false` selects it without switching the screen to it.
    pub async fn select_image(&self, content_id: &str, category: Option<&str>, show: bool) -> Result<()> {
        self.request_lenient(json!({
            "request": "select_image",
            "category_id": category,
            "content_id": content_id,
            "show": show,
        }))
        .await?;
        Ok(())
    }

    pub async fn delete(&self, content_ids: &[String]) -> Result<()> {
        if content_ids.is_empty() {
            return Ok(());
        }
        let list: Vec<Value> = content_ids.iter().map(|id| json!({"content_id": id})).collect();
        self.request_lenient(json!({"request": "delete_image_list", "content_id_list": list})).await?;
        Ok(())
    }

    pub async fn set_favorite(&self, content_id: &str, favorite: bool) -> Result<()> {
        self.request_with(
            json!({"request": "change_favorite", "content_id": content_id, "status": if favorite {"on"} else {"off"}}),
            Some("favorite_changed"),
            self.request_timeout,
        )
        .await?;
        Ok(())
    }

    // ---- upload ------------------------------------------------------------------------------

    /// Upload an image (JPEG or PNG bytes). Returns the new `content_id` (e.g. `MY_F0042`).
    pub async fn upload(&self, image: &[u8], opts: &UploadOptions) -> Result<String> {
        let file_type = match opts.file_type.to_ascii_lowercase().as_str() {
            "jpeg" | "jpg" => "jpg".to_string(),
            other => other.to_string(),
        };
        if self.is_legacy_api().await {
            return self.upload_legacy(image, &file_type, opts).await;
        }
        let date =
            opts.image_date.clone().unwrap_or_else(|| chrono::Local::now().format("%Y:%m:%d %H:%M:%S").to_string());
        let id = new_id();
        // id, request_id and conn_info.id must all be the same.
        let request = json!({
            "request": "send_image",
            "file_type": file_type,
            "id": id,
            "request_id": id,
            "conn_info": {"d2d_mode": "socket", "connection_id": connection_id(), "id": id},
            "image_date": date,
            "matte_id": opts.matte,
            "portrait_matte_id": opts.portrait_matte,
            "file_size": image.len(),
        });
        let ready = self.request_with(request, None, Duration::from_secs(15)).await?;
        let ci = parse_conn_info(&ready.data)?;

        let confirm = self.router.register(Some(id), Some("image_added".into()))?;
        let header = json!({
            "num": 0,
            "total": 1,
            "fileLength": image.len(),
            "fileName": "dummy",
            "fileType": file_type,
            "secKey": ci.key.clone().unwrap_or_default(),
            "version": "0.0.1",
        })
        .to_string();

        let mut stream = d2d_connect(&ci, ci.secured).await?;
        stream.write_all(&(header.len() as u32).to_be_bytes()).await?;
        stream.write_all(header.as_bytes()).await?;
        for chunk in image.chunks(64 * 1024) {
            stream.write_all(chunk).await?;
        }
        stream.flush().await?;
        let _ = stream.shutdown().await;

        let added =
            check_error(confirm.wait(opts.confirm_timeout, "upload confirmation (image_added)").await?, "send_image")?;
        str_field(&added.data, "content_id")
            .ok_or_else(|| Error::protocol(format!("image_added without content_id: {}", added.data)))
    }

    async fn upload_legacy(&self, image: &[u8], file_type: &str, opts: &UploadOptions) -> Result<String> {
        let request = json!({
            "request": "send_image",
            "file_type": if file_type == "jpg" { "JPEG".to_string() } else { file_type.to_ascii_uppercase() },
            "matte_id": opts.matte,
            "id": new_id(),
        });
        let confirm = self.router.register(None, Some("image_added".into()))?;
        let frame = protocol::legacy_binary_upload(&request, image)?;
        self.send_ws(Message::Binary(frame.into())).await?;
        let added = confirm.wait(opts.confirm_timeout, "upload confirmation (image_added)").await?;
        str_field(&added.data, "content_id").ok_or_else(|| Error::protocol("image_added without content_id"))
    }

    // ---- thumbnails --------------------------------------------------------------------------

    pub async fn thumbnails(&self, content_ids: &[String]) -> Result<Vec<Thumbnail>> {
        if content_ids.is_empty() {
            return Ok(vec![]);
        }
        match self.api_major().await {
            Some(m) if m < 4 => self.thumbnails_one_by_one(content_ids).await,
            _ => {
                let mut out = Vec::new();
                for batch in content_ids.chunks(THUMBNAIL_BATCH) {
                    out.extend(self.thumbnail_list(batch).await?);
                }
                Ok(out)
            }
        }
    }

    /// Art API 4.x (2022+): one request, all thumbnails over one socket.
    async fn thumbnail_list(&self, content_ids: &[String]) -> Result<Vec<Thumbnail>> {
        let list: Vec<Value> = content_ids.iter().map(|id| json!({"content_id": id})).collect();
        let msg = self
            .request_with(
                json!({
                    "request": "get_thumbnail_list",
                    "content_id_list": list,
                    "conn_info": {"d2d_mode": "socket", "connection_id": connection_id(), "id": new_id()},
                }),
                None,
                Duration::from_secs(15),
            )
            .await?;
        let ci = parse_conn_info(&msg.data)?;
        let mut stream = d2d_connect(&ci, ci.secured).await?;
        read_d2d_files(stream.as_mut()).await
    }

    /// `get_thumbnail` per item (inline bytes on API < 2). Art API < 4 only has this; newer TVs
    /// mostly still answer it, which helps when `get_thumbnail_list` skips an item. Items the TV
    /// has no thumbnail for are left out (an error only if that's the only one asked for).
    pub async fn thumbnails_one_by_one(&self, content_ids: &[String]) -> Result<Vec<Thumbnail>> {
        let mut out = Vec::new();
        for id in content_ids {
            let msg = self
                .request_with(
                    json!({
                        "request": "get_thumbnail",
                        "content_id": id,
                        "conn_info": {"d2d_mode": "socket", "connection_id": connection_id(), "id": new_id()},
                    }),
                    None,
                    Duration::from_secs(15),
                )
                .await?;
            if let Some(bytes) = msg.binary {
                let file_type = if bytes.starts_with(b"\x89PNG") { "png" } else { "jpg" };
                out.push(Thumbnail { content_id: id.clone(), file_type: file_type.into(), data: bytes });
                continue;
            }
            let ci = parse_conn_info(&msg.data)?;
            let mut stream = d2d_connect(&ci, false).await?;
            match read_d2d_files(stream.as_mut()).await {
                Ok(mut files) => {
                    for f in &mut files {
                        if f.content_id.is_empty() {
                            f.content_id.clone_from(id);
                        }
                    }
                    out.extend(files);
                }
                Err(e) if content_ids.len() > 1 => tracing::warn!("no thumbnail for {id}: {e}"),
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    // ---- art mode, mattes, filters -----------------------------------------------------------

    /// `true` when the TV is showing art mode.
    pub async fn artmode(&self) -> Result<bool> {
        let msg = self.request(json!({"request": "get_artmode_status"})).await?;
        let v = str_field(&msg.data, "value").or_else(|| str_field(&msg.data, "status")).unwrap_or_default();
        Ok(v == "on")
    }

    pub async fn set_artmode(&self, on: bool) -> Result<()> {
        self.request_lenient(json!({"request": "set_artmode_status", "value": if on {"on"} else {"off"}})).await?;
        Ok(())
    }

    pub async fn matte_list(&self) -> Result<MatteList> {
        let msg = self.request(json!({"request": "get_matte_list"})).await?;
        Ok(parse_matte_list(&msg.data))
    }

    /// Change the matte of an artwork, e.g. `shadowbox_polar`, `modern_apricot`, or `none`.
    /// Not every matte is allowed for every image size.
    pub async fn change_matte(&self, content_id: &str, matte: &str, portrait_matte: Option<&str>) -> Result<()> {
        let mut req = json!({"request": "change_matte", "content_id": content_id, "matte_id": matte});
        if let Some(p) = portrait_matte {
            req["portrait_matte_id"] = json!(p);
        }
        self.request_lenient(req).await?;
        Ok(())
    }

    pub async fn photo_filters(&self) -> Result<Vec<String>> {
        let msg = self.request(json!({"request": "get_photo_filter_list"})).await?;
        let list = json_field(&msg.data, "filter_list").unwrap_or(Value::Array(vec![]));
        Ok(list
            .as_array()
            .map(|a| {
                a.iter().filter_map(|f| str_field(f, "filter_id").or_else(|| f.as_str().map(str::to_owned))).collect()
            })
            .unwrap_or_default())
    }

    pub async fn set_photo_filter(&self, content_id: &str, filter_id: &str) -> Result<()> {
        self.request_lenient(json!({"request": "set_photo_filter", "content_id": content_id, "filter_id": filter_id}))
            .await?;
        Ok(())
    }

    // ---- slideshow ---------------------------------------------------------------------------

    /// Art API 2.x calls the slideshow `auto_rotation`, 4.x calls it `slideshow`; each TV fails
    /// the other name, so try the likely one first and remember which worked.
    async fn slideshow_request(&self, action: &str, fields: Value) -> Result<ArtMessage> {
        let cached = *self.slideshow_name.lock().unwrap();
        let first = match cached {
            Some(n) => n,
            None => match self.api_major().await {
                Some(m) if m < 4 => "auto_rotation",
                _ => "slideshow",
            },
        };
        let second = if first == "slideshow" { "auto_rotation" } else { "slideshow" };
        let mut last_err = None;
        for name in [first, second] {
            let mut req = json!({"request": format!("{action}_{name}_status")});
            if let (Some(obj), Some(extra)) = (req.as_object_mut(), fields.as_object()) {
                obj.extend(extra.clone());
            }
            match self.request(req).await {
                Ok(m) => {
                    *self.slideshow_name.lock().unwrap() = Some(name);
                    return Ok(m);
                }
                Err(e @ (Error::Art { .. } | Error::Timeout(..))) => last_err = Some(e),
                Err(e) => return Err(e),
            }
        }
        Err(last_err.unwrap_or(Error::Other("slideshow request failed".into())))
    }

    pub async fn slideshow_status(&self) -> Result<SlideshowStatus> {
        let msg = self.slideshow_request("get", json!({})).await?;
        let value = str_field(&msg.data, "value");
        Ok(SlideshowStatus {
            minutes: value.as_deref().and_then(|v| v.parse().ok()).filter(|m| *m > 0),
            shuffle: str_field(&msg.data, "type").as_deref() == Some("shuffleslideshow"),
            category_id: str_field(&msg.data, "category_id"),
            raw: msg.data,
        })
    }

    /// Start (`Some(minutes)`) or stop (`None`) the slideshow over `category`.
    pub async fn set_slideshow(&self, minutes: Option<u32>, shuffle: bool, category: &str) -> Result<()> {
        let value = match minutes {
            Some(m) if m > 0 => m.to_string(),
            _ => "off".to_string(),
        };
        self.slideshow_request(
            "set",
            json!({
                "value": value,
                "category_id": category,
                "type": if shuffle { "shuffleslideshow" } else { "slideshow" },
            }),
        )
        .await?;
        Ok(())
    }

    // ---- settings ----------------------------------------------------------------------------

    /// Art mode settings: brightness, color_temperature, motion_sensitivity, motion_timer,
    /// brightness_sensor_setting (whatever the TV reports).
    pub async fn artmode_settings(&self) -> Result<Vec<ArtSetting>> {
        let major = self.api_major().await;
        if major.is_none_or(|m| m >= 4) {
            match self.request(json!({"request": "get_artmode_settings"})).await {
                Ok(m) => {
                    let list = json_field(&m.data, "data").unwrap_or(Value::Array(vec![]));
                    return Ok(serde_json::from_value(list)?);
                }
                Err(e) if e.is_unsupported() => {}
                Err(e) => return Err(e),
            }
        }
        // Art API 2.x: one request per setting.
        let mut out = Vec::new();
        for item in
            ["brightness", "color_temperature", "motion_sensitivity", "motion_timer", "brightness_sensor_setting"]
        {
            match self.request(json!({"request": format!("get_{item}")})).await {
                Ok(m) => out.push(ArtSetting {
                    item: item.to_string(),
                    value: str_field(&m.data, "value").or_else(|| str_field(&m.data, "status")),
                    min: str_field(&m.data, "min"),
                    max: str_field(&m.data, "max"),
                    valid_values: m.data.get("valid_values").cloned(),
                }),
                Err(Error::Art { .. } | Error::Timeout(..)) => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    /// Set an art mode setting: `brightness` ("0".."10" on most models), `color_temperature`
    /// ("-5".."5"), `motion_sensitivity` ("1".."3"), `motion_timer` ("off", "5", "15", ...),
    /// `brightness_sensor_setting` ("on"/"off").
    pub async fn set_setting(&self, item: &str, value: &str) -> Result<()> {
        self.request(json!({"request": format!("set_{item}"), "value": value})).await?;
        Ok(())
    }
}

async fn read_loop(mut stream: SplitStream<Ws>, router: Arc<Router>) {
    loop {
        match connection::next_message(&mut stream).await {
            Ok(Some(msg)) => router.dispatch(msg),
            Ok(None) => {
                tracing::debug!("art channel closed by the TV");
                break;
            }
            Err(e) => {
                tracing::debug!("art channel read error: {e}");
                break;
            }
        }
    }
    router.close();
}

/// Newer TVs send `matte_type_list` + `matte_color_list`; 2018 Frames send combined ids
/// like `shadowbox_polar` in `matte_types` / `matte_list`.
fn parse_matte_list(data: &Value) -> MatteList {
    let mut out = MatteList::default();
    if let Some(types) = json_field(data, "matte_type_list") {
        out.types = types.as_array().into_iter().flatten().filter_map(|t| str_field(t, "matte_type")).collect();
        out.colors = json_field(data, "matte_color_list")
            .and_then(|c| c.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|c| str_field(c, "color"))
            .collect();
        return out;
    }
    let combined = json_field(data, "matte_types").or_else(|| json_field(data, "matte_list")).unwrap_or_default();
    for m in combined.as_array().into_iter().flatten() {
        let Some(id) = str_field(m, "matte_id") else { continue };
        let (t, c) = id.split_once('_').unwrap_or((id.as_str(), ""));
        if !t.is_empty() && !out.types.iter().any(|x| x == t) {
            out.types.push(t.to_string());
        }
        if !c.is_empty() && !out.colors.iter().any(|x| x == c) {
            out.colors.push(c.to_string());
        }
    }
    out
}

/// Deserializers tolerant of the TV's habit of sending numbers as strings and vice versa.
mod lenient {
    use serde::{Deserialize, Deserializer};
    use serde_json::Value;

    pub fn opt_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
        Ok(match Option::<Value>::deserialize(d)? {
            Some(Value::String(s)) => Some(s),
            Some(Value::Number(n)) => Some(n.to_string()),
            Some(Value::Bool(b)) => Some(b.to_string()),
            _ => None,
        })
    }

    pub fn opt_u32<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
        Ok(match Option::<Value>::deserialize(d)? {
            Some(Value::Number(n)) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
            Some(Value::String(s)) => s.trim().parse().ok(),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waiter(id: Option<&str>, event: Option<&str>) -> Waiter {
        let (tx, _rx) = oneshot::channel();
        Waiter { serial: 0, id: id.map(Into::into), event: event.map(Into::into), tx }
    }

    #[test]
    fn waiter_matching() {
        let by_id = waiter(Some("a"), None);
        assert!(by_id.matches("ready_to_use", Some("a")));
        assert!(!by_id.matches("ready_to_use", Some("b")));
        assert!(!by_id.matches("ready_to_use", None));

        let added = waiter(Some("a"), Some("image_added"));
        assert!(added.matches("image_added", Some("a")));
        assert!(added.matches("image_added", None)); // 2022 Frames omit the id
        assert!(!added.matches("image_added", Some("b")));
        assert!(!added.matches("ready_to_use", Some("a")));
        assert!(added.matches("error", Some("a")));

        let any_added = waiter(None, Some("image_added"));
        assert!(any_added.matches("image_added", Some("zzz")));
    }

    #[test]
    fn content_list_parses_lenient_numbers() {
        let v = json!([
            {"content_id": "MY_F0001", "category_id": "MY-C0002", "width": "3840", "height": 2160, "slideshow": "false"},
            {"content_id": "SAM-S0700", "category_id": "MY-C0008"}
        ]);
        let items: Vec<ArtItem> = serde_json::from_value(v).unwrap();
        assert_eq!(items[0].width, Some(3840));
        assert_eq!(items[0].height, Some(2160));
        assert_eq!(items[0].extra["slideshow"], "false");
        assert_eq!(items[1].width, None);
    }

    #[test]
    fn matte_lists_both_formats() {
        let new = json!({
            "matte_type_list": "[{\"matte_type\":\"none\"},{\"matte_type\":\"shadowbox\"}]",
            "matte_color_list": "[{\"color\":\"polar\"},{\"color\":\"black\"}]"
        });
        let m = parse_matte_list(&new);
        assert_eq!(m.types, ["none", "shadowbox"]);
        assert_eq!(m.colors, ["polar", "black"]);

        let old = json!({"matte_types": [{"matte_id": "shadowbox_polar"}, {"matte_id": "modern_polar"}, {"matte_id": "none"}]});
        let m = parse_matte_list(&old);
        assert_eq!(m.types, ["shadowbox", "modern", "none"]);
        assert_eq!(m.colors, ["polar"]);
    }

    #[test]
    fn conn_info_as_string_or_object() {
        let s = json!({"conn_info": "{\"ip\":\"10.0.0.9\",\"port\":\"34567\",\"key\":\"k\",\"secured\":true}"});
        let ci = parse_conn_info(&s).unwrap();
        assert_eq!((ci.ip.as_str(), ci.port, ci.secured), ("10.0.0.9", 34567, true));
        let o = json!({"conn_info": {"ip": "10.0.0.9", "port": 1234}});
        let ci = parse_conn_info(&o).unwrap();
        assert_eq!((ci.port, ci.secured, ci.key), (1234, false, None));
    }

    #[test]
    fn error_reply_maps_to_art_error() {
        let msg = ArtMessage {
            event: "error".into(),
            request_id: Some("x".into()),
            data: json!({"event": "error", "error_code": "-9", "request_data": "{\"request\":\"get_slideshow_status\"}"}),
            binary: None,
            reply: true,
        };
        let err = check_error(msg, "fallback").unwrap_err();
        assert!(err.is_unsupported());
        assert!(err.to_string().contains("get_slideshow_status"));
    }
}
