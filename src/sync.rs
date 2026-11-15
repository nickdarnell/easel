//! Mirroring a local folder to a TV's "My Photos".
//!
//! Uploaded images are tracked per TV as `content_id -> {content hash, source path}`, so
//! renames and moves are free, duplicates upload once, and only images *we* uploaded are ever
//! deleted. Images added from the SmartThings app are left alone.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::art::{ArtClient, UploadOptions};
use crate::config::write_atomic;
use crate::image_prep::{self, PrepOptions, Prepared};
use crate::{Error, Result};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TvState {
    /// content_id -> what we uploaded there.
    #[serde(default)]
    pub uploads: BTreeMap<String, UploadRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadRecord {
    /// SHA-256 of the source file.
    pub hash: String,
    pub source: PathBuf,
    pub uploaded_at: String,
    /// How the photo was framed, if someone chose explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framing: Option<Framing>,
}

/// Someone's explicit choice of how a photo sits on the screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Framing {
    /// `cover` fills the screen (no matte); `contain` shows the whole photo.
    pub fit: image_prep::Fit,
    /// Part of the photo to keep, before fitting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<image_prep::Crop>,
    /// Matte id (`none`, `shadowbox_polar`, ...); `None` or `auto` uses the TV's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte: Option<String>,
}

impl Framing {
    pub fn apply(&self, settings: &mut UploadSettings) {
        settings.prep.fit = self.fit;
        settings.prep.crop = self.crop;
        if let Some(m) = self.matte.as_deref().filter(|m| !m.eq_ignore_ascii_case("auto")) {
            settings.matte = m.to_string();
            settings.portrait_matte = m.to_string();
        }
    }
}

impl TvState {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        write_atomic(path, &serde_json::to_vec_pretty(self)?)
    }

    pub fn record(&mut self, content_id: &str, hash: &str, source: &Path) {
        self.record_framed(content_id, hash, source, None);
    }

    pub fn record_framed(&mut self, content_id: &str, hash: &str, source: &Path, framing: Option<Framing>) {
        self.uploads.insert(
            content_id.to_string(),
            UploadRecord {
                hash: hash.to_string(),
                source: source.to_path_buf(),
                uploaded_at: chrono::Local::now().to_rfc3339(),
                framing,
            },
        );
    }

    pub fn content_for_hash(&self, hash: &str) -> Option<&str> {
        self.uploads.iter().find(|(_, r)| r.hash == hash).map(|(id, _)| id.as_str())
    }
}

/// State file for a TV, named after it.
pub fn state_path(state_dir: &Path, tv_name: &str) -> PathBuf {
    let safe: String = tv_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    state_dir.join(format!("{safe}.json"))
}

#[derive(Debug, Clone)]
pub struct LocalImage {
    pub path: PathBuf,
    pub hash: String,
}

pub fn hash_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Supported images under `dir` (recursively), sorted by path, duplicates (same content) removed.
pub fn scan_folder(dir: &Path) -> Result<Vec<LocalImage>> {
    if !dir.is_dir() {
        return Err(Error::Other(format!("{} is not a folder", dir.display())));
    }
    let mut paths: Vec<PathBuf> = walkdir::WalkDir::new(dir)
        .follow_links(true)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && image_prep::is_supported_path(e.path()))
        .map(|e| e.into_path())
        .collect();
    paths.sort();
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let hash = hash_file(&path)?;
        if seen.insert(hash.clone()) {
            out.push(LocalImage { path, hash });
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Default)]
pub struct SyncPlan {
    pub upload: Vec<LocalImage>,
    /// (content_id, original source) of our uploads whose file is gone locally.
    pub delete: Vec<(String, PathBuf)>,
    /// State records whose image no longer exists on the TV (deleted on the TV itself).
    pub forget: Vec<String>,
    pub unchanged: usize,
}

