// State for the framing composer: choosing how photos sit on the TV before sending (or reframing one).
import { ref } from "vue";
import type { Photo, Tv } from "./types";
import { type Frameable, frameDefaults, framingOf, panelAspect, setFromCrop } from "./framing";
import { S, act, api, curTv, dialog, loadPhotos, loadStatus, openDialog, closeDialog, tvPath } from "./state";
import { toast } from "./toasts";
import { enqueue } from "./upload";
import { enc, errorMessage, isImage } from "./util";

export interface Item extends Frameable {
  name: string;
  /** The file being sent (not set when reframing). */
  file?: File;
  /** A ready-made image URL (reframing). */
  src?: string;
  /** Object URLs to revoke when the composer closes. */
  urls: string[];
  /** The preview shown on the stage, once loaded. */
  url?: string;
  error?: string;
  /** Sets the starting framing once the size is known (instead of the TV's defaults). */
  init?: (it: Item) => void;
}

export interface Composer {
  kind: "send" | "reframe";
  items: Item[];
  tvs: string[];
  tv: Tv | undefined;
  /** Panel aspect ratio. */
  pa: number;
  sel: number;
  photo?: Photo;
}

export const composer = ref<Composer | null>(null);

/** True while the send composer is open, so new photos join it. */
export const composing = () => dialog.value?.kind === "composer" && composer.value?.kind === "send";

const newItem = (name: string, extra: Partial<Item> = {}): Item => ({
  name,
  urls: [],
  matte: "shadowbox_polar",
  zoom: 1,
  cx: 0.5,
  cy: 0.5,
  ...extra,
});

export function openComposer(files: File[], tvs: string[]) {
  files = files.filter(isImage);
  if (!files.length) {
    toast("Those don't look like photos", true);
    return;
  }
  const items = files.map((file) => newItem(file.name, { file }));
  const c = composer.value;
  if (composing() && c) {
    const start = c.items.length;
    c.items.push(...items);
    c.items.slice(start).forEach(loadItem);
    return;
  }
  start({ kind: "send", items, tvs, tv: S.tvs.find((t) => t.name === tvs[0]) || curTv.value, pa: 0, sel: 0 });
}

export async function openReframe(p: Photo) {
  const tv = curTv.value;
  let url: string;
  try {
    const r = await api<Response>(tvPath(S.tv!, `/photos/${enc(p.id)}/original`));
    url = URL.createObjectURL(await r.blob());
  } catch (e) {
    toast(errorMessage(e), true);
    return;
  }
  const pa = panelAspect(tv);
  const it = newItem(p.source || "Photo", {
    src: url,
    urls: [url],
    init(it) {
      frameDefaults(it, tv);
      const f = p.framing;
      if (f) {
        it.mode = f.fit === "cover" ? "fill" : "whole";
        if (f.crop) setFromCrop(it, f.crop, pa);
        if (f.matte && f.matte !== "none" && f.matte !== "auto") it.matte = f.matte;
      } else if (p.matte) {
        it.mode = p.matte === "none" ? "fill" : "whole";
        if (p.matte !== "none") it.matte = p.matte;
      }
    },
  });
  start({ kind: "reframe", items: [it], tvs: [S.tv!], tv, photo: p, pa, sel: 0 });
}

function start(c: Composer) {
  c.pa = panelAspect(c.tv);
  composer.value = c;
  openDialog({ kind: "composer" });
  // Load through the reactive proxies so the stage updates as previews arrive.
  composer.value.items.forEach(loadItem);
}

/** Forget the composer's state (when its dialog closes). */
export function disposeComposer(c: Composer) {
  if (composer.value !== c) return;
  c.items.forEach((it) => it.urls.forEach((u) => URL.revokeObjectURL(u)));
  composer.value = null;
}

export function removeItem(c: Composer, i: number) {
  c.items[i]!.urls.forEach((u) => URL.revokeObjectURL(u));
  c.items.splice(i, 1);
  if (c.sel >= i && c.sel > 0) c.sel--;
}

const loadImage = (url: string) =>
  new Promise<HTMLImageElement>((res, rej) => {
    const im = new Image();
    im.onload = () => res(im);
    im.onerror = rej;
    im.src = url;
  });

async function loadItem(it: Item) {
  try {
    let url = it.src || URL.createObjectURL(it.file!);
    if (!it.src) it.urls.push(url);
    let im: HTMLImageElement;
    try {
      im = await loadImage(url);
    } catch {
      // Not viewable here (e.g. HEIC outside Safari): let the server make a preview.
      const r = await fetch("/api/preview", { method: "POST", body: it.file });
      if (!r.ok) throw new Error();
      url = URL.createObjectURL(await r.blob());
      it.urls.push(url);
      im = await loadImage(url);
    }
    it.iw = im.naturalWidth;
    it.ih = im.naturalHeight;
    if (it.init) it.init(it);
    else frameDefaults(it, composer.value?.tv);
    it.url = url;
  } catch {
    it.error = "No preview for this one. It will be sent with the TV's usual framing.";
  }
}

export function useForAll(c: Composer) {
  const it = c.items[c.sel]!;
  for (const o of c.items) {
    if (o !== it && o.iw) {
      o.mode = it.mode;
      o.matte = it.matte;
      o.zoom = 1;
      o.cx = o.cy = 0.5;
    }
  }
}

export function send(c: Composer) {
  const entries = c.items.map((it) => ({ file: it.file!, framing: framingOf(it, c.pa) }));
  closeDialog();
  enqueue(entries, c.tvs);
}

export async function saveReframe(c: Composer) {
  const f = framingOf(c.items[0]!, c.pa);
  if (!f) return;
  await act(async () => {
    await api(tvPath(S.tv!, `/photos/${enc(c.photo!.id)}/reframe`), { json: f });
    closeDialog();
    toast("Reframed");
    loadPhotos();
    setTimeout(loadStatus, 800);
  });
}

// ── choosing files ───────────────────────────────────────────────────
let picker: HTMLInputElement | null = null;
let pickTargets: string[] = [];

/** Open the file picker; the chosen photos go to `tvs`. */
export function pick(tvs: string[]) {
  pickTargets = tvs;
  if (!picker) {
    picker = document.createElement("input");
    picker.type = "file";
    picker.accept = "image/*,.heic,.heif";
    picker.multiple = true;
    picker.hidden = true;
    picker.addEventListener("change", () => {
      if (picker!.files?.length) openComposer([...picker!.files], pickTargets.length ? pickTargets : [S.tv!]);
    });
    document.body.append(picker);
  }
  picker.value = "";
  picker.click();
}
