//! What each TV last reported, kept in memory and on disk (`/config/cache/<tv>/`), so pages
//! still load, from slightly old data, while a TV is off, asleep or slow to answer.

use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// A value the TV reported, and when.
#[derive(Clone)]
pub struct Snapshot<T> {
    pub value: T,
    /// When the TV reported it, in Unix milliseconds.
    pub at: u64,
    /// When this process last got it from the TV; `None` if it was read from disk.
    checked: Option<Instant>,
    /// Known to be out of date (something changed it), so it's only good as a fallback.
    pub stale: bool,
}

impl<T> Snapshot<T> {
    /// A value the TV just sent.
    pub fn now(value: T) -> Self {
        Self { value, at: now_ms(), checked: Some(Instant::now()), stale: false }
    }

    /// Whether it came from the TV within `max_age` and nothing has changed it since.
    pub fn is_fresh(&self, max_age: Duration) -> bool {
        !self.stale && self.checked.is_some_and(|c| c.elapsed() < max_age)
    }
}

#[derive(Serialize)]
struct Write<'a, T> {
    at: u64,
    value: &'a T,
}

#[derive(Deserialize)]
struct Read<T> {
    at: u64,
    value: T,
}

pub fn load<T: DeserializeOwned>(path: &Path) -> Option<Snapshot<T>> {
    let r: Read<T> = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    Some(Snapshot { value: r.value, at: r.at, checked: None, stale: false })
}

/// Best effort: the cache is only a fallback, so failing to write it isn't an error.
pub fn save<T: Serialize>(path: &Path, snap: &Snapshot<T>) {
    let result = (|| -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(&Write { at: snap.at, value: &snap.value })?)?;
        std::fs::rename(tmp, path)
    })();
    if let Err(e) = result {
        tracing::warn!("caching {}: {e}", path.display());
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_ages() {
        let dir = std::env::temp_dir().join(format!("easel-cache-{}", std::process::id()));
        let path = dir.join("tv").join("photos.json");
        let snap = Snapshot::now(vec!["MY_F0001".to_string()]);
        assert!(snap.is_fresh(Duration::from_secs(10)));
        save(&path, &snap);

        let back: Snapshot<Vec<String>> = load(&path).unwrap();
        assert_eq!(back.value, snap.value);
        assert_eq!(back.at, snap.at);
        // Read from disk: usable, but not confirmed by the TV in this process.
        assert!(!back.is_fresh(Duration::from_secs(10)));
        assert!(!back.stale);

        let mut changed = snap.clone();
        changed.stale = true;
        assert!(!changed.is_fresh(Duration::from_secs(10)));
        let _ = std::fs::remove_dir_all(dir);
    }
}
