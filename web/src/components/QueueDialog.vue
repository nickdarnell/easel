<script setup lang="ts">
// What the server is waiting on for the selected TV: what it's asking the TV right now, and the
// changes (photos, mattes, the slideshow…) it'll make whenever the TV answers.
import { computed, onUnmounted, ref } from "vue";
import ModalDialog from "./ModalDialog.vue";
import { S, dropChange, loadQueue, retryChange, tvPath } from "../state";
import type { Change } from "../types";
import { ago } from "../util";

const now = ref(Date.now());
// Ask often while it's open: what the TV is being asked isn't sent in events.
loadQueue();
const poll = setInterval(loadQueue, 1500);
const tick = setInterval(() => (now.value = Date.now()), 1000);
onUnmounted(() => {
  clearInterval(poll);
  clearInterval(tick);
});

const q = computed(() => S.queue);
const secs = (ms: number) => {
  const s = Math.max(0, Math.round(ms / 1000));
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${s % 60}s`;
};
const others = computed(() => {
  const w = q.value?.waiting;
  if (!w) return "";
  const parts = [
    w.commands && `${w.commands} ${w.commands === 1 ? "thing" : "things"} you asked for`,
    w.reads && `${w.reads} ${w.reads === 1 ? "thing" : "things"} a page is waiting to see`,
    w.background && `${w.background} background ${w.background === 1 ? "check" : "checks"}`,
  ].filter(Boolean);
  return parts.join(", ");
});

function stateText(c: Change) {
  if (c.state === "working") return c.attempts ? `Trying again (attempt ${c.attempts + 1})…` : "Doing it now…";
  if (c.state === "failed") return `${S.tv} turned it down: ${c.error}`;
  if (!c.attempts) return `Waiting its turn (asked ${ago(c.queued)})`;
  const when = c.retry_at
    ? c.retry_at > now.value
      ? `in ${secs(c.retry_at - now.value)}`
      : "soon"
    : "when it answers";
  return `${c.error ?? "Didn't work"}. Trying again ${when}.`;
}
const preview = (c: Change) => tvPath(S.tv!, `/queue/${c.id}/preview`);
</script>

<template>
  <ModalDialog :title="`Waiting on ${S.tv}`" wide>
    <div v-if="!q" class="muted"><span class="spinner sm"></span>Loading…</div>
    <div v-else class="stack">
      <div class="qd-now">
        <div class="ss-title">Right now</div>
        <div v-if="q.activity" class="row">
          <span class="spinner sm"></span><span class="grow">{{ q.activity.message }}</span
          ><span class="muted small">{{ secs(now - q.activity.since) }}</span>
        </div>
        <div v-if="q.asking" class="row">
          <span class="spinner sm"></span
          ><span class="grow"
            >Waiting for {{ S.tv }} to answer: {{ q.asking.label }}
            <span class="muted small">({{ q.asking.request }})</span></span
          ><span class="muted small">{{ secs(q.asking.ms) }}</span>
        </div>
        <template v-if="!q.activity && !q.asking">
          <div v-if="q.changes.some((c) => c.state === 'working')" class="row">
            <span class="spinner sm"></span><span class="grow">Getting through to {{ S.tv }}…</span>
          </div>
          <div v-else class="muted">Not waiting for {{ S.tv }} to answer anything.</div>
        </template>
        <div v-if="others" class="muted small">Also waiting their turn: {{ others }}.</div>
      </div>

      <div class="ss-title">Changes to make</div>
      <div v-if="!q.changes.length" class="muted">None. Everything you asked for is on the TV.</div>
      <ol v-else class="qd-list">
        <li v-for="c in q.changes" :key="c.id" :class="c.state">
          <img v-if="c.op.kind === 'upload'" class="qd-thumb" :src="preview(c)" alt="" />
          <div class="grow">
            <div>{{ c.label }}</div>
            <div class="small" :class="c.state === 'failed' ? 'errtext' : 'muted'">
              <span v-if="c.state === 'working'" class="spinner sm"></span>{{ stateText(c) }}
            </div>
          </div>
          <div class="row tight">
            <button v-if="c.state !== 'working'" class="ghost" @click="retryChange(c.id)">
              {{ c.state === "failed" ? "Try again" : "Try now" }}
            </button>
            <button v-if="c.state !== 'working'" class="ghost" title="Don't make this change" @click="dropChange(c.id)">
              Cancel
            </button>
          </div>
        </li>
      </ol>
      <p class="muted small">
        Changes are made one at a time, oldest first, whenever {{ S.tv }} answers. "Try now" wakes it up if it can; by
        itself the server only tries again while the TV is on.
      </p>
    </div>
  </ModalDialog>
</template>
