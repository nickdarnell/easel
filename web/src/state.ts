import { computed, reactive, ref, watch } from "vue";
import type {
  Change,
  ChangeOp,
  Filter,
  Info,
  MatteList,
  MatteUpdate,
  Photo,
  PhotoIds,
  PhotoList,
  Settings,
  SlideshowUpdate,
  Status,
  KeyPress,
  Toggle,
  Tv,
  TvQueue,
} from "./types";
import { toast } from "./toasts";
import { enc, errorMessage, sleep, store } from "./util";

const guestMatch = location.pathname.match(/^\/tv\/(.+)$/);

export const S = reactive({
  info: null as Info | null,
  tvs: [] as Tv[],
  tv: null as string | null,
  status: null as Status | null,
  statusErr: null as string | null,
  photos: null as Photo[] | null,
  photosErr: null as string | null,
  /** Set when the TV couldn't be reached, so `photos` is the last list it sent. */
  photosOffline: null as { reason: string; asOf: number } | null,
  filter: "all" as Filter,
  mattes: null as MatteList | null,
  pw: store.get("pw", ""),
  /** Set on `/tv/<name>`: the guest page for one TV. */
  guestTv: guestMatch ? decodeURIComponent(guestMatch[1]!) : null,
  /** Changes waiting to be made on the TV, and what it's being asked right now. */
  queue: null as TvQueue | null,
});

/**
 * Changes sent to the TV that haven't shown up in what it reports yet. Controls show the new
 * value with a spinner meanwhile, rather than snapping back to the old one.
 */
export const pending = reactive({
  art: null as boolean | null,
  slideshow: null as SlideshowChoice | null,
  share: null as "showing" | "ending" | null,
  /** What's happening to the selected photos, e.g. "Deleting 3…". */
  photos: null as string | null,
  /** The photo being put on the screen. */
  show: null as string | null,
});

/** Photos ticked in the grid, for acting on several at once. */
export const selection = reactive({ on: false, ids: new Set<string>() });

export interface SlideshowChoice {
  /** 0 is off. */
  minutes: number;
  favorites: boolean;
  shuffle: boolean;
}

export const curTv = computed(() => S.tvs.find((t) => t.name === S.tv));
/** Whether the selected TV takes requests now, or the server is getting it to (requests wait
 * for that rather than fail). Some TVs say they're in standby while showing art. */
export const tvReady = computed(() => {
  const tv = curTv.value;
  if (!tv) return false;
  return tv.power === "on" || !!S.status?.activity || (tv.power === "standby" && S.status?.artmode === true);
});
/** Admin controls are shown to admins, but never on the guest page. */
export const isAdmin = computed(() => !!S.info?.admin && !S.guestTv);

// ── dialogs ──────────────────────────────────────────────────────────
// One dialog at a time, like a single <dialog> element: opening another replaces it.
export type Dialog =
  | { kind: "photo"; id: string }
  | { kind: "tvSettings" }
  | { kind: "appSettings"; settings: Settings }
  | { kind: "addTv" }
  | { kind: "share" }
  | { kind: "password" }
  | { kind: "composer" }
  | { kind: "queue" }
  | { kind: "remote" };

export const dialog = ref<(Dialog & { key: number }) | null>(null);
let dialogKey = 0;

export function openDialog(d: Dialog) {
  dialog.value = { ...d, key: ++dialogKey };
}

/** Called when a dialog closes; ignores dialogs that were already replaced. */
export function dialogClosed(key: number) {
  if (dialog.value?.key === key) dialog.value = null;
}

export function closeDialog() {
  dialog.value = null;
}

export function askPassword() {
  if (dialog.value?.kind !== "password") openDialog({ kind: "password" });
}

export async function openAppSettings() {
  try {
    openDialog({ kind: "appSettings", settings: await api<Settings>("/api/settings") });
  } catch (e) {
    toast(errorMessage(e), true);
  }
}

// ── API ──────────────────────────────────────────────────────────────
export interface ApiOptions {
  method?: string;
  json?: unknown;
  body?: BodyInit;
  headers?: Record<string, string>;
}

