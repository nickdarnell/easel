//! The JSON the web API speaks, in both directions.
//!
//! `cargo test` writes each of these as TypeScript into `web/src/api/` (ts-rs), and the web UI
//! imports those, so `npm run build` fails if the UI and the server disagree. CI checks that the
//! committed files are up to date.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use samsung_frame::image_prep::Fit;
use samsung_frame::sync::Framing;

// ── responses ────────────────────────────────────────────────────────────────────────────────

/// The body of every failed request.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct ErrorBody {
    pub error: String,
}

/// A LocalSend target this server advertises.
#[derive(Clone, Serialize, TS)]
#[ts(export)]
pub struct LocalSendTarget {
    pub alias: String,
    pub port: u16,
}

/// `GET /api/info`: what this visitor may do, and what the server supports.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct Info {
    /// Whether this request carried the admin password (or none is set).
    pub admin: bool,
    pub password_set: bool,
    /// The address guests should use, if one is set.
    pub public_url: Option<String>,
    pub localsend: Vec<LocalSendTarget>,
    /// Whether the server can read HEIC photos itself (otherwise the browser converts them).
    pub heif: bool,
    pub version: String,
}

/// `GET /api/settings` (admin): the server-wide settings.
#[allow(clippy::struct_excessive_bools)]
#[derive(Serialize, TS)]
#[ts(export)]
pub struct Settings {
    pub password_set: bool,
    pub public_url: Option<String>,
    pub localsend: bool,
    pub localsend_pin: Option<String>,
    pub show_on_receive: bool,
    pub localsend_targets: Vec<LocalSendTarget>,
    /// Why LocalSend couldn't start, if it couldn't.
    pub localsend_error: Option<String>,
    pub in_docker: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum Power {
    On,
    /// The screen is off but the TV answers (and can usually be woken).
    Standby,
    /// Not answering at all.
    Off,
}

impl Power {
    /// From the `PowerState` the TV reports over REST.
    pub fn from_tv(state: &str) -> Self {
        if state.eq_ignore_ascii_case("standby") { Power::Standby } else { Power::On }
    }
}

/// `GET /api/tvs`: a paired TV.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct Tv {
    pub name: String,
    pub host: String,
    pub model: Option<String>,
    /// Screen size in inches (from the model number).
    pub size_inches: Option<u32>,
    /// Panel size in pixels, landscape.
    pub resolution: (u32, u32),
    /// Folder on the server that this TV syncs from and saves received photos in.
    pub folder: Option<String>,
    pub matte: Option<String>,
    pub portrait_matte: Option<String>,
    pub fit: Option<Fit>,
    pub power: Power,
    /// Whether the server knows its MAC address, so it can be woken over the network.
    pub can_wake: bool,
}

/// `GET /api/tvs/{tv}/status`: what the TV last reported (the server keeps it up to date and
/// sends a `status` event when it changes). Each part is `null` if the TV didn't answer that.
#[derive(Clone, Default, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct Status {
    /// False until the TV has answered at all.
    pub loaded: bool,
    pub artmode: Option<bool>,
    pub current: Option<Current>,
    pub slideshow: Option<Slideshow>,
    /// A QR code card is up on the TV (see `POST .../share-card`).
    pub sharing: bool,
}

#[derive(Clone, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct Current {
    pub content_id: String,
}

#[derive(Clone, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct Slideshow {
    /// Minutes between photos, or `null` when it's off.
    pub minutes: Option<u32>,
    pub shuffle: bool,
    /// Playing only the favorites, not all of My Photos.
    pub favorites: bool,
}

/// `GET /api/tvs/{tv}/photos`: one photo on the TV, newest first.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct Photo {
    pub id: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// As the TV reports it, e.g. `2026:05:01 10:00:00`.
    pub date: Option<String>,
    pub matte: Option<String>,
    pub portrait_matte: Option<String>,
    /// Sent from this server (so it knows the original file).
    pub mine: bool,
    /// The original's file name.
    pub source: Option<String>,
    /// How it was framed, if someone chose explicitly.
    pub framing: Option<Framing>,
    /// Whether it can be reframed: the original file is still there.
    pub reframe: bool,
    /// Whether the thumbnail is cached. Others arrive later, announced by a `thumbs` event.
    pub thumb: bool,
}

/// `GET /api/tvs/{tv}/photos`: the photos, and how current the list is.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct PhotoList {
    pub photos: Vec<Photo>,
    /// When the TV sent this list, in Unix milliseconds.
    #[ts(type = "number")]
    pub as_of: u64,
    /// Why the TV couldn't be reached, when it couldn't (so this is the last list it sent).
    pub offline: Option<String>,
    /// The TV hasn't sent a list yet; one is on its way (announced by a `changed` event).
    pub loading: bool,
}

/// `POST /api/discover` (admin): a TV found on the network.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct Discovered {
    pub host: String,
    pub name: String,
    pub model: Option<String>,
    /// Whether it says it's a Frame (has art mode).
    pub frame: bool,
    pub power: Power,
    pub paired: bool,
}

/// `POST /api/tvs` (admin): a newly paired TV.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct Paired {
    pub name: String,
    pub api_version: Option<String>,
    pub photos: Option<u32>,
    pub frame: bool,
}

