//! Remote control channel: pairing (token acquisition) and key presses.

use std::time::Duration;

use futures_util::SinkExt;
use tokio_tungstenite::tungstenite::Message;

use crate::connection::{self, Ws};
use crate::{Endpoint, Error, Result, protocol};

/// Get a token by opening the remote control channel, which shows the "Allow" prompt on every
/// model (the art channel does not on some, e.g. 2017 and 2024 Frames).
///
/// If `endpoint.token` is set it is tried first and returned if the TV still accepts it.
pub async fn pair(endpoint: &Endpoint, timeout: Duration) -> Result<String> {
    if let Some(existing) = endpoint.token.as_deref() {
        match open(endpoint, Some(existing), connection::OPEN_TIMEOUT).await {
            Ok((ws, issued)) => {
                close(ws).await;
                return Ok(issued.unwrap_or_else(|| existing.to_string()));
            }
            Err(e) => tracing::info!("existing token rejected ({e}); asking for a new one"),
        }
    }
    let (ws, issued) = open(endpoint, None, timeout).await?;
    close(ws).await;
    issued.ok_or_else(|| {
        Error::Other(if endpoint.port == protocol::SECURE_PORT {
            "the TV accepted the connection but did not issue a token".into()
        } else {
            format!("port {} does not use tokens; pair on port {}", endpoint.port, protocol::SECURE_PORT)
        })
    })
}

async fn open(endpoint: &Endpoint, token: Option<&str>, timeout: Duration) -> Result<(Ws, Option<String>)> {
    let opened = connection::open_channel(
        &endpoint.host,
        endpoint.port,
        protocol::REMOTE_CHANNEL,
        &endpoint.client_name,
        token,
        timeout,
    )
    .await?;
    Ok((opened.ws, opened.token))
}

async fn close(mut ws: Ws) {
    let _ = ws.close(None).await;
}

/// A connected remote control session.
pub struct RemoteClient {
    ws: Ws,
}

impl RemoteClient {
    pub async fn connect(endpoint: &Endpoint) -> Result<Self> {
        let (ws, _) = open(endpoint, endpoint.token.as_deref(), connection::OPEN_TIMEOUT).await?;
        Ok(Self { ws })
    }

    /// Send a key such as `KEY_POWER`, `KEY_HOME`, `KEY_VOLUP`.
    pub async fn click(&mut self, key: &str) -> Result<()> {
        self.send(key, "Click").await
    }

    /// Hold a key for `duration` (e.g. hold `KEY_POWER` ~3s to fully power off a 2022-2024 Frame).
    pub async fn hold(&mut self, key: &str, duration: Duration) -> Result<()> {
        self.send(key, "Press").await?;
        tokio::time::sleep(duration).await;
        self.send(key, "Release").await
    }

    async fn send(&mut self, key: &str, cmd: &str) -> Result<()> {
        let payload = protocol::remote_key(key, cmd).to_string();
        self.ws.send(Message::Text(payload.into())).await?;
        Ok(())
    }

    pub async fn close(self) {
        close(self.ws).await;
    }
}
