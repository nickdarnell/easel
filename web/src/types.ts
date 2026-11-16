// The server's API types are generated from the Rust code (src/bin/frametv/api.rs) into ./api
// by `cargo test`; don't edit those by hand. This re-exports them, plus a few UI-only types.

export type * from "./api/AddTv";
export type * from "./api/Crop";
export type * from "./api/Current";
export type * from "./api/Deleted";
export type * from "./api/Discovered";
export type * from "./api/ErrorBody";
export type * from "./api/Fit";
export type * from "./api/Framing";
export type * from "./api/Info";
export type * from "./api/LocalSendTarget";
export type * from "./api/MatteList";
export type * from "./api/MatteUpdate";
export type * from "./api/Paired";
export type * from "./api/Photo";
export type * from "./api/Power";
export type * from "./api/Reframed";
export type * from "./api/ServerEvent";
export type * from "./api/Settings";
export type * from "./api/SettingsUpdate";
export type * from "./api/ShareCard";
export type * from "./api/ShareCardRequest";
export type * from "./api/Slideshow";
export type * from "./api/SlideshowUpdate";
export type * from "./api/Status";
export type * from "./api/SyncRequest";
export type * from "./api/Toggle";
export type * from "./api/Tv";
export type * from "./api/TvUpdate";
export type * from "./api/Uploaded";

/** The photo grid's filter. */
export type Filter = "all" | "mine" | "favorites";
