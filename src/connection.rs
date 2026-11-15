//! Opening a TV WebSocket channel and performing the `ms.channel.connect` handshake.

use std::time::Duration;

use futures_util::StreamExt;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream, connect_async_tls_with_config};

use crate::protocol::{self, events};
use crate::{Error, Result};

pub(crate) type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Default time allowed to open a channel when we already have a token.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
/// Time allowed for someone to press "Allow" on the TV.
pub const PAIRING_TIMEOUT: Duration = Duration::from_secs(60);

/// The TVs use self-signed certificates, so certificate and hostname checks must be off.
pub(crate) fn insecure_tls() -> Result<native_tls::TlsConnector> {
    Ok(native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .build()?)
}

/// A WebSocket message decoded into JSON, with any trailing binary payload (legacy art APIs).
#[derive(Debug, Clone)]
pub(crate) struct Incoming {
    pub json: Value,
    pub binary: Option<Vec<u8>>,
}

/// Read the next JSON message, skipping control frames. `Ok(None)` means the stream ended.
pub(crate) async fn next_message<S>(ws: &mut S) -> Result<Option<Incoming>>
where
    S: futures_util::Stream<Item = std::result::Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    while let Some(msg) = ws.next().await {
        match msg? {
            Message::Text(text) => {
                let text: &str = text.as_ref();
                tracing::trace!(target: "samsung_frame::wire", "<- {text}");
                match serde_json::from_str(text) {
                    Ok(json) => return Ok(Some(Incoming { json, binary: None })),
                    Err(e) => tracing::debug!("ignoring non-JSON text frame ({e}): {text}"),
                }
            }
            Message::Binary(bytes) => {
                tracing::trace!(target: "samsung_frame::wire", "<- binary frame, {} bytes", bytes.len());
                match protocol::split_json_and_binary(&bytes) {
                    Some((json, tail)) => {
                        let binary = (!tail.is_empty()).then_some(tail);
                        return Ok(Some(Incoming { json, binary }));
                    }
                    None => tracing::debug!("ignoring binary frame without JSON header"),
                }
            }
            Message::Close(_) => return Ok(None),
            // Pings are answered by tungstenite automatically.
            _ => {}
        }
    }
    Ok(None)
}

pub(crate) struct Opened {
    pub ws: Ws,
    /// Token issued by the TV in `ms.channel.connect`, if any (new pairing, or token rotated).
    pub token: Option<String>,
}

/// Connect to a channel and wait for `ms.channel.connect`.
pub(crate) async fn open_channel(
    host: &str,
    port: u16,
    channel: &str,
    client_name: &str,
    token: Option<&str>,
    timeout: Duration,
) -> Result<Opened> {
    let url = protocol::channel_url(host, port, channel, client_name, token);
    tracing::debug!("connecting to {}", redact_token(&url));
    let connector = Connector::NativeTls(insecure_tls()?);

    let handshake = async {
        let (mut ws, _resp) = connect_async_tls_with_config(url.as_str(), None, false, Some(connector)).await?;
        loop {
            let Some(msg) = next_message(&mut ws).await? else {
                return Err(Error::Disconnected);
            };
            let event = msg.json.get("event").and_then(Value::as_str).unwrap_or("*");
            match event {
                e if events::IGNORE_AT_STARTUP.contains(&e) => continue,
                events::MS_CHANNEL_CONNECT => {
                    let token = msg.json.get("data").and_then(|d| protocol::str_field(d, "token"));
                    return Ok(Opened { ws, token });
                }
                events::MS_CHANNEL_UNAUTHORIZED => return Err(Error::Unauthorized),
                events::MS_CHANNEL_TIMEOUT => return Err(Error::ChannelTimeout),
                _ => return Err(Error::protocol(format!("unexpected event while connecting: {}", msg.json))),
            }
        }
    };

    tokio::time::timeout(timeout, handshake)
        .await
        .map_err(|_| Error::Timeout(timeout, format!("{channel} channel on {host}:{port}")))?
}

fn redact_token(url: &str) -> String {
    match url.find("&token=") {
        Some(i) => format!("{}&token=***", &url[..i]),
        None => url.to_string(),
    }
}
