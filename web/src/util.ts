export const enc = encodeURIComponent;

/** Per-browser preferences, kept under the same `frames.` keys the app has always used. */
export const store = {
  get<T>(key: string, fallback: T): T {
    try {
      const v = localStorage.getItem("frames." + key);
      return v === null ? fallback : (JSON.parse(v) as T);
    } catch {
      return fallback;
    }
  },
  set(key: string, value: unknown) {
    try {
      localStorage.setItem("frames." + key, JSON.stringify(value));
    } catch {
      // Private mode or storage blocked: preferences just won't stick.
    }
  },
};

export const cap = (s: string | undefined) => (s ? s[0]!.toUpperCase() + s.slice(1) : "");
export const clamp = (v: number, lo: number, hi: number) => Math.min(Math.max(v, lo), hi);
export const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/** `shadowbox_polar` → `["shadowbox", "polar"]`; `none` → `["none", ""]`. */
export function matteParts(m: string | null | undefined): [string, string] {
  const s = m || "none";
  const i = s.indexOf("_");
  return i < 0 ? [s, ""] : [s.slice(0, i), s.slice(i + 1)];
}

export function prettyMatte(id: string | null | undefined): string {
  if (!id || id === "none") return "No matte";
  const [t, c] = matteParts(id);
  return c ? `${cap(t)}, ${c}` : cap(t);
}

export function prettyDate(d: string | null | undefined): string {
  if (!d) return "";
  const m = d.match(/^(\d{4})[:-](\d\d)[:-](\d\d)/);
  return m
    ? new Date(+m[1]!, +m[2]! - 1, +m[3]!).toLocaleDateString(undefined, {
        year: "numeric",
        month: "short",
        day: "numeric",
      })
    : d;
}

export function isImage(f: File): boolean {
  return f.type.startsWith("image/") || /\.(jpe?g|png|heic|heif|webp|gif|bmp|tiff?)$/i.test(f.name);
}

export const errorMessage = (e: unknown) => (e instanceof Error ? e.message : String(e));
