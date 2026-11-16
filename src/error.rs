use std::time::Duration;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("websocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),

    #[error("TLS error: {0}")]
    Tls(#[from] native_tls::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("image error: {0}")]
    Image(#[from] image::ImageError),

    #[error("timed out after {0:?} waiting for {1}")]
    Timeout(Duration, String),

    /// `ms.channel.unauthorized`: the TV refused us outright (Allow denied, or device blocked in
    /// Settings > General > External Device Manager > Device Connection Manager).
    #[error("the TV refused the connection (ms.channel.unauthorized); allow this device on the TV and re-pair")]
    Unauthorized,

    /// `ms.channel.timeOut`: the token is missing or wrong, or nobody pressed Allow in time.
    #[error(
        "the TV did not accept the connection (ms.channel.timeOut); the token is missing or stale, or Allow was not pressed. Run `frametv pair`"
    )]
    ChannelTimeout,

    /// The art app answered a request with an `error` event.
    #[error("the TV rejected `{request}` with error code {code}")]
    Art { request: String, code: String },

    #[error("the TV closed the connection")]
    Disconnected,

    #[error("unexpected response from the TV: {0}")]
    Protocol(String),

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub(crate) fn protocol(msg: impl Into<String>) -> Self {
        Error::Protocol(msg.into())
    }

    /// True when the art app said it does not know this request (typical for a request
    /// name that belongs to a different art API generation).
    pub fn is_unsupported(&self) -> bool {
        matches!(self, Error::Art { code, .. } if code == "-9" || code == "-1")
    }
}