/** Fetch an API path with the admin password. JSON comes back parsed; other bodies as the Response. */
export async function api<T = unknown>(path: string, opts: ApiOptions = {}): Promise<T> {
  const headers: Record<string, string> = { ...opts.headers };
  if (S.pw) headers.Authorization = "Bearer " + S.pw;
  let body = opts.body;
  if (opts.json !== undefined) {
    headers["Content-Type"] = "application/json";
    body = JSON.stringify(opts.json);
  }
  const r = await fetch(path, { method: opts.method || (body ? "POST" : "GET"), headers, body });
  if (r.status === 401) {
    askPassword();
    throw new Error("That needs the admin password");
  }
  if (!r.ok) {
    let msg = r.statusText;
    try {
      msg = ((await r.json()) as { error?: string }).error || msg;
    } catch {
      // Not JSON: keep the status text.
    }
    throw new Error(msg);
  }
  if (r.status === 202 || r.status === 204) return null as T;
  return ((r.headers.get("content-type") || "").includes("json") ? r.json() : r) as Promise<T>;
}

export const tvPath = (tv: string, rest = "") => `/api/tvs/${enc(tv)}${rest}`;
/** Bumped for a photo when its preview arrives, so pages showing it load it again. */
export const thumbRev = reactive(new Map<string, number>());
/** Photos whose preview didn't load in the grid (until it arrives). */
export const brokenThumbs = reactive(new Set<string>());
export const thumbUrl = (tv: string, id: string) => {
  const rev = thumbRev.get(id);
  return tvPath(tv, `/thumb/${enc(id)}`) + (rev ? `?rev=${rev}` : "");
};

/** Run an action, showing any error as a toast. */
export async function act(fn: () => Promise<unknown>) {
  try {
    await fn();
  } catch (e) {
    toast(errorMessage(e), true);
  }
}

// ── loading ──────────────────────────────────────────────────────────
export async function loadInfo() {
  S.info = await api<Info>("/api/info");
}

export async function loadTvs() {
  S.tvs = await api<Tv[]>("/api/tvs");
  if (S.guestTv) {
    const want = S.guestTv.toLowerCase();
    S.tv = S.tvs.find((t) => t.name.toLowerCase() === want)?.name ?? null;
  } else if (!S.tvs.some((t) => t.name === S.tv)) {
    const want = decodeURIComponent(location.hash.slice(1)) || store.get("tv", "");
    S.tv = (S.tvs.find((t) => t.name === want) || S.tvs[0])?.name ?? null;
  }
}

// Load a TV's photos when it's selected, and again whenever it wakes up.
watch(
  () => [S.tv, curTv.value?.power],
  () => loadTv(),
);

export async function loadTv() {
  const tv = curTv.value;
  if (!tv) return;
  // The server answers both from what it last heard (so photos show even while the TV is off),
  // and asks the TV in the background, waking it if need be.
  await Promise.all([loadStatus(), loadPhotos(), loadQueue()]);
}

export async function loadQueue() {
  const name = S.tv;
  if (!name || !isAdmin.value) return;
  try {
    const q = await api<TvQueue>(tvPath(name, "/queue"));
    if (name === S.tv) S.queue = q;
  } catch {
    // Shown again on the next event.
  }
}

export async function loadStatus() {
  const name = S.tv;
  if (!name) return;
  try {
    const s = await api<Status>(tvPath(name, "/status"));
    if (name === S.tv) {
      S.status = s;
      S.statusErr = null;
    }
  } catch (e) {
    if (name === S.tv) S.statusErr = errorMessage(e);
  }
}

export async function loadPhotos() {
  const name = S.tv;
  if (!name) return;
  try {
    const p = await api<PhotoList>(tvPath(name, "/photos" + (S.filter === "favorites" ? "?category=favorites" : "")));
    if (name === S.tv) {
      // Still on its way from the TV: a `changed` event says when it's here.
      S.photos = p.loading ? null : p.photos;
      S.photosOffline = p.offline ? { reason: p.offline, asOf: p.as_of } : null;
      S.photosErr = null;
    }
  } catch (e) {
    if (name === S.tv) S.photosErr = errorMessage(e);
  }
}

export function selectTv(name: string) {
  if (name === S.tv) return;
  endSelection();
  S.tv = name;
  S.status = S.photos = S.statusErr = S.photosErr = S.photosOffline = S.mattes = S.queue = null;
  S.filter = "all";
  store.set("tv", name);
  history.replaceState(null, "", "#" + enc(name));
}

export function setFilter(f: Filter) {
  const prev = S.filter;
  endSelection();
  S.filter = f;
  if ((prev === "favorites") !== (f === "favorites")) {
    S.photos = null;
    loadPhotos();
  }
}

