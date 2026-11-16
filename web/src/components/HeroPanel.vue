<script setup lang="ts">
// The selected TV: what it's showing, sending photos, and its controls.
import { computed } from "vue";
import type { Tv } from "../types";
import { pick } from "../composer";
import { S, isAdmin, openDialog, openTvSettings, recheck, setSlideshow, thumbUrl, toggleArt, wake } from "../state";

const props = defineProps<{ tv: Tv }>();

const on = computed(() => props.tv.power === "on");
const current = computed(() => S.status?.current?.content_id);
const minutes = computed(() => S.status?.slideshow?.minutes || 0);
const slideshowOptions = computed(() => {
  const opts: [number, string][] = [
    [0, "Off"],
    [15, "Every 15 min"],
    [60, "Every hour"],
    [180, "Every 3 hours"],
    [720, "Every 12 hours"],
    [1440, "Every day"],
  ];
  const ss = minutes.value;
  if (ss && !opts.some((o) => o[0] === ss)) opts.push([ss, `Every ${ss} min`]);
  return opts;
});
const state = computed(() =>
  on.value
    ? S.status?.artmode === false
      ? "On, watching TV"
      : "Showing art"
    : props.tv.power === "standby"
      ? "Standby"
      : "Off",
);
const sendToAll = () => pick(S.tvs.filter((t) => t.power === "on").map((t) => t.name));
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
      <span v-if="on && current" class="caption">Now showing</span>
    </div>
    <div>
      <h1 class="tvname">
        <template v-if="S.guestTv">Send a photo to<br /></template>{{ tv.name }}
      </h1>
      <div class="meta">
        <span
          ><span class="dot" :class="tv.power" style="display: inline-block; margin-right: 6px"></span>{{ state }}</span
        >
        <span>{{ tv.model || "" }}</span>
        <span>{{ S.photos?.length ?? "…" }} photos</span>
      </div>
      <div v-if="S.statusErr && on" class="notice err">{{ S.statusErr }}</div>
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
      <div v-if="isAdmin" class="controls">
        <template v-if="on">
          <button @click="toggleArt">{{ S.status?.artmode === false ? "Switch to art" : "Art mode on" }}</button>
          <span class="field"
            >Slideshow
            <select :value="minutes" @change="setSlideshow(+($event.target as HTMLSelectElement).value)">
              <option v-for="[v, l] in slideshowOptions" :key="v" :value="v">{{ l }}</option>
            </select></span
          >
        </template>
        <button @click="openDialog({ kind: 'share' })">Share…</button>
        <button class="ghost" @click="openTvSettings">TV settings</button>
      </div>
      <div v-if="!S.guestTv && !S.info?.admin" class="controls">
        <button class="ghost" @click="openDialog({ kind: 'share' })">Share this TV…</button>
      </div>
    </div>
  </section>
</template>
