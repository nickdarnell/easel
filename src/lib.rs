//! Control Samsung **The Frame** TVs' art mode from Rust.
//!
//! ```no_run
//! # async fn demo() -> samsung_frame::Result<()> {
//! use samsung_frame::{Endpoint, art::{ArtClient, UploadOptions}, remote};
//!
//! let mut tv = Endpoint::new("192.168.1.50");
//! tv.token = Some(remote::pair(&tv, std::time::Duration::from_secs(60)).await?); // press Allow on the TV
//!
//! let art = ArtClient::connect(&tv).await?;
//! let id = art.upload(&std::fs::read("sunset.jpg")?, &UploadOptions::default()).await?;
//! art.select_image(&id, None, true).await?;
//! # Ok(()) }
//! ```

pub mod art;
pub mod config;
mod connection;
pub mod discovery;
mod error;
pub mod image_prep;
pub mod localsend;
pub mod pairing;
pub mod protocol;
pub mod remote;
pub mod rest;
pub mod sync;
pub mod wol;

pub use error::{Error, Result};

/// How to reach a TV.
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub host: String,
    /// 8002 (TLS + token, recommended) or 8001 (plain, older models).
    pub port: u16,
    pub token: Option<String>,
    /// Shown on the TV's Allow prompt; tokens are tied to it.
    pub client_name: String,
}

impl Endpoint {
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port: protocol::SECURE_PORT,
            token: None,
            client_name: config::DEFAULT_CLIENT_NAME.to_string(),
        }
    }
}