impl SyncPlan {
    pub fn is_empty(&self) -> bool {
        self.upload.is_empty() && self.delete.is_empty() && self.forget.is_empty()
    }
}

/// Work out what to do. `on_tv` = content ids currently in My Photos.
pub fn plan(local: &[LocalImage], state: &TvState, on_tv: &HashSet<String>, delete_removed: bool) -> SyncPlan {
    let forget: Vec<String> = state.uploads.keys().filter(|id| !on_tv.contains(*id)).cloned().collect();
    let live: HashMap<&str, (&str, &UploadRecord)> = state
        .uploads
        .iter()
        .filter(|(id, _)| on_tv.contains(*id))
        .map(|(id, r)| (r.hash.as_str(), (id.as_str(), r)))
        .collect();
    let local_hashes: HashSet<&str> = local.iter().map(|l| l.hash.as_str()).collect();

    let upload: Vec<LocalImage> = local.iter().filter(|l| !live.contains_key(l.hash.as_str())).cloned().collect();
    let delete = if delete_removed {
        let mut d: Vec<(String, PathBuf)> = state
            .uploads
            .iter()
            .filter(|(id, r)| on_tv.contains(*id) && !local_hashes.contains(r.hash.as_str()))
            .map(|(id, r)| (id.clone(), r.source.clone()))
            .collect();
        d.sort();
        d
    } else {
        Vec::new()
    };
    SyncPlan { unchanged: local.len() - upload.len(), upload, delete, forget }
}

/// Matte selection: `auto` uses no matte for images that fill the screen and
/// `shadowbox_polar` (a neutral white frame) for everything else.
pub fn resolve_matte(requested: &str, prepared: &Prepared, panel: (u32, u32)) -> String {
    if requested.eq_ignore_ascii_case("auto") {
        if prepared.fills(panel.0, panel.1) { "none".into() } else { "shadowbox_polar".into() }
    } else {
        requested.to_string()
    }
}

#[derive(Debug, Clone)]
pub struct UploadSettings {
    pub prep: PrepOptions,
    /// `auto` or a matte id.
    pub matte: String,
    pub portrait_matte: String,
}

#[derive(Debug, Clone)]
pub struct UploadResult {
    pub content_id: String,
    pub bytes: usize,
    pub width: u32,
    pub height: u32,
    pub reencoded: bool,
    pub cropped: bool,
    pub matte: String,
}

/// Prepare (off the async runtime) and upload one file.
pub async fn upload_file(art: &ArtClient, path: &Path, settings: &UploadSettings) -> Result<UploadResult> {
    let p = path.to_path_buf();
    let prep_opts = settings.prep.clone();
    let prepared = tokio::task::spawn_blocking(move || image_prep::prepare_file(&p, &prep_opts))
        .await
        .map_err(|e| Error::Other(format!("image task failed: {e}")))??;
    let panel = (settings.prep.width, settings.prep.height);
    let matte = resolve_matte(&settings.matte, &prepared, panel);
    let opts = UploadOptions {
        file_type: prepared.file_type.to_string(),
        matte: matte.clone(),
        portrait_matte: settings.portrait_matte.clone(),
        image_date: prepared.image_date.clone(),
        ..Default::default()
    };
    let content_id = art.upload(&prepared.data, &opts).await?;
    Ok(UploadResult {
        content_id,
        bytes: prepared.data.len(),
        width: prepared.width,
        height: prepared.height,
        reencoded: prepared.reencoded,
        cropped: prepared.cropped,
        matte,
    })
}

#[derive(Debug)]
pub enum SyncEvent<'a> {
    Uploading { index: usize, total: usize, path: &'a Path },
    Uploaded { path: &'a Path, result: &'a UploadResult },
    UploadFailed { path: &'a Path, error: &'a Error },
    Deleted { content_id: &'a str, source: &'a Path },
}