export async function getMattes(): Promise<MatteList> {
  if (!S.mattes && S.tv) {
    S.mattes = await api<MatteList>(tvPath(S.tv, "/mattes")).catch(() => ({ types: [], colors: [] }));
  }
  return S.mattes ?? { types: [], colors: [] };
}

export function thumbsArrived(ids: string[]) {
  for (const id of ids) {
    const p = S.photos?.find((x) => x.id === id);
    if (p) p.thumb = true;
    brokenThumbs.delete(id);
    thumbRev.set(id, (thumbRev.get(id) ?? 0) + 1);
  }
}

// ── waiting changes ──────────────────────────────────────────────────
/** Changes to the selected TV that haven't been made yet (including ones it turned down). */
export const changes = computed<Change[]>(() => S.queue?.changes ?? []);
type Op<K extends ChangeOp["kind"]> = Extract<ChangeOp, { kind: K }>;
const ofKind = <K extends ChangeOp["kind"]>(kind: K) =>
  changes.value.filter((c): c is Change & { op: Op<K> } => c.op.kind === kind);

/** Photos waiting to be sent. */
export const waitingUploads = computed(() => ofKind("upload"));
/** Photos waiting to be deleted. */
export const waitingDeletes = computed(() => new Set(ofKind("delete").flatMap((c) => c.op.ids)));
/** The slideshow as it'll be once the last waiting change to it is made. */
export const waitingSlideshow = computed<SlideshowChoice | null>(() => {
  const c = ofKind("slideshow").at(-1);
  return c && c.state !== "failed"
    ? { minutes: c.op.minutes ?? 0, shuffle: c.op.shuffle, favorites: c.op.favorites }
    : null;
});
export const failedSlideshow = computed(() => {
  const c = ofKind("slideshow").at(-1);
  return c?.state === "failed" ? c : null;
});

/** The waiting changes to one photo (matte, favorite, reframe, delete). */
export function changesTo(id: string): Change[] {
  return changes.value.filter(
    (c) => ("id" in c.op && c.op.id === id) || (c.op.kind === "delete" && c.op.ids.includes(id)),
  );
}

/** The photo's matte, as it'll be once waiting changes are made. */
export function wantedMatte(p: Photo): string | null {
  const c = ofKind("matte")
    .filter((c) => c.op.id === p.id)
    .at(-1);
  return c ? c.op.matte : p.matte;
}

/** Try a change again now (waking the TV if need be). */
export const retryChange = (id: number) =>
  act(async () => {
    await api(tvPath(S.tv!, `/queue/${id}`), { method: "POST" });
  });

/** Give up on a change. */
export const dropChange = (id: number) =>
  act(async () => {
    await api(tvPath(S.tv!, `/queue/${id}`), { method: "DELETE" });
  });

/** Press a button on the TV's remote (held down for `holdMs`, if given). */
export const pressKey = (key: string, holdMs?: number) =>
  act(async () => {
    await api(tvPath(S.tv!, "/key"), { json: { key, hold_ms: holdMs ?? null } satisfies KeyPress });
  });

const photos = (n: number) => `${n} ${n === 1 ? "photo" : "photos"}`;

// ── TV actions ───────────────────────────────────────────────────────
/** Wait up to `seconds` for the TV's status to show `done` (it can take a few seconds to apply
 * things). The server pushes status changes; asking now and then makes it check again. */
async function settle(done: () => boolean, seconds = 12) {
  const end = Date.now() + seconds * 1000;
  let ask = 0;
  while (Date.now() < end) {
    if (done()) return true;
    if (Date.now() >= ask) {
      loadStatus();
      ask = Date.now() + 2400;
    }
    await sleep(300);
  }
  return done();
}

export async function toggleArt() {
  const on = S.status?.artmode === false;
  pending.art = on;
  await act(async () => {
    await api(tvPath(S.tv!, "/artmode"), { json: { on } satisfies Toggle });
    await settle(() => S.status?.artmode === on);
  });
  pending.art = null;
}

/** The slideshow as the TV reports it (minutes 0 when off). */
export const slideshow = computed<SlideshowChoice>(() => ({
  minutes: S.status?.slideshow?.minutes || 0,
  favorites: !!S.status?.slideshow?.favorites,
  shuffle: S.status?.slideshow?.shuffle ?? true,
}));

