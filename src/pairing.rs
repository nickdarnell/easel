//! Pairing a TV: ask for permission (the TV shows an Allow prompt), verify the art channel,
//! and build its [`TvConfig`].

use std::time::Duration;

use crate::art::ArtClient;
use crate::config::{Config, TvConfig};
use crate::protocol::category;
use crate::{Error, Result, remote, rest};

/// What we learned while pairing.
#[derive(Debug, Clone)]
pub struct Paired {
    pub tv: TvConfig,
    pub api_version: Option<String>,
    pub photo_count: Option<usize>,
    /// False if the TV does not report Frame/art-mode support.
    pub is_frame: bool,
}

/// Turn the TV's network name ("[TV] [Darnell] Living Room") into a friendly one ("Living Room").
pub fn friendly_name(raw: &str) -> String {
    let mut s = raw.trim();
    while let Some(rest) = s.strip_prefix('[').and_then(|r| r.split_once(']')).map(|(_, rest)| rest.trim_start()) {
        if rest.is_empty() {
            break;
        }
        s = rest;
    }
    s.to_string()
}

/// Pair with the TV at `host`. Blocks for up to `timeout` while someone presses Allow on the
/// TV (instant if this client was allowed before). Existing settings for the same TV (found by
/// `name` or `host`) are kept; the result still has to be saved with [`Config::upsert`].
pub async fn pair(cfg: &Config, host: &str, name: Option<&str>, port: u16, timeout: Duration) -> Result<Paired> {
    let info = rest::device_info(host, Duration::from_secs(5)).await.ok();
    let name = name
        .map(str::to_owned)
        .or_else(|| cfg.find(host).map(|t| t.name.clone()))
        .or_else(|| info.as_ref().map(|i| friendly_name(&i.display_name())))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| host.to_string());
    let mut tv = cfg.find(&name).or_else(|| cfg.find(host)).cloned().unwrap_or_else(|| TvConfig::new(&name, host));
    tv.name = name;
    tv.host = host.to_string();
    tv.port = port;
    if let Some(i) = &info {
        tv.mac = i.device.wifi_mac.clone().or(tv.mac);
        tv.model = i.device.model_name.clone().or(tv.model);
        tv.resolution = i.device.resolution.clone().or(tv.resolution);
    }

    let mut endpoint = tv.endpoint(&cfg.client_name);
    let token = remote::pair(&endpoint, timeout).await?;
    endpoint.token = Some(token.clone());
    tv.token = Some(token);

    let art = ArtClient::connect(&endpoint)
        .await
        .map_err(|e| Error::Other(format!("paired, but the art channel did not answer: {e}")))?;
    let api_version = art.api_version().await.ok();
    let photo_count = art.content_list(Some(category::MY_PHOTOS)).await.map(|l| l.len()).ok();
    if let Some(t) = art.issued_token() {
        tv.token = Some(t.to_string());
    }
    art.close().await;

    Ok(Paired { tv, api_version, photo_count, is_frame: info.as_ref().is_none_or(rest::DeviceInfo::is_frame) })
}

#[cfg(test)]
mod tests {
    use super::friendly_name;

    #[test]
    fn strips_tags() {
        assert_eq!(friendly_name("[TV] [Darnell] Living Room"), "Living Room");
        assert_eq!(friendly_name("[TV] Fireplace"), "Fireplace");
        assert_eq!(friendly_name("Bedroom"), "Bedroom");
        assert_eq!(friendly_name("[TV]"), "[TV]");
    }
}
