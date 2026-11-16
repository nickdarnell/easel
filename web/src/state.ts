import { computed, reactive, ref, watch } from "vue";
import type {
  Deleted,
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
  Toggle,
  Tv,
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
  showNow: store.get("showNow", true),
});

watch(
  () => S.showNow,
  (v) => store.set("showNow", v),
);

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
  | { kind: "composer" };

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
export const thumbUrl = (tv: string, id: string) => tvPath(tv, `/thumb/${enc(id)}`);

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
  await Promise.all([loadStatus(), loadPhotos()]);
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
  S.status = S.photos = S.statusErr = S.photosErr = S.photosOffline = S.mattes = null;
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
  }
}

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
    await settle(() => slideshow.value.minutes === want.minutes);
    if (!want.minutes) toast("Slideshow stopped");
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

async function eachSelected(label: string, fn: (id: string) => Promise<unknown>) {
  const ids = [...selection.ids];
  let done = 0;
  pending.photos = `${label} ${ids.length}…`;
  await act(async () => {
    for (const id of ids) {
      await fn(id);
      pending.photos = `${label} ${ids.length - ++done}…`;
    }
  });
  pending.photos = null;
  return done;
}

export async function favoriteSelected(on: boolean) {
  const n = await eachSelected(on ? "Adding" : "Removing", (id) =>
    api(tvPath(S.tv!, `/photos/${enc(id)}/favorite`), { json: { on } satisfies Toggle }),
  );
  if (n) toast(`${n} ${n === 1 ? "photo" : "photos"} ${on ? "added to" : "removed from"} favorites`);
  endSelection();
  if (S.filter === "favorites") loadPhotos();
}

export async function matteSelected(matte: string) {
  const n = await eachSelected("Changing", async (id) => {
    await api(tvPath(S.tv!, `/photos/${enc(id)}/matte`), { json: { matte, portrait: null } satisfies MatteUpdate });
    const p = S.photos?.find((x) => x.id === id);
    if (p) p.matte = matte;
  });
  if (n) toast(`Matte changed on ${n} ${n === 1 ? "photo" : "photos"}`);
}

export async function deleteSelected() {
  const ids = [...selection.ids];
  pending.photos = `Deleting ${ids.length}…`;
  await act(async () => {
    const r = await api<Deleted[]>(tvPath(S.tv!, "/photos/delete"), { json: { ids } satisfies PhotoIds });
    const gone = new Set(r.map((d) => d.deleted));
    if (S.photos) S.photos = S.photos.filter((p) => !gone.has(p.id));
    const moved = r.filter((d) => d.moved_source_to).length;
    toast(
      `Deleted ${r.length} ${r.length === 1 ? "photo" : "photos"}` + (moved ? " (originals moved to .removed)" : ""),
    );
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

// ── opening dialogs that need the TV's mattes first ──────────────────
export async function openPhoto(id: string) {
  if (isAdmin.value) await getMattes();
  openDialog({ kind: "photo", id });
}

export async function openTvSettings() {
  if (curTv.value?.power === "on") await getMattes();
  openDialog({ kind: "tvSettings" });
}