export async function setSlideshow(want: SlideshowChoice) {
  pending.slideshow = want;
  const was = slideshow.value.minutes;
  await act(async () => {
    await api(tvPath(S.tv!, "/slideshow"), {
      json: { minutes: want.minutes, shuffle: want.shuffle, favorites: want.favorites } satisfies SlideshowUpdate,
    });
    // Made whenever the TV answers: usually straight away.
    const made =
      tvReady.value &&
      (await settle(
        () => !!failedSlideshow.value || (!waitingSlideshow.value && slideshow.value.minutes === want.minutes),
        10,
      ));
    if (failedSlideshow.value)
      toast(failedSlideshow.value.error || "The TV could not apply the slideshow settings", true);
    else if (!made && waitingSlideshow.value) toast(`The slideshow will change when ${S.tv} answers`);
    else if (!want.minutes) toast("Slideshow stopped");
    else toast(was ? "Slideshow updated" : "Slideshow started");
  });
  pending.slideshow = null;
}

export async function showPhoto(id: string) {
  pending.show = id;
  await act(async () => {
    await api(tvPath(S.tv!, `/photos/${enc(id)}/show`), { method: "POST" });
    await settle(() => S.status?.current?.content_id === id, 8);
  });
  pending.show = null;
}

// ── sharing ──────────────────────────────────────────────────────────
export async function showShareCard(url: string) {
  pending.share = "showing";
  let ok = false;
  await act(async () => {
    await api(tvPath(S.tv!, "/share-card"), { json: { url } });
    await settle(() => !!S.status?.sharing, 5);
    ok = true;
  });
  pending.share = null;
  return ok;
}

/** Take the QR code down and go back to the photos (and the slideshow, if one was running). */
export async function endShare() {
  pending.share = "ending";
  await act(async () => {
    await api(tvPath(S.tv!, "/share-card"), { method: "DELETE" });
    await settle(() => !S.status?.sharing, 5);
    toast("Back to your photos");
  });
  pending.share = null;
}

// ── several photos at once ───────────────────────────────────────────
export function toggleSelected(id: string) {
  selection.on = true;
  if (!selection.ids.delete(id)) selection.ids.add(id);
}

export function endSelection() {
  selection.on = false;
  selection.ids.clear();
}

/** Ask for a change to each selected photo. They're made whenever the TV answers. */
async function eachSelected(fn: (id: string) => Promise<unknown>) {
  const ids = [...selection.ids];
  let done = 0;
  pending.photos = "Saving…";
  await act(async () => {
    for (const id of ids) {
      await fn(id);
      done++;
    }
  });
  pending.photos = null;
  return done;
}

export async function favoriteSelected(on: boolean) {
  const n = await eachSelected((id) =>
    api(tvPath(S.tv!, `/photos/${enc(id)}/favorite`), { json: { on } satisfies Toggle }),
  );
  if (n) toast(`${on ? "Adding" : "Removing"} ${photos(n)} ${on ? "to" : "from"} favorites`);
  endSelection();
}

export async function matteSelected(matte: string) {
  const n = await eachSelected((id) =>
    api(tvPath(S.tv!, `/photos/${enc(id)}/matte`), { json: { matte, portrait: null } satisfies MatteUpdate }),
  );
  if (n) toast(`Changing the matte on ${photos(n)}`);
}

export async function deleteSelected() {
  const ids = [...selection.ids];
  pending.photos = "Saving…";
  await act(async () => {
    await api(tvPath(S.tv!, "/photos/delete"), { json: { ids } satisfies PhotoIds });
    toast(`Deleting ${photos(ids.length)}`);
    endSelection();
  });
  pending.photos = null;
}

export function wake() {
  return act(async () => {
    await api(tvPath(S.tv!, "/wake"), { method: "POST" });
    toast("Waking it up…");
    for (let i = 0; i < 12; i++) {
      await sleep(2500);
      await loadTvs();
      if (curTv.value?.power === "on") return;
    }
    toast("It didn't wake up. Is “Power On with Mobile” enabled on the TV?", true);
  });
}

export const recheck = loadTvs;

export function shareUrl() {
  const base = S.info?.public_url || location.origin;
  return `${base}/tv/${enc(S.tv!)}`;
}

// ── dialogs with matte pickers ───────────────────────────────────────
// The mattes come in meanwhile (the TV may be off, and the server may not have them yet).
export function openPhoto(id: string) {
  if (isAdmin.value) getMattes();
  openDialog({ kind: "photo", id });
}

export function openTvSettings() {
  getMattes();
  openDialog({ kind: "tvSettings" });
}