#[derive(Debug, Default, Clone)]
pub struct SyncOutcome {
    pub uploaded: Vec<String>,
    pub failed: usize,
    pub deleted: usize,
    pub forgotten: usize,
    pub unchanged: usize,
}

/// Carry out a plan, saving state after every change so an interrupted sync resumes cleanly.
pub async fn execute(
    art: &ArtClient,
    state: &mut TvState,
    state_file: &Path,
    plan: SyncPlan,
    settings: &UploadSettings,
    mut on_event: impl FnMut(SyncEvent<'_>),
) -> Result<SyncOutcome> {
    let mut outcome = SyncOutcome { unchanged: plan.unchanged, ..Default::default() };

    for id in &plan.forget {
        state.uploads.remove(id);
    }
    outcome.forgotten = plan.forget.len();
    if !plan.forget.is_empty() {
        state.save(state_file)?;
    }

    let total = plan.upload.len();
    for (index, img) in plan.upload.iter().enumerate() {
        on_event(SyncEvent::Uploading { index, total, path: &img.path });
        match upload_file(art, &img.path, settings).await {
            Ok(result) => {
                state.record(&result.content_id, &img.hash, &img.path);
                state.save(state_file)?;
                on_event(SyncEvent::Uploaded { path: &img.path, result: &result });
                outcome.uploaded.push(result.content_id);
            }
            // Losing the connection means every following upload fails too.
            Err(e @ (Error::Disconnected | Error::WebSocket(_))) => return Err(e),
            Err(e) => {
                on_event(SyncEvent::UploadFailed { path: &img.path, error: &e });
                outcome.failed += 1;
            }
        }
    }

    if !plan.delete.is_empty() {
        let ids: Vec<String> = plan.delete.iter().map(|(id, _)| id.clone()).collect();
        art.delete(&ids).await?;
        for (id, source) in &plan.delete {
            state.uploads.remove(id);
            on_event(SyncEvent::Deleted { content_id: id, source });
        }
        outcome.deleted = ids.len();
        state.save(state_file)?;
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(name: &str, hash: &str) -> LocalImage {
        LocalImage { path: PathBuf::from(name), hash: hash.into() }
    }

    fn state(entries: &[(&str, &str)]) -> TvState {
        let mut s = TvState::default();
        for (id, hash) in entries {
            s.record(id, hash, Path::new("x.jpg"));
        }
        s
    }

    #[test]
    fn plans_uploads_deletes_and_forgets() {
        let local = vec![img("a.jpg", "A"), img("b.jpg", "B"), img("c.jpg", "C")];
        // A is on the TV; D was ours but removed locally; E was deleted on the TV itself.
        let st = state(&[("MY_F1", "A"), ("MY_F2", "D"), ("MY_F3", "E")]);
        let on_tv: HashSet<String> = ["MY_F1", "MY_F2", "MY_F9"].iter().map(|s| s.to_string()).collect();

        let p = plan(&local, &st, &on_tv, true);
        let up: Vec<_> = p.upload.iter().map(|l| l.hash.as_str()).collect();
        assert_eq!(up, ["B", "C"]);
        assert_eq!(p.delete.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["MY_F2"]);
        assert_eq!(p.forget, ["MY_F3"]);
        assert_eq!(p.unchanged, 1);

        // MY_F9 (not ours) is never deleted, and nothing is deleted without the flag.
        let p = plan(&local, &st, &on_tv, false);
        assert!(p.delete.is_empty());
    }

    #[test]
    fn reupload_when_deleted_on_tv() {
        let local = vec![img("a.jpg", "A")];
        let st = state(&[("MY_F1", "A")]);
        let p = plan(&local, &st, &HashSet::new(), true);
        assert_eq!(p.upload.len(), 1);
        assert_eq!(p.forget, ["MY_F1"]);
    }

    #[test]
    fn state_file_names_are_safe() {
        assert_eq!(state_path(Path::new("s"), "Living Room/1"), Path::new("s").join("Living_Room_1.json"));
    }
}
