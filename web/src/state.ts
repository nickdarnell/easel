import { computed, reactive, ref, watch } from "vue";
import type { Filter, Info, Mattes, Photo, Settings, Status, Tv } from "./types";
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
  filter: "all" as Filter,
  mattes: null as Mattes | null,
  pw: store.get("pw", ""),
  /** Set on `/tv/<name>`: the guest page for one TV. */
  guestTv: guestMatch ? decodeURIComponent(guestMatch[1]!) : null,
  showNow: store.get("showNow", true),
});

watch(
  () => S.showNow,
  (v) => store.set("showNow", v),
);

export const curTv = computed(() => S.tvs.find((t) => t.name === S.tv));
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
  if (!tv || tv.power !== "on") return;
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
    const p = await api<Photo[]>(tvPath(name, "/photos" + (S.filter === "favorites" ? "?category=favorites" : "")));
    if (name === S.tv) {
      S.photos = p;
      S.photosErr = null;
    }
  } catch (e) {
    if (name === S.tv) S.photosErr = errorMessage(e);
  }
}

export function selectTv(name: string) {
  if (name === S.tv) return;
  S.tv = name;
  S.status = S.photos = S.statusErr = S.photosErr = S.mattes = null;
  S.filter = "all";
  store.set("tv", name);
  history.replaceState(null, "", "#" + enc(name));
}

export function setFilter(f: Filter) {
  const prev = S.filter;
  S.filter = f;
  if ((prev === "favorites") !== (f === "favorites")) {
    S.photos = null;
    loadPhotos();
  }
}

export async function getMattes(): Promise<Mattes> {
  if (!S.mattes && S.tv) {
    S.mattes = await api<Mattes>(tvPath(S.tv, "/mattes")).catch(() => ({ types: [], colors: [] }));
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
export function toggleArt() {
  return act(async () => {
    await api(tvPath(S.tv!, "/artmode"), { json: { on: S.status?.artmode === false } });
    setTimeout(loadStatus, 1200);
  });
}

export function setSlideshow(minutes: number) {
  return act(async () => {
    await api(tvPath(S.tv!, "/slideshow"), { json: { minutes, shuffle: true } });
    toast(minutes ? "Slideshow on" : "Slideshow off");
  });
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
