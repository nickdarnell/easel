// Live updates from the server (`/api/events`). The server talks to the TVs; the page only
// talks to the server, which pushes what changes (including the TV's status) through here.
import { watch } from "vue";
import type { ServerEvent } from "./types";
import { S, api, loadPhotos, loadQueue, loadStatus, loadTvs, thumbsArrived } from "./state";
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
    case "status":
      if (here) {
        S.status = e.status;
        S.statusErr = null;
      }
      break;
    case "uploaded":
    case "changed":
    case "sync-done":
      if (here) soon("photos", loadPhotos);
      if (e.type === "sync-done") {
        syncToast?.remove();
        toast(
          `${e.tv}: sync finished, ${e.uploaded} added` +
            (e.deleted ? `, ${e.deleted} removed` : "") +
            (e.failed ? `, ${e.failed} failed` : ""),
        );
      }
      break;
    case "queue":
      // Events leave out what the TV is being asked (the queue dialog asks for that).
      if (here) S.queue = { ...e.queue, asking: e.queue.asking ?? S.queue?.asking ?? null };
      break;
    case "tv-event":
      // The server already acts on these, and sends `status` or `changed` if anything changed.
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

let es: EventSource | null = null;
let retry: ReturnType<typeof setTimeout> | undefined;
let following = false;

/** Listen for events. Saying which TV is on screen tells the server to keep an eye on it (and
 * stop once nobody's looking), so it reconnects whenever another TV is chosen. */
export function connectEvents(missed = false) {
  if (!following) {
    following = true;
    watch(
      () => S.tv,
      () => connectEvents(),
    );
  }
  clearTimeout(retry);
  es?.close();
  const me = new EventSource("/api/events" + (S.tv ? "?tv=" + encodeURIComponent(S.tv) : ""));
  es = me;
  me.onopen = () => {
    // Catch up on whatever happened while disconnected.
    if (missed) {
      missed = false;
      soon("tvs", loadTvs);
      soon("photos", loadPhotos);
      soon("status", loadStatus);
      soon("queue", loadQueue);
    }
  };
  me.onmessage = (m) => {
    let e: ServerEvent;
    try {
      e = JSON.parse(m.data);
    } catch {
      return;
    }
    handle(e);
  };
  me.onerror = () => {
    me.close();
    if (es === me) retry = setTimeout(() => connectEvents(true), 3000);
  };
}
