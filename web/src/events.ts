// Live updates from the server (`/api/events`).
import type { ServerEvent } from "./types";
import { S, api, loadPhotos, loadStatus, loadTvs, thumbsArrived } from "./state";
import type { Info } from "./types";
import { type ToastHandle, toast } from "./toasts";
import { queue } from "./upload";

const timers: Record<string, ReturnType<typeof setTimeout>> = {};
function soon(key: string, fn: () => unknown, ms = 600) {
  clearTimeout(timers[key]);
  timers[key] = setTimeout(fn, ms);
}

let syncToast: ToastHandle | null = null;

function handle(e: ServerEvent) {
  const here = "tv" in e && e.tv === S.tv;
  switch (e.type) {
    case "thumbs":
      if (here) thumbsArrived(e.ids);
      break;
    case "uploaded":
    case "changed":
    case "sync-done":
      if (here) {
        soon("photos", loadPhotos);
        soon("status", loadStatus, 900);
      }
      if (e.type === "sync-done") {
        syncToast?.remove();
        toast(
          `${e.tv}: sync finished, ${e.uploaded} added` +
            (e.deleted ? `, ${e.deleted} removed` : "") +
            (e.failed ? `, ${e.failed} failed` : ""),
        );
      }
      break;
    case "tv-event":
      if (here) soon("status", loadStatus, 900);
      break;
    case "tvs-changed":
      soon("tvs", loadTvs);
      break;
    case "settings-changed":
      soon("info", async () => {
        S.info = await api<Info>("/api/info").catch(() => S.info);
      });
      break;
    case "sync-start":
      syncToast?.remove();
      syncToast = toast(`${e.tv}: syncing ${e.upload} new photos…`, false, 600000);
      break;
    case "sync-progress":
      syncToast?.set(`${e.tv}: uploading ${e.index} of ${e.total}…`);
      break;
    case "error":
      // Failures of our own uploads already show in the queue.
      if (!queue.items.some((i) => i.status === "sending" && i.tv === e.tv)) toast(e.message, true);
      break;
  }
}

export function connectEvents() {
  const es = new EventSource("/api/events");
  es.onmessage = (m) => {
    let e: ServerEvent;
    try {
      e = JSON.parse(m.data);
    } catch {
      return;
    }
    handle(e);
  };
  es.onerror = () => {
    es.close();
    setTimeout(connectEvents, 3000);
  };
}
