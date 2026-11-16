//! Changes waiting to be made on a TV. The page doesn't wait for a slow or stuck TV: changes
//! (photos to add or delete, mattes, favorites, the slideshow, reframing) are written down here
//! and made by a worker whenever the TV answers, so the TV ends up the way it was asked to be.
//!
//! The list is kept on disk, so a restart doesn't lose it. A newer change replaces an older
//! one it makes pointless (a second matte for the same photo, a new slideshow setting), and
//! deleting a photo drops the changes still waiting for it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use samsung_frame::sync::Framing;

use crate::api;
use crate::cache::now_ms;

/// After the TV fails to answer, the first wait before trying again; it doubles each time...
const RETRY_FIRST: Duration = Duration::from_secs(30);
/// ...up to this.
const RETRY_MOST: Duration = Duration::from_secs(5 * 60);

/// What to change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Op {
    /// Send a photo (already saved at `path`), and put it on the screen if `show`.
    Upload {
        name: String,
        path: PathBuf,
        hash: String,
        framing: Option<Framing>,
        show: bool,
        /// The file was saved for this upload (not already there), so dropping the upload
        /// removes it again.
        fresh: bool,
    },
    Delete {
        ids: Vec<String>,
    },
    Matte {
        id: String,
        matte: String,
        portrait: Option<String>,
    },
    Favorite {
        id: String,
        on: bool,
    },
    Slideshow {
        minutes: Option<u32>,
        shuffle: bool,
        favorites: bool,
    },
    Reframe {
        id: String,
        framing: Framing,
    },
}

impl Op {
    /// The photo this changes, if it's about one photo already on the TV.
    fn photo(&self) -> Option<&str> {
        match self {
            Op::Matte { id, .. } | Op::Favorite { id, .. } | Op::Reframe { id, .. } => Some(id),
            _ => None,
        }
    }

    /// Whether `self`, made later, makes `older` pointless.
    fn replaces(&self, older: &Op) -> bool {
        match (self, older) {
            (Op::Matte { id: a, .. }, Op::Matte { id: b, .. })
            | (Op::Favorite { id: a, .. }, Op::Favorite { id: b, .. })
            | (Op::Reframe { id: a, .. }, Op::Reframe { id: b, .. }) => a == b,
            (Op::Slideshow { .. }, Op::Slideshow { .. }) => true,
            (Op::Delete { ids }, older) => older.photo().is_some_and(|id| ids.iter().any(|i| i == id)),
            _ => false,
        }
    }

    pub fn describe(&self) -> String {
        let photos = |n: usize| if n == 1 { "1 photo".to_string() } else { format!("{n} photos") };
        match self {
            Op::Upload { name, show: true, .. } => format!("Add {name} and show it"),
            Op::Upload { name, .. } => format!("Add {name}"),
            Op::Delete { ids } => format!("Delete {}", photos(ids.len())),
            Op::Matte { matte, .. } => format!("Set a photo's matte to {}", pretty_matte(matte)),
            Op::Favorite { on: true, .. } => "Add a photo to favorites".into(),
            Op::Favorite { on: false, .. } => "Remove a photo from favorites".into(),
            Op::Slideshow { minutes: None | Some(0), .. } => "Stop the slideshow".into(),
            Op::Slideshow { minutes: Some(m), favorites, .. } => {
                let what = if *favorites { "favorites" } else { "all photos" };
                format!("Slideshow of {what}, every {}", pretty_minutes(*m))
            }
            Op::Reframe { .. } => "Reframe a photo".into(),
        }
    }

    fn to_api(&self) -> api::ChangeOp {
        match self.clone() {
            Op::Upload { name, show, .. } => api::ChangeOp::Upload { name, show },
            Op::Delete { ids } => api::ChangeOp::Delete { ids },
            Op::Matte { id, matte, portrait } => api::ChangeOp::Matte { id, matte, portrait },
            Op::Favorite { id, on } => api::ChangeOp::Favorite { id, on },
            Op::Slideshow { minutes, shuffle, favorites } => api::ChangeOp::Slideshow { minutes, shuffle, favorites },
            Op::Reframe { id, .. } => api::ChangeOp::Reframe { id },
        }
    }
}

