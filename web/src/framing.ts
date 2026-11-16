// Framing math shared by the composer: how a photo fills the panel, or sits inside a matte.
import type { Crop, Framing, Tv } from "./types";
import { clamp, matteParts } from "./util";

/** Approximations of the TV's matte colours, for the preview. */
export const MATTE_COLORS: Record<string, string> = {
  polar: "#f4f4f2",
  antique: "#e9e1cf",
  warm: "#efe6d8",
  sand: "#d9c6a5",
  seafoam: "#a8c8bd",
  sage: "#a9b39a",
  burgandy: "#6b2333",
  navy: "#22304a",
  apricot: "#f0b48a",
  byzantine: "#7c3f73",
  lavender: "#b8a9cf",
  redorange: "#d4583a",
  skyblue: "#8fb8d8",
  turqoise: "#3a9a9a",
  black: "#151515",
};

/** Size of each matte type's opening, as fractions of the panel. */
export const MATTE_OPENING: Record<string, [number, number]> = {
  modernthin: [0.86, 0.86],
  modern: [0.76, 0.76],
  modernwide: [0.66, 0.66],
  flexible: [0.8, 0.8],
  shadowbox: [0.74, 0.74],
  panoramic: [0.9, 0.56],
  triptych: [0.84, 0.7],
};

export type Mode = "fill" | "whole";

/** The part of a photo being framed. `iw`/`ih` are known once its preview loads. */
export interface Frameable {
  iw?: number;
  ih?: number;
  mode?: Mode;
  suggest?: Mode;
  /** The TV's usual matte for this orientation. */
  pref?: string;
  matte: string;
  zoom: number;
  cx: number;
  cy: number;
}

export const panelAspect = (tv: Tv | undefined) => {
  const r = tv?.resolution;
  return r?.[0] && r[1] ? r[0] / r[1] : 16 / 9;
};

const r4 = (v: number) => Math.round(v * 1e4) / 1e4;

/** Start from what the TV would do on its own (the server's "auto" rule). */
export function frameDefaults(it: Frameable, tv: Tv | undefined) {
  const pa = panelAspect(tv);
  const ia = it.iw! / it.ih!;
  const pref = ia < 1 ? tv?.portrait_matte || "shadowbox_polar" : tv?.matte || "auto";
  const close = Math.abs(ia / pa - 1) <= 0.12;
  it.suggest = close ? "fill" : "whole";
  it.mode = tv?.fit === "cover" ? "fill" : tv?.fit === "contain" || tv?.fit === "original" ? "whole" : it.suggest;
  if (pref === "none" && tv?.fit !== "contain") it.mode = "fill"; // this TV prefers no borders
  it.pref = pref;
  it.matte = pref === "auto" || pref === "none" ? "shadowbox_polar" : pref;
  it.zoom = 1;
  it.cx = 0.5;
  it.cy = 0.5;
}

function cropSize(it: Frameable, pa: number) {
  const ia = it.iw! / it.ih!;
  return { w: (ia > pa ? pa / ia : 1) / it.zoom, h: (ia > pa ? 1 : ia / pa) / it.zoom };
}

/** Keep the crop's centre where the crop stays inside the photo. */
export function clampCenter(it: Frameable, pa: number) {
  const { w, h } = cropSize(it, pa);
  it.cx = clamp(it.cx, w / 2, 1 - w / 2);
  it.cy = clamp(it.cy, h / 2, 1 - h / 2);
}

/** The region of the photo that fills the panel, as fractions of the photo. */
export function cropOf(it: Frameable, pa: number): Crop {
  const { w, h } = cropSize(it, pa);
  const cx = clamp(it.cx, w / 2, 1 - w / 2);
  const cy = clamp(it.cy, h / 2, 1 - h / 2);
  return { x: cx - w / 2, y: cy - h / 2, w, h };
}

export function setFromCrop(it: Frameable, crop: Crop, pa: number) {
  const ia = it.iw! / it.ih!;
  it.zoom = clamp((ia > pa ? pa / ia : 1) / crop.w, 1, 4);
  it.cx = crop.x + crop.w / 2;
  it.cy = crop.y + crop.h / 2;
}

export function framingOf(it: Frameable, pa: number): Framing | null {
  if (!it.iw) return null;
  if (it.mode === "fill") {
    const k = cropOf(it, pa);
    return { fit: "cover", crop: { x: r4(k.x), y: r4(k.y), w: r4(k.w), h: r4(k.h) }, matte: "none" };
  }
  return { fit: "contain", crop: null, matte: it.matte };
}

/** Where the photo (and the matte's shadow) sit on the stage, as fractions of the panel. */
export function stageLayout(it: Frameable, pa: number) {
  if (it.mode === "fill") {
    const k = cropOf(it, pa);
    return { fill: true, img: { x: -k.x / k.w, y: -k.y / k.h, w: 1 / k.w, h: 1 / k.h } } as const;
  }
  const [type, color] = matteParts(it.matte);
  const [ow, oh] = MATTE_OPENING[type] || MATTE_OPENING.modern!;
  const ia = it.iw! / it.ih!;
  const oa = (pa * ow) / oh;
  const [w, h] = ia > oa ? [ow, (ow * pa) / ia] : [(oh * ia) / pa, oh];
  return {
    fill: false,
    img: { x: (1 - w) / 2, y: (1 - h) / 2, w, h },
    background: MATTE_COLORS[color] || MATTE_COLORS.polar!,
    shadowbox: type === "shadowbox",
  } as const;
}

/** How much of the photo "fill the screen" trims, in percent. */
export function trimmedPercent(it: Frameable, pa: number) {
  const ia = it.iw! / it.ih!;
  return Math.round((1 - Math.min(ia / pa, pa / ia)) * 100);
}
