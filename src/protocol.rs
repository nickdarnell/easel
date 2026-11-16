//! Wire-level constants and helpers for the Samsung Tizen WebSocket API (2016+ TVs).
//!
//! Protocol notes were derived from the open-source `samsung-tv-ws-api` projects
//! (xchwarze, NickWaterton) and verified against their behaviour notes per model year.

use base64::Engine as _;
use serde_json::{Value, json};

/// Art mode application channel.
pub const ART_CHANNEL: &str = "com.samsung.art-app";
/// Remote control channel. Every model shows the "Allow" prompt here, so pairing uses it.
pub const REMOTE_CHANNEL: &str = "samsung.remote.control";

/// TLS WebSocket port that supports token auth.
pub const SECURE_PORT: u16 = 8002;
/// Plain HTTP REST + legacy WebSocket port.
pub const REST_PORT: u16 = 8001;

pub mod events {
    pub const D2D_SERVICE_MESSAGE: &str = "d2d_service_message";
    pub const MS_CHANNEL_CONNECT: &str = "ms.channel.connect";
    pub const MS_CHANNEL_READY: &str = "ms.channel.ready";
    pub const MS_CHANNEL_UNAUTHORIZED: &str = "ms.channel.unauthorized";
    pub const MS_CHANNEL_TIMEOUT: &str = "ms.channel.timeOut";
    pub const MS_ERROR: &str = "ms.error";
    /// Events some TVs send before `ms.channel.connect`; skip them.
    pub const IGNORE_AT_STARTUP: &[&str] = &["ed.edenTV.update", "ms.voiceApp.hide"];
}

/// Well-known art categories.
pub mod category {
    /// "My Photos": images uploaded by the user.
    pub const MY_PHOTOS: &str = "MY-C0002";
    /// Favourites.
    pub const FAVORITES: &str = "MY-C0004";
    /// Art Store purchases / subscription.
    pub const STORE: &str = "MY-C0008";
}

/// Wrap IPv6 literals in brackets for URLs.
pub fn format_host(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') { format!("[{host}]") } else { host.to_string() }
}

pub fn channel_url(host: &str, port: u16, channel: &str, client_name: &str, token: Option<&str>) -> String {
    let host = format_host(host);
    let name = base64::engine::general_purpose::STANDARD.encode(client_name);
    if port == SECURE_PORT {
        match token {
            Some(t) if !t.is_empty() => {
                format!("wss://{host}:{port}/api/v2/channels/{channel}?name={name}&token={t}")
            }
            _ => format!("wss://{host}:{port}/api/v2/channels/{channel}?name={name}"),
        }
    } else {
        format!("ws://{host}:{port}/api/v2/channels/{channel}?name={name}")
    }
}

/// `ms.channel.emit` envelope carrying an art app request. The inner request is sent as a JSON
/// *string*, not a nested object.
pub fn art_request_envelope(request: &Value) -> Value {
    json!({
        "method": "ms.channel.emit",
        "params": {
            "event": "art_app_request",
            "to": "host",
            "data": request.to_string(),
        }
    })
}

/// Remote key press (e.g. `KEY_POWER`).
pub fn remote_key(key: &str, cmd: &str) -> Value {
    json!({
        "method": "ms.remote.control",
        "params": {
            "Cmd": cmd,
            "DataOfCmd": key,
            "Option": "false",
            "TypeOfRemote": "SendRemoteKey",
        }
    })
}

/// Parse a WebSocket binary frame from older art APIs (2017/2018 Frames): an optional binary
/// prefix, a JSON object, an optional newline, then raw file bytes.
pub fn split_json_and_binary(buf: &[u8]) -> Option<(Value, Vec<u8>)> {
    let start = buf.iter().position(|&b| b == b'{')?;
    let mut stream = serde_json::Deserializer::from_slice(&buf[start..]).into_iter::<Value>();
    let value = stream.next()?.ok()?;
    if !value.is_object() {
        return None;
    }
    let mut tail = &buf[start + stream.byte_offset()..];
    if tail.first() == Some(&b'\n') {
        tail = &tail[1..];
    }
    Some((value, tail.to_vec()))
}

/// Legacy (art API < 2) uploads go as one binary WebSocket message:
/// big-endian u16 header length, the compact JSON envelope, then the image bytes.
pub fn legacy_binary_upload(request: &Value, image: &[u8]) -> Result<Vec<u8>, crate::Error> {
    let header = serde_json::to_vec(&art_request_envelope(request))?;
    let len = u16::try_from(header.len()).map_err(|_| crate::Error::protocol("upload header too large"))?;
    let mut out = Vec::with_capacity(2 + header.len() + image.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(image);
    Ok(out)
}

/// Some TV fields are JSON-in-a-string (`content_list`, `conn_info`, ...). Accept either form.
pub fn json_field(data: &Value, key: &str) -> Option<Value> {
    match data.get(key)? {
        Value::String(s) => serde_json::from_str(s).ok(),
        Value::Null => None,
        other => Some(other.clone()),
    }
}

/// A string-or-number field as a string.
pub fn str_field(data: &Value, key: &str) -> Option<String> {
    match data.get(key)? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Major version number from an art API version string like "4.3.4.0" or "0.97".
pub fn major_version(version: &str) -> Option<u32> {
    version.split('.').next()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_formats() {
        assert_eq!(
            channel_url("10.0.0.5", 8002, ART_CHANNEL, "Easel", Some("1234")),
            "wss://10.0.0.5:8002/api/v2/channels/com.samsung.art-app?name=RWFzZWw=&token=1234"
        );
        assert_eq!(
            channel_url("10.0.0.5", 8002, REMOTE_CHANNEL, "Easel", None),
            "wss://10.0.0.5:8002/api/v2/channels/samsung.remote.control?name=RWFzZWw="
        );
        assert_eq!(
            channel_url("fe80::1", 8001, ART_CHANNEL, "x", Some("t")),
            "ws://[fe80::1]:8001/api/v2/channels/com.samsung.art-app?name=eA=="
        );
    }

    #[test]
    fn envelope_embeds_request_as_string() {
        let env = art_request_envelope(&json!({"request": "get_artmode_status"}));
        assert_eq!(env["method"], "ms.channel.emit");
        assert_eq!(env["params"]["data"], "{\"request\":\"get_artmode_status\"}");
    }

    #[test]
    fn binary_frame_split() {
        let mut buf = b"\x00\x10{\"event\":\"x\",\"s\":\"}{\"}\n".to_vec();
        buf.extend_from_slice(b"\x89PNGDATA");
        let (v, tail) = split_json_and_binary(&buf).unwrap();
        assert_eq!(v["event"], "x");
        assert_eq!(tail, b"\x89PNGDATA");
    }

    #[test]
    fn legacy_upload_layout() {
        let msg = legacy_binary_upload(&json!({"request": "send_image"}), b"IMG").unwrap();
        let len = u16::from_be_bytes([msg[0], msg[1]]) as usize;
        let header: Value = serde_json::from_slice(&msg[2..2 + len]).unwrap();
        assert_eq!(header["params"]["event"], "art_app_request");
        assert_eq!(&msg[2 + len..], b"IMG");
    }

    #[test]
    fn versions() {
        assert_eq!(major_version("4.3.4.0"), Some(4));
        assert_eq!(major_version("0.97"), Some(0));
        assert_eq!(major_version("garbage"), None);
    }
}