/// `POST /api/tvs/{tv}/upload`: a photo that's now on the TV.
#[derive(Clone, Serialize, TS)]
#[ts(export)]
pub struct Uploaded {
    pub content_id: String,
    pub name: String,
    /// It was already there with the same framing, so nothing was sent.
    pub duplicate: bool,
    pub shown: bool,
}

/// `POST /api/tvs/{tv}/photos/{id}/reframe` (admin): the reframed photo replaces the old one.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct Reframed {
    pub content_id: String,
    pub replaced: String,
    pub matte: String,
}

/// `DELETE /api/tvs/{tv}/photos/{id}` (admin).
#[derive(Serialize, TS)]
#[ts(export)]
pub struct Deleted {
    pub deleted: String,
    /// Where the original went, if it came from the TV's sync folder (so sync won't re-add it).
    pub moved_source_to: Option<String>,
}

/// `POST /api/tvs/{tv}/share-card` (admin): the QR code image now on the TV. `DELETE` the same
/// path to take it down and go back to the photos.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct ShareCard {
    pub content_id: String,
}

// ── live events (`GET /api/events`) ─────────────────────────────────────────────────────────

/// Sent to every open page as server-sent events, one JSON object each.
#[derive(Clone, Serialize, TS)]
#[serde(tag = "type", rename_all = "kebab-case")]
#[ts(export)]
pub enum ServerEvent {
    /// The TV's status changed.
    Status {
        tv: String,
        status: Status,
    },
    /// Thumbnails that are now cached.
    Thumbs {
        tv: String,
        ids: Vec<String>,
    },
    Uploading {
        tv: String,
        name: String,
    },
    Uploaded {
        tv: String,
        #[serde(flatten)]
        photo: Uploaded,
    },
    /// The TV's photos or settings changed.
    Changed {
        tv: String,
    },
    /// Something the TV itself reported (art mode, slideshow, selection changes, ...).
    TvEvent {
        tv: String,
        event: String,
        #[ts(type = "unknown")]
        data: Value,
    },
    TvsChanged,
    SettingsChanged,
    SyncStart {
        tv: String,
        upload: u32,
        delete: u32,
    },
    SyncProgress {
        tv: String,
        index: u32,
        total: u32,
        name: Option<String>,
    },
    SyncDone {
        tv: String,
        uploaded: u32,
        deleted: u32,
        failed: u32,
    },
    Error {
        tv: String,
        message: String,
    },
}

// ── request bodies ──────────────────────────────────────────────────────────────────────────

/// `PUT /api/settings` (admin). Fields left out stay as they are; empty strings clear.
#[derive(Deserialize, TS)]
#[ts(export, optional_fields)]
pub struct SettingsUpdate {
    /// The new admin password; empty removes it.
    pub password: Option<String>,
    pub public_url: Option<String>,
    pub localsend: Option<bool>,
    pub localsend_pin: Option<String>,
    pub show_on_receive: Option<bool>,
}

/// `POST /api/tvs` (admin): pair a TV. Someone has to press Allow on it.
#[derive(Deserialize, TS)]
#[ts(export)]
pub struct AddTv {
    pub host: String,
    #[ts(optional = nullable)]
    pub name: Option<String>,
}

/// `PATCH /api/tvs/{tv}` (admin). Fields left out stay as they are; empty strings clear.
#[derive(Deserialize, TS)]
#[ts(export, optional_fields)]
pub struct TvUpdate {
    pub matte: Option<String>,
    pub portrait_matte: Option<String>,
    /// A [`Fit`], or empty for automatic.
    pub fit: Option<String>,
    /// Folder (on the machine running the server) to sync from and save received photos in.
    pub folder: Option<String>,
}

/// `POST /api/tvs/{tv}/photos/{id}/matte` (admin).
#[derive(Deserialize, TS)]
#[ts(export)]
pub struct MatteUpdate {
    pub matte: String,
    #[ts(optional = nullable)]
    pub portrait: Option<String>,
}

/// `POST .../favorite` and `POST .../artmode` (admin).
#[derive(Deserialize, TS)]
#[ts(export)]
pub struct Toggle {
    pub on: bool,
}

/// `POST /api/tvs/{tv}/slideshow` (admin).
#[derive(Deserialize, TS)]
#[ts(export, optional_fields)]
pub struct SlideshowUpdate {
    /// Minutes between photos; left out or 0 turns the slideshow off.
    pub minutes: Option<u32>,
    /// Defaults to true.
    pub shuffle: Option<bool>,
    /// Play only the favorites instead of all of My Photos.
    pub favorites: Option<bool>,
}

/// `POST /api/tvs/{tv}/photos/delete` (admin): the photos to act on.
#[derive(Deserialize, TS)]
#[ts(export)]
pub struct PhotoIds {
    pub ids: Vec<String>,
}

/// `POST /api/tvs/{tv}/sync` (admin): sync the TV with its folder. Progress arrives as events.
#[derive(Deserialize, Default, TS)]
#[ts(export, optional_fields)]
pub struct SyncRequest {
    /// Also remove photos from the TV whose files were deleted from the folder.
    pub delete: Option<bool>,
}

/// `POST /api/tvs/{tv}/share-card` (admin): show a QR code for `url` on the TV.
#[derive(Deserialize, TS)]
#[ts(export)]
pub struct ShareCardRequest {
    pub url: String,
}
