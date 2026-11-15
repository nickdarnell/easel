//! `http://<tv>:8001/api/v2/` device info. Answers whenever the TV's network stack is up
//! (on, art mode, and on most models in standby).

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{Result, protocol};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub device: Device,
    #[serde(default, rename = "isSupport")]
    pub is_support: Option<String>,
    #[serde(default)]
    pub uri: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Device {
    #[serde(default, rename = "FrameTVSupport")]
    pub frame_tv_support: Option<String>,
    #[serde(default, rename = "PowerState")]
    pub power_state: Option<String>,
    #[serde(default, rename = "TokenAuthSupport")]
    pub token_auth_support: Option<String>,
    /// Internal model code, e.g. `22_PONTUSM_FTV` (leading digits = model year).
    #[serde(default)]
    pub model: Option<String>,
    /// Retail model, e.g. `QE55LS03BAUXXU`.
    #[serde(default, rename = "modelName")]
    pub model_name: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, rename = "wifiMac")]
    pub wifi_mac: Option<String>,
    /// Panel resolution, e.g. `3840x2160`.
    #[serde(default)]
    pub resolution: Option<String>,
    #[serde(default)]
    pub ip: Option<String>,
    #[serde(default, rename = "networkType")]
    pub network_type: Option<String>,
    #[serde(default, rename = "firmwareVersion")]
    pub firmware_version: Option<String>,
    #[serde(default, rename = "OS")]
    pub os: Option<String>,
}

impl DeviceInfo {
    pub fn is_frame(&self) -> bool {
        self.device.frame_tv_support.as_deref() == Some("true")
    }

    /// Model year (2-digit, e.g. 22) from the internal model code; 0 if unknown.
    pub fn model_year(&self) -> u32 {
        self.device
            .model
            .as_deref()
            .and_then(|m| m.split('_').next())
            .and_then(|y| y.parse().ok())
            .unwrap_or(0)
    }

    /// Panel resolution as (width, height).
    pub fn resolution(&self) -> Option<(u32, u32)> {
        let (w, h) = self.device.resolution.as_deref()?.split_once('x')?;
        Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
    }

    /// Friendly name with HTML entities the TV sometimes sends decoded.
    pub fn display_name(&self) -> String {
        let raw = self.device.name.clone().or_else(|| self.name.clone()).unwrap_or_else(|| "Samsung TV".into());
        raw.replace("&quot;", "\"").replace("&amp;", "&").replace("&#39;", "'")
    }

    /// Normalized power state: `on`, `standby`, or whatever the TV reports. TVs before 2019
    /// do not report PowerState but only answer when on.
    pub fn power_state(&self) -> &str {
        self.device.power_state.as_deref().unwrap_or("on")
    }
}

pub async fn device_info(host: &str, timeout: Duration) -> Result<DeviceInfo> {
    let url = format!("http://{}:{}/api/v2/", protocol::format_host(host), protocol::REST_PORT);
    let client = reqwest::Client::builder().timeout(timeout).build()?;
    Ok(client.get(url).send().await?.error_for_status()?.json().await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_frame_reply() {
        let body = r#"{"device":{"FrameTVSupport":"true","PowerState":"on","model":"22_PONTUSM_FTV",
            "modelName":"QE55LS03BAUXXU","name":"[TV] Living Room &amp; Hall","resolution":"3840x2160",
            "wifiMac":"aa:bb:cc:dd:ee:ff","TokenAuthSupport":"true"},"id":"uuid:abc","isSupport":"{}",
            "name":"[TV] Living Room","type":"Samsung SmartTV","uri":"http://10.0.0.5:8001/api/v2/"}"#;
        let info: DeviceInfo = serde_json::from_str(body).unwrap();
        assert!(info.is_frame());
        assert_eq!(info.model_year(), 22);
        assert_eq!(info.resolution(), Some((3840, 2160)));
        assert_eq!(info.display_name(), "[TV] Living Room & Hall");
    }
}
