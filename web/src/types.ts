// Shapes of the JSON the server (src/bin/frametv/serve.rs) sends.

export type Power = "on" | "standby" | "off";

export interface LocalSendTarget {
  alias: string;
  port: number;
}

export interface Info {
  admin: boolean;
  password_set: boolean;
  public_url: string | null;
  localsend: LocalSendTarget[];
  heif: boolean;
  version: string;
}

export interface Settings {
  password_set: boolean;
  public_url: string | null;
  localsend: boolean;
  localsend_pin: string | null;
  show_on_receive: boolean;
  localsend_targets: LocalSendTarget[];
  localsend_error: string | null;
  in_docker: boolean;
}

export type Fit = "cover" | "contain" | "original";

export interface Tv {
  name: string;
  host: string;
  model: string | null;
  resolution: [number, number] | null;
  folder: string | null;
  matte: string | null;
  portrait_matte: string | null;
  fit: Fit | null;
  power: Power;
  can_wake: boolean;
}

export interface Status {
  artmode: boolean | null;
  current: { content_id: string } | null;
  slideshow: { minutes: number; shuffle: boolean } | null;
}

export interface Crop {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** How a photo sits on the screen. `matte` of `none`/`auto`/missing means the TV's default. */
export interface Framing {
  fit: Fit;
  crop?: Crop | null;
  matte?: string | null;
}

export interface Photo {
  id: string;
  width: number | null;
  height: number | null;
  date: string | null;
  matte: string | null;
  portrait_matte: string | null;
  mine: boolean;
  source: string | null;
  framing: Framing | null;
  reframe: boolean;
  thumb: boolean;
}

export interface Mattes {
  types: string[];
  colors: string[];
}

export interface Discovered {
  host: string;
  name: string;
  model: string | null;
  frame: boolean;
  power: Power;
  paired: boolean;
}

export interface Paired {
  name: string;
  photos: number | null;
  frame: boolean;
}

export interface UploadResult {
  duplicate?: boolean;
  shown?: boolean;
}

export type Filter = "all" | "mine" | "favorites";

/** Server-sent events from `/api/events`. */
export type ServerEvent =
  | { type: "thumbs"; tv: string; ids: string[] }
  | { type: "uploaded" | "changed"; tv: string }
  | { type: "sync-done"; tv: string; uploaded: number; deleted: number; failed: number }
  | { type: "sync-start"; tv: string; upload: number }
  | { type: "sync-progress"; tv: string; index: number; total: number }
  | { type: "tv-event"; tv: string }
  | { type: "tvs-changed" | "settings-changed"; tv?: undefined }
  | { type: "error"; tv: string; message: string }
  | { type: "uploading"; tv: string };