fn pretty_matte(id: &str) -> String {
    if id == "none" { "none".into() } else { id.replace('_', " ") }
}

fn pretty_minutes(m: u32) -> String {
    match m {
        60 => "hour".into(),
        1440 => "day".into(),
        m if m % 60 == 0 => format!("{} hours", m / 60),
        m => format!("{m} minutes"),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Change {
    pub id: u64,
    pub op: Op,
    /// When it was asked for (ms since the epoch).
    pub queued: u64,
    #[serde(default)]
    pub attempts: u32,
    /// Why the last attempt failed.
    #[serde(default)]
    pub error: Option<String>,
    /// The TV turned it down: it waits for someone to retry or drop it.
    #[serde(default)]
    pub failed: bool,
    /// Being made right now.
    #[serde(skip)]
    pub working: bool,
    /// Not tried again before this (the TV didn't answer last time).
    #[serde(skip)]
    pub not_before: Option<Instant>,
    /// Someone just asked for it: the TV may be woken (or restarted) for it, like for anything
    /// done from the page. Retries by themselves only go ahead if the TV answers.
    #[serde(skip)]
    pub eager: bool,
}

impl Change {
    fn to_api(&self) -> api::Change {
        let retry_at = self.not_before.filter(|_| !self.working && !self.failed).map(|at| {
            let left = at.saturating_duration_since(Instant::now());
            now_ms() + u64::try_from(left.as_millis()).unwrap_or(0)
        });
        api::Change {
            id: self.id,
            op: self.op.to_api(),
            label: self.op.describe(),
            state: if self.working {
                api::ChangeState::Working
            } else if self.failed {
                api::ChangeState::Failed
            } else {
                api::ChangeState::Waiting
            },
            error: self.error.clone(),
            attempts: self.attempts,
            queued: self.queued,
            retry_at,
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    next_id: u64,
    changes: Vec<Change>,
}

/// One TV's waiting changes, oldest first.
pub struct Outbox {
    path: PathBuf,
    next_id: u64,
    changes: Vec<Change>,
}

/// Something taken off the list without being made, and what to tidy up for it.
pub struct Dropped(pub Change);

impl Dropped {
    /// Remove a photo saved only for an upload that's no longer happening.
    pub fn tidy(&self) {
        if let Op::Upload { path, fresh: true, .. } = &self.0.op {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl Outbox {
    pub fn load(path: &Path) -> Self {
        let saved: Saved = std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Outbox { path: path.to_path_buf(), next_id: saved.next_id.max(1), changes: saved.changes }
    }

    fn save(&self) {
        let saved = Saved { next_id: self.next_id, changes: self.changes.clone() };
        let write = || -> std::io::Result<()> {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = self.path.with_extension("tmp");
            std::fs::write(&tmp, serde_json::to_vec_pretty(&saved)?)?;
            std::fs::rename(tmp, &self.path)
        };
        if let Err(e) = write() {
            tracing::warn!("saving {}: {e}", self.path.display());
        }
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn to_api(&self) -> Vec<api::Change> {
        self.changes.iter().map(Change::to_api).collect()
    }

    /// Add a change, dropping older ones it makes pointless (unless they're being made right
    /// now). Returns its id and what was dropped.
    pub fn push(&mut self, op: Op) -> (u64, Vec<Dropped>) {
        let mut dropped = Vec::new();
        let mut kept = Vec::with_capacity(self.changes.len() + 1);
        for c in self.changes.drain(..) {
            if !c.working && op.replaces(&c.op) {
                dropped.push(Dropped(c));
            } else {
                kept.push(c);
            }
        }
        self.changes = kept;
        let id = self.next_id;
        self.next_id += 1;
        self.changes.push(Change {
            id,
            op,
            queued: now_ms(),
            attempts: 0,
            error: None,
            failed: false,
            working: false,
            not_before: None,
            eager: true,
        });
        self.save();
        (id, dropped)
    }

    /// The next change to make, marked as being made; or, if the next one has to wait, how
    /// long. `kicked`: try now even if it was going to wait.
    pub fn take_next(&mut self, kicked: bool) -> Result<Change, Option<Duration>> {
        let Some(c) = self.changes.iter_mut().find(|c| !c.failed && !c.working) else { return Err(None) };
        if !kicked && let Some(at) = c.not_before {
            let left = at.saturating_duration_since(Instant::now());
            if !left.is_zero() {
                return Err(Some(left));
            }
        }
        c.working = true;
        c.attempts += 1;
        let taken = c.clone();
        c.eager = false;
        Ok(taken)
    }

    /// It's done: take it off the list.
    pub fn done(&mut self, id: u64) {
        self.changes.retain(|c| c.id != id);
        self.save();
    }

    /// The TV didn't answer: try again later, waiting longer each time.
    pub fn later(&mut self, id: u64, error: String) {
        if let Some(c) = self.changes.iter_mut().find(|c| c.id == id) {
            c.working = false;
            c.error = Some(error);
            let doublings = c.attempts.saturating_sub(1).min(8);
            c.not_before = Some(Instant::now() + (RETRY_FIRST * 2u32.pow(doublings)).min(RETRY_MOST));
        }
        self.save();
    }

    /// The TV turned it down: keep it, for someone to retry or drop.
    pub fn failed(&mut self, id: u64, error: String) {
        if let Some(c) = self.changes.iter_mut().find(|c| c.id == id) {
            c.working = false;
            c.failed = true;
            c.error = Some(error);
        }
        self.save();
    }

    /// Someone asked for it to be tried again now. `false` if there's no such change.
    pub fn retry(&mut self, id: u64) -> bool {
        let Some(c) = self.changes.iter_mut().find(|c| c.id == id) else { return false };
        c.failed = false;
        c.not_before = None;
        c.eager = true;
        self.save();
        true
    }

    /// Try every change that's waiting now (the TV is answering again).
    pub fn wake_up(&mut self) -> bool {
        let mut any = false;
        for c in self.changes.iter_mut().filter(|c| !c.failed && !c.working) {
            any |= c.not_before.take().is_some();
        }
        any
    }

    /// Take a change off the list without making it. `Err` if it's being made right now.
    pub fn drop_change(&mut self, id: u64) -> Result<Option<Dropped>, ()> {
        let Some(pos) = self.changes.iter().position(|c| c.id == id) else { return Ok(None) };
        if self.changes[pos].working {
            return Err(());
        }
        let c = self.changes.remove(pos);
        self.save();
        Ok(Some(Dropped(c)))
    }

    /// A photo got a new id (it was reframed): changes still waiting for it follow it.
    pub fn renamed(&mut self, old: &str, new: &str) {
        for c in &mut self.changes {
            match &mut c.op {
                Op::Matte { id, .. } | Op::Favorite { id, .. } | Op::Reframe { id, .. } if id == old => {
                    *id = new.to_string();
                }
                Op::Delete { ids } => {
                    for id in ids.iter_mut().filter(|i| *i == old) {
                        *id = new.to_string();
                    }
                }
                _ => {}
            }
        }
        self.save();
    }

    /// The saved photo a waiting upload will send.
    pub fn upload_file(&self, id: u64) -> Option<PathBuf> {
        self.changes.iter().find(|c| c.id == id).and_then(|c| match &c.op {
            Op::Upload { path, .. } => Some(path.clone()),
            _ => None,
        })
    }

    /// Remove photos saved only for uploads that were dropped, unless another upload still
    /// waiting needs the same file.
    pub fn tidy(&self, dropped: &[Dropped]) {
        for d in dropped {
            let Op::Upload { path, .. } = &d.0.op else { continue };
            let needed = self.changes.iter().any(|c| matches!(&c.op, Op::Upload { path: p, .. } if p == path));
            if !needed {
                d.tidy();
            }
        }
    }

    /// A photo is gone from the TV: changes still waiting for it are pointless.
    pub fn forget(&mut self, gone: &[String]) -> Vec<Dropped> {
        let mut dropped = Vec::new();
        self.changes.retain(|c| {
            let pointless = !c.working && c.op.photo().is_some_and(|id| gone.iter().any(|g| g == id));
            if pointless {
                dropped.push(Dropped(c.clone()));
            }
            !pointless
        });
        if !dropped.is_empty() {
            self.save();
        }
        dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch folder, removed when dropped.
    struct Dir(PathBuf);

    impl Dir {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn outbox() -> (Dir, Outbox) {
        let dir = Dir(std::env::temp_dir().join(format!("easel-outbox-{}", uuid::Uuid::new_v4().simple())));
        let ob = Outbox::load(&dir.path().join("pending.json"));
        (dir, ob)
    }

    fn matte(id: &str, m: &str) -> Op {
        Op::Matte { id: id.into(), matte: m.into(), portrait: None }
    }

    #[test]
    fn newer_changes_replace_older_ones() {
        let (_d, mut ob) = outbox();
        ob.push(matte("a", "none"));
        ob.push(matte("b", "none"));
        ob.push(Op::Slideshow { minutes: Some(60), shuffle: true, favorites: false });
        let (_, dropped) = ob.push(matte("a", "flexible_polar"));
        assert_eq!(dropped.len(), 1);
        ob.push(Op::Slideshow { minutes: None, shuffle: true, favorites: false });
        let ops: Vec<_> = ob.changes.iter().map(|c| c.op.clone()).collect();
        assert_eq!(
            ops,
            [
                matte("b", "none"),
                matte("a", "flexible_polar"),
                Op::Slideshow { minutes: None, shuffle: true, favorites: false }
            ]
        );
        // Deleting a photo drops what was waiting for it.
        ob.push(Op::Delete { ids: vec!["a".into()] });
        assert_eq!(ob.changes.len(), 3);
        assert!(matches!(ob.changes.last().unwrap().op, Op::Delete { .. }));
    }

    #[test]
    fn a_change_being_made_is_left_alone() {
        let (_d, mut ob) = outbox();
        let (first, _) = ob.push(matte("a", "none"));
        assert_eq!(ob.take_next(false).unwrap().id, first);
        let (_, dropped) = ob.push(matte("a", "shadowbox_polar"));
        assert!(dropped.is_empty());
        assert!(ob.drop_change(first).is_err());
        assert_eq!(ob.changes.len(), 2);
    }

    #[test]
    fn waits_after_no_answer_and_survives_a_restart() {
        let (dir, mut ob) = outbox();
        let (id, _) = ob.push(matte("a", "none"));
        let c = ob.take_next(false).unwrap();
        assert!(c.eager);
        ob.later(id, "no answer".into());
        assert!(matches!(ob.take_next(false), Err(Some(_))));
        // A kick (the TV answered) tries it now, and only an explicit retry makes it eager.
        let c = ob.take_next(true).unwrap();
        assert!(!c.eager);
        ob.failed(id, "rejected".into());
        assert!(matches!(ob.take_next(true), Err(None)));
        let again = Outbox::load(&dir.path().join("pending.json"));
        assert_eq!(again.changes.len(), 1);
        assert!(again.changes[0].failed);
        assert!(ob.retry(id));
        assert!(ob.take_next(false).unwrap().eager);
        ob.done(id);
        assert!(ob.is_empty());
        assert!(ob.next_id > id);
    }

    #[test]
    fn follows_a_reframed_photo() {
        let (_d, mut ob) = outbox();
        ob.push(Op::Favorite { id: "old".into(), on: true });
        ob.renamed("old", "new");
        assert_eq!(ob.changes[0].op, Op::Favorite { id: "new".into(), on: true });
        assert_eq!(ob.forget(&["new".into()]).len(), 1);
        assert!(ob.is_empty());
    }
}
