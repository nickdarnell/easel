<script setup lang="ts">
// The selected TV: what it's showing, sending photos, and its controls.
import { computed, onUnmounted, reactive, ref, watch } from "vue";
import type { Tv } from "../types";
import { pick } from "../composer";
import {
  S,
  type SlideshowChoice,
  endShare,
  isAdmin,
  openDialog,
  openTvSettings,
  pending,
  recheck,
  setSlideshow,
  slideshow,
  thumbUrl,
  toggleArt,
  tvReady,
  wake,
} from "../state";
import { store } from "../util";

const props = defineProps<{ tv: Tv }>();

// What the server is doing to get the TV to answer; anything sent meanwhile waits for it.
const activity = computed(() => S.status?.activity ?? null);
const on = tvReady;
const now = ref(Date.now());
let tick: ReturnType<typeof setInterval> | undefined;
watch(
  () => !!activity.value,
  (busy) => {
    clearInterval(tick);
    if (busy) tick = setInterval(() => (now.value = Date.now()), 1000);
  },
  { immediate: true },
);
onUnmounted(() => clearInterval(tick));
const elapsed = computed(() => {
  const a = activity.value;
  if (!a) return "";
  const s = Math.max(0, Math.round((now.value - a.since) / 1000));
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${s % 60}s`;
});
const current = computed(() => S.status?.current?.content_id);
const sharing = computed(() => !!S.status?.sharing || !!pending.share);
const artmode = computed(() => pending.art ?? S.status?.artmode);
const state = computed(() =>
  activity.value && activity.value.kind !== "waiting"
    ? "Not answering yet"
    : on.value
      ? artmode.value === false
        ? "On, watching TV"
        : "Showing art"
      : props.tv.power === "standby"
        ? "Standby"
        : "Off",
);
const identity = computed(() =>
  [props.tv.size_inches ? `${props.tv.size_inches}″` : null, props.tv.model].filter(Boolean).join(" · "),
);
const sendToAll = () => pick(S.tvs.filter((t) => t.power === "on").map((t) => t.name));

// ── slideshow ──
// What Play starts: the TV's settings while it plays, otherwise the last ones chosen here.
const draft = reactive<SlideshowChoice>({
  minutes: store.get("slideshowMinutes", 60),
  favorites: store.get("slideshowFavorites", false),
  shuffle: store.get("slideshowShuffle", true),
});
watch(
  slideshow,
  (s) => {
    if (s.minutes) Object.assign(draft, s);
  },
  { immediate: true },
);
watch(draft, (d) => {
  store.set("slideshowMinutes", d.minutes);
  store.set("slideshowFavorites", d.favorites);
  store.set("slideshowShuffle", d.shuffle);
});
const playing = computed(() => slideshow.value.minutes > 0);
/** What the controls show: the change being applied, else the TV's slideshow, else the draft. */
const shown = computed(() => pending.slideshow ?? (playing.value ? slideshow.value : draft));
const intervals = computed(() => {
  const opts: [number, string][] = [
    [15, "every 15 minutes"],
    [60, "every hour"],
    [180, "every 3 hours"],
    [720, "every 12 hours"],
    [1440, "every day"],
  ];
  const m = shown.value.minutes;
  if (m && !opts.some((o) => o[0] === m)) opts.push([m, `every ${m} minutes`]);
  return opts;
});
const every = (m: number) => intervals.value.find((o) => o[0] === m)?.[1] ?? `every ${m} minutes`;
const slideshowText = computed(() => {
  const p = pending.slideshow;
  if (p)
    return !p.minutes
      ? "Stopping the slideshow…"
      : playing.value
        ? "Changing the slideshow…"
        : "Starting the slideshow…";
  const s = slideshow.value;
  if (!s.minutes) return "Off. The TV stays on one photo.";
  return `Playing ${s.favorites ? "your favorites" : "all your photos"}${s.shuffle ? " in random order" : " in order"}, a new one ${every(s.minutes)}.`;
});
function change(patch: Partial<SlideshowChoice>) {
  Object.assign(draft, patch);
  if (playing.value) setSlideshow({ ...draft });
}
const play = () => setSlideshow({ ...draft, minutes: draft.minutes || 60 });
const stop = () => setSlideshow({ ...slideshow.value, minutes: 0 });
</script>

<template>
  <section class="hero">
    <div class="frame">
      <div class="screen">
        <div v-if="!on" class="sleep">
          <div class="muted">{{ tv.power === "standby" ? "Screen is off" : "TV is off or unreachable" }}</div>
        </div>
        <img
          v-else-if="current"
          :key="current"
          :src="thumbUrl(tv.name, current)"
          alt=""
          @error="($event.target as HTMLImageElement).style.visibility = 'hidden'"
        />
        <div v-else class="spinner"></div>
      </div>
      <span v-if="on && current" class="caption">{{ sharing ? "QR code" : "Now showing" }}</span>
    </div>
    <div>
      <h1 class="tvname">
        <template v-if="S.guestTv">Send a photo to<br /></template>{{ tv.name }}
      </h1>
      <div class="meta">
        <span
          ><span class="dot" :class="tv.power" style="display: inline-block; margin-right: 6px"></span>{{ state }}</span
        >
        <span v-if="identity">{{ identity }}</span>
        <span>{{ S.photos?.length ?? "…" }} photos</span>
      </div>
      <div v-if="activity" class="notice row activity">
        <span class="spinner sm"></span>
        <span class="grow">{{ activity.message }}</span>
        <span class="muted small">{{ elapsed }}</span>
      </div>
      <div v-else-if="S.statusErr && on" class="notice err">{{ S.statusErr }}</div>

      <div v-if="isAdmin && on && sharing" class="notice banner">
        <div class="grow">
          <strong>The QR code is on the TV.</strong> Point a phone camera at it to send photos.
          <div class="muted small">It goes back to your photos by itself after 30 minutes.</div>
        </div>
        <button class="primary" :disabled="!!pending.share" @click="endShare">
          <span v-if="pending.share" class="spinner sm"></span
          >{{
            pending.share === "ending"
              ? "Going back…"
              : pending.share === "showing"
                ? "Putting it up…"
                : "Done, back to my photos"
          }}
        </button>
      </div>

      <div v-if="on" class="send">
        <button class="primary big" @click="pick([tv.name])">Choose photos…</button>
        <label class="check"><input v-model="S.showNow" type="checkbox" /> Put it on the TV right away</label>
        <span class="hint"
          >Or drag photos anywhere onto this page.
          <a v-if="S.tvs.length > 1 && !S.guestTv" href="#" style="color: var(--accent)" @click.prevent="sendToAll"
            >Send to all TVs…</a
          ></span
        >
      </div>
      <div v-else class="send">
        <button v-if="tv.can_wake && isAdmin" class="primary big" @click="wake">Wake it up</button>
        <span class="hint"
          >Photos can be sent once the TV is on.{{ tv.can_wake ? "" : " Turn it on with the remote." }}</span
        >
        <button class="ghost" @click="recheck">Check again</button>
      </div>

      <div v-if="isAdmin && on && S.status?.loaded" class="slideshow" :class="{ busy: pending.slideshow }">
        <div class="ss-head">
          <div class="grow">
            <div class="ss-title">Slideshow</div>
            <div class="muted small"><span v-if="pending.slideshow" class="spinner sm"></span>{{ slideshowText }}</div>
          </div>
          <button v-if="playing" :disabled="!!pending.slideshow" @click="stop">■ Stop</button>
          <button v-else class="primary" :disabled="!!pending.slideshow" @click="play">▶ Play</button>
        </div>
        <div class="ss-opts">
          <select
            :value="shown.favorites ? 'fav' : 'all'"
            :disabled="!!pending.slideshow"
            @change="change({ favorites: ($event.target as HTMLSelectElement).value === 'fav' })"
          >
            <option value="all">All my photos</option>
            <option value="fav">Favorites only</option>
          </select>
          <select
            :value="shown.minutes || draft.minutes"
            :disabled="!!pending.slideshow"
            @change="change({ minutes: +($event.target as HTMLSelectElement).value })"
          >
            <option v-for="[v, l] in intervals" :key="v" :value="v">A new one {{ l }}</option>
          </select>
          <label class="check"
            ><input
              type="checkbox"
              :checked="shown.shuffle"
              :disabled="!!pending.slideshow"
              @change="change({ shuffle: ($event.target as HTMLInputElement).checked })"
            />
            Shuffle</label
          >
        </div>
      </div>

      <div v-if="isAdmin" class="controls">
        <button v-if="on" :disabled="pending.art !== null" @click="toggleArt">
          <template v-if="pending.art !== null"
            ><span class="spinner sm"></span>{{ pending.art ? "Switching to art…" : "Leaving art mode…" }}</template
          >
          <template v-else>{{ S.status?.artmode === false ? "Switch to art mode" : "Leave art mode" }}</template>
        </button>
        <button @click="openDialog({ kind: 'share' })">Share…</button>
        <button class="ghost" @click="openTvSettings">TV settings</button>
      </div>
      <div v-if="!S.guestTv && !S.info?.admin" class="controls">
        <button class="ghost" @click="openDialog({ kind: 'share' })">Share this TV…</button>
      </div>
    </div>
  </section>
</template>
