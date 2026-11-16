// The upload panel: photos going to the server, one at a time per TV, with progress. The server
// keeps them and sends them on whenever the TV answers (see the TV's waiting changes).
import { reactive } from "vue";
import type { ErrorBody, Framing, Queued } from "./types";
import { S, tvPath } from "./state";
import { enc, errorMessage } from "./util";

export type QueueStatus = "queued" | "sending" | "done" | "failed";

export interface QueueItem {
  file: File;
  framing: Framing | null;
  tv: string;
  show: boolean;
  status: QueueStatus;
  pct: number;
  msg: string;
}

export const queue = reactive({ items: [] as QueueItem[] });
const running = new Set<string>();
let hideTimer: ReturnType<typeof setTimeout> | undefined;

export interface Entry {
  file: File;
  /** `null` uses the TV's usual framing. */
  framing: Framing | null;
}

export function enqueue(entries: Entry[], tvs: string[]) {
  if (!entries.length) return;
  for (const tv of tvs) {
    entries.forEach((e, i) =>
      queue.items.push({
        file: e.file,
        framing: e.framing,
        tv,
        show: S.showNow && i === entries.length - 1,
        status: "queued",
        pct: 0,
        msg: "Waiting",
      }),
    );
  }
  clearTimeout(hideTimer);
  for (const tv of tvs) pump(tv);
}

/** Drop finished items from the panel; anything still sending stays. */
export function clearFinished() {
  queue.items = queue.items.filter((i) => i.status === "queued" || i.status === "sending");
}

async function pump(tv: string) {
  if (running.has(tv)) return;
  running.add(tv);
  let item: QueueItem | undefined;
  while ((item = queue.items.find((q) => q.tv === tv && q.status === "queued"))) {
    item.status = "sending";
    item.msg = "Uploading";
    try {
      await sendOne(item);
      item.status = "done";
      item.msg = "Saved, going to the TV";
    } catch (e) {
      item.status = "failed";
      item.msg = errorMessage(e);
    }
  }
  running.delete(tv);
  if (!running.size && queue.items.every((q) => q.status === "done")) {
    hideTimer = setTimeout(() => (queue.items = []), 5000);
  }
}

async function prepareFile(file: File): Promise<{ body: Blob; name: string; type: string }> {
  // If the server can't decode HEIC but this browser can (Safari), convert it here.
  const heic = /\.(heic|heif)$/i.test(file.name) || /image\/hei[cf]/.test(file.type);
  if (heic && !S.info?.heif) {
    try {
      const bmp = await createImageBitmap(file);
      const c = document.createElement("canvas");
      c.width = bmp.width;
      c.height = bmp.height;
      c.getContext("2d")!.drawImage(bmp, 0, 0);
      const blob = await new Promise<Blob | null>((res) => c.toBlob(res, "image/jpeg", 0.92));
      if (blob) return { body: blob, name: file.name.replace(/\.(heic|heif)$/i, ".jpg"), type: "image/jpeg" };
    } catch {
      throw new Error("This server can't read HEIC photos; export as JPEG first");
    }
  }
  return { body: file, name: file.name, type: file.type || "application/octet-stream" };
}

async function sendOne(item: QueueItem): Promise<Queued> {
  const f = await prepareFile(item.file);
  // XMLHttpRequest rather than fetch, for upload progress.
  return new Promise((resolve, reject) => {
    const x = new XMLHttpRequest();
    const fr = item.framing;
    let qs = `name=${enc(f.name)}&show=${item.show}`;
    if (fr) {
      qs += `&fit=${fr.fit}&matte=${enc(fr.matte || "")}`;
      if (fr.crop) qs += `&crop=${[fr.crop.x, fr.crop.y, fr.crop.w, fr.crop.h].join(",")}`;
    }
    x.open("POST", tvPath(item.tv, `/upload?${qs}`));
    if (S.pw) x.setRequestHeader("Authorization", "Bearer " + S.pw);
    x.setRequestHeader("Content-Type", f.type);
    x.upload.onprogress = (e) => {
      if (!e.lengthComputable) return;
      item.pct = Math.round((e.loaded / e.total) * 100);
      item.msg = item.pct < 100 ? `Uploading ${item.pct}%` : "Saving…";
    };
    x.onload = () => {
      let body: unknown = null;
      try {
        body = JSON.parse(x.responseText);
      } catch {
        // Not JSON: report the status.
      }
      if (x.status === 202 || x.status === 200) resolve(body as Queued);
      else reject(new Error((body as Partial<ErrorBody> | null)?.error || `Failed (${x.status})`));
    };
    x.onerror = () => reject(new Error("Network error"));
    x.send(f.body);
  });
}
