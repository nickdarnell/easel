//! Persistent configuration: known TVs, their tokens and per-TV defaults.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Endpoint, Error, Result, protocol};

pub const DEFAULT_CLIENT_NAME: &str = "Easel";

fn default_client_name() -> String {
    DEFAULT_CLIENT_NAME.to_string()
}

fn default_port() -> u16 {
    protocol::SECURE_PORT
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Name shown on the TV's "Allow" prompt and device list. Tokens are tied to it.
    #[serde(default = "default_client_name")]
    pub client_name: String,
    /// TV used when a command does not name one and several are configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_tv: Option<String>,
    /// Web app settings (edited from the web page).
    #[serde(default)]
    pub server: ServerSettings,
    #[serde(default, rename = "tv")]
    pub tvs: Vec<TvConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            client_name: default_client_name(),
            default_tv: None,
            server: ServerSettings::default(),
            tvs: Vec::new(),
        }
    }
}

fn yes() -> bool {
    true
}

/// Settings for `frametv serve`, changed from the web page's settings dialog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerSettings {
    /// Admin password as `salt$sha256hex`. Delete this line to reset a forgotten password.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_password: Option<String>,
    /// Base URL for share links and QR codes, e.g. `http://192.168.1.10:3273`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    /// Advertise each TV as a LocalSend target.
    #[serde(default = "yes")]
    pub localsend: bool,
    /// PIN LocalSend senders must enter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub localsend_pin: Option<String>,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self { admin_password: None, public_url: None, localsend: true, localsend_pin: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TvConfig {
    pub name: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// For Wake-on-LAN.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Panel resolution, e.g. `3840x2160`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    /// Folder that `frametv sync` mirrors to this TV's My Photos.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<PathBuf>,
    /// Default matte for uploads (`auto`, `none`, `shadowbox_polar`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portrait_matte: Option<String>,
    /// Default fit for uploads (`auto`, `cover`, `contain`, `blur`, `original`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fit: Option<String>,
}

impl TvConfig {
    pub fn new(name: impl Into<String>, host: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            host: host.into(),
            port: default_port(),
            token: None,
            mac: None,
            model: None,
            resolution: None,
            folder: None,
            matte: None,
            portrait_matte: None,
            fit: None,
        }
    }

    pub fn endpoint(&self, client_name: &str) -> Endpoint {
        Endpoint {
            host: self.host.clone(),
            port: self.port,
            token: self.token.clone(),
            client_name: client_name.to_string(),
        }
    }

    /// Panel resolution, defaulting to 4K.
    pub fn panel_size(&self) -> (u32, u32) {
        self.resolution
            .as_deref()
            .and_then(|r| r.split_once('x'))
            .and_then(|(w, h)| Some((w.trim().parse().ok()?, h.trim().parse().ok()?)))
            .unwrap_or((3840, 2160))
    }

    /// Screen size in inches, from the model number.
    pub fn size_inches(&self) -> Option<u32> {
        self.model.as_deref().and_then(screen_size_inches)
    }
}

/// Screen size in inches from a Samsung model number: the digits after its region letters,
/// e.g. `QN65LS03TAFXZA` and `QE55LS03BAUXXU` → 65 and 55, `UN32LS03` → 32.
pub fn screen_size_inches(model: &str) -> Option<u32> {
    let model = model.trim();
    let digits_at = model.find(|c: char| c.is_ascii_digit())?;
    if !(1..=3).contains(&digits_at) || !model[..digits_at].chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let digits: String = model[digits_at..].chars().take_while(char::is_ascii_digit).collect();
    let size: u32 = digits.parse().ok()?;
    (20..=120).contains(&size).then_some(size)
}

impl Config {
    /// Load from `path`; a missing file is an empty config.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|e| Error::Other(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).map_err(|e| Error::Other(format!("serializing config: {e}")))?;
        write_atomic(path, text.as_bytes())
    }

    /// Find a TV by name (case-insensitive) or host.
    pub fn find(&self, key: &str) -> Option<&TvConfig> {
        self.tvs.iter().find(|t| t.name.eq_ignore_ascii_case(key) || t.host == key)
    }

    pub fn find_mut(&mut self, key: &str) -> Option<&mut TvConfig> {
        self.tvs.iter_mut().find(|t| t.name.eq_ignore_ascii_case(key) || t.host == key)
    }

    /// Insert or replace (matched by name, then host).
    pub fn upsert(&mut self, tv: TvConfig) {
        if let Some(pos) = self.tvs.iter().position(|t| t.name.eq_ignore_ascii_case(&tv.name) || t.host == tv.host) {
            self.tvs[pos] = tv;
        } else {
            self.tvs.push(tv);
        }
    }

    pub fn remove(&mut self, key: &str) -> Option<TvConfig> {
        let pos = self.tvs.iter().position(|t| t.name.eq_ignore_ascii_case(key) || t.host == key)?;
        if self.default_tv.as_deref().is_some_and(|d| d.eq_ignore_ascii_case(&self.tvs[pos].name)) {
            self.default_tv = None;
        }
        Some(self.tvs.remove(pos))
    }
}

fn project_dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("", "", "samsung-frame")
}

/// If set, everything (config, sync state, caches) lives under this directory. Handy for
/// containers: `FRAMETV_DATA_DIR=/data`.
pub const DATA_DIR_ENV: &str = "FRAMETV_DATA_DIR";

fn data_dir_override() -> Option<PathBuf> {
    std::env::var_os(DATA_DIR_ENV).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// `$FRAMETV_DATA_DIR/config.toml` if set, else `%APPDATA%\samsung-frame\config\config.toml` on
/// Windows, `~/.config/samsung-frame/config.toml` on Linux.
pub fn default_config_path() -> PathBuf {
    if let Some(d) = data_dir_override() {
        return d.join("config.toml");
    }
    project_dirs().map_or_else(|| PathBuf::from("frametv.toml"), |d| d.config_dir().join("config.toml"))
}

/// Where per-TV sync state lives.
pub fn default_state_dir() -> PathBuf {
    if let Some(d) = data_dir_override() {
        return d.join("state");
    }
    project_dirs().map_or_else(|| PathBuf::from("frametv-state"), |d| d.data_dir().join("state"))
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_size_from_model() {
        assert_eq!(screen_size_inches("QN65LS03TAFXZA"), Some(65));
        assert_eq!(screen_size_inches("QE55LS03BAUXXU"), Some(55));
        assert_eq!(screen_size_inches("UN32LS03"), Some(32));
        assert_eq!(screen_size_inches("QN85LS03D"), Some(85));
        assert_eq!(screen_size_inches("22_PONTUSM_FTV"), None);
        assert_eq!(screen_size_inches("Frame"), None);
    }

    #[test]
    fn roundtrip_and_lookup() {
        let mut cfg = Config::default();
        let mut tv = TvConfig::new("Living Room", "10.0.0.5");
        tv.token = Some("123".into());
        tv.resolution = Some("1920x1080".into());
        cfg.upsert(tv);
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        let tv = back.find("living room").unwrap();
        assert_eq!(tv.port, 8002);
        assert_eq!(tv.panel_size(), (1920, 1080));
        assert!(back.find("10.0.0.5").is_some());
    }
}
