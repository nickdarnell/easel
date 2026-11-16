<script setup lang="ts">
// The photos on the selected TV. Admins can tick several (Select, or Ctrl/⌘/Shift-click) to act
// on them together.
import { computed, ref } from "vue";
import type { Filter, Photo, Tv } from "../types";
import MatteSelect from "./MatteSelect.vue";
import {
  S,
  deleteSelected,
  endSelection,
  favoriteSelected,
  getMattes,
  isAdmin,
  matteSelected,
  openPhoto,
  pending,
  selection,
  setFilter,
  thumbUrl,
  brokenThumbs,
  toggleSelected,
} from "../state";
import { ago } from "../util";

const props = defineProps<{ tv: Tv }>();

const filters: [Filter, string][] = [
  ["all", "All"],
  ["mine", "Added here"],
  ["favorites", "Favorites"],
];
const list = computed(() => (S.filter === "mine" ? (S.photos ?? []).filter((p) => p.mine) : (S.photos ?? [])));
const current = computed(() => S.status?.current?.content_id);
const on = computed(() => props.tv.power === "on");
const loaded = (e: Event) => (e.target as HTMLImageElement).classList.add("loaded");
/** Thumbnails the server couldn't get. */
const broken = brokenThumbs;

const offlineText = computed(() => {
  const o = S.photosOffline;
  if (!o) return "";
  const why = on.value
    ? `Can't reach ${props.tv.name} right now`
    : `${props.tv.name} is ${props.tv.power === "standby" ? "asleep" : "off"}`;
  return `${why}, so these are its photos as of ${ago(o.asOf)}.`;
});

// ── selecting ──
let anchor: string | null = null;
function startSelecting() {
  selection.on = true;
  if (on.value) getMattes();
}
function clicked(p: Photo, e: MouseEvent) {
  if (!isAdmin.value || (!selection.on && !e.ctrlKey && !e.metaKey && !e.shiftKey)) {
    openPhoto(p.id);
    return;
  }
  if (!selection.on) startSelecting();
  const ids = list.value.map((x) => x.id);
  if (e.shiftKey && anchor && ids.includes(anchor)) {
    const [a, b] = [ids.indexOf(anchor), ids.indexOf(p.id)].sort((x, y) => x - y);
    for (const id of ids.slice(a, b + 1)) selection.ids.add(id);
  } else {
    toggleSelected(p.id);
  }
  anchor = p.id;
}
const count = computed(() => selection.ids.size);
const allSelected = computed(() => list.value.length > 0 && list.value.every((p) => selection.ids.has(p.id)));
function selectAll() {
  if (allSelected.value) selection.ids.clear();
  else for (const p of list.value) selection.ids.add(p.id);
}
const bulkMatte = ref("none");
const armed = ref(false);
function remove() {
  if (!armed.value) {
    armed.value = true;
    setTimeout(() => (armed.value = false), 4000);
    return;
  }
  armed.value = false;
  deleteSelected();
}
const plural = (n: number) => `${n} ${n === 1 ? "photo" : "photos"}`;
</script>

<template>
  <section v-if="on || S.photos">
    <div class="section-head">
      <h2>On this TV</h2>
      <div class="filters">
        <button v-for="[k, l] in filters" :key="k" :class="{ active: S.filter === k }" @click="setFilter(k)">
          {{ l }}
        </button>
        <button v-if="isAdmin && !selection.on && list.length" class="ghost" @click="startSelecting">Select</button>
      </div>
    </div>
    <div v-if="S.photosErr" class="notice err">{{ S.photosErr }}</div>
    <div v-else-if="S.photosOffline" class="notice" :title="S.photosOffline.reason">{{ offlineText }}</div>
    <div v-if="!S.photos && !S.photosErr" class="grid"><div v-for="n in 8" :key="n" class="tile"></div></div>
    <div v-else-if="list.length" class="grid" :class="{ selecting: selection.on }">
      <button
        v-for="p in list"
        :key="p.id"
        class="tile"
        :class="{ current: p.id === current, selected: selection.ids.has(p.id) }"
        :title="p.source || p.id"
        @click="clicked(p, $event)"
      >
        <span class="ph">{{
          broken.has(p.id) ? "No preview" : p.width && p.height ? `${p.width}×${p.height}` : ""
        }}</span>
        <img
          v-if="!broken.has(p.id)"
          alt=""
          :src="p.thumb ? thumbUrl(tv.name, p.id) : undefined"
          @load="loaded"
          @error="broken.add(p.id)"
        />
        <span class="badges">
          <span v-if="p.id === current" class="badge gold">On now</span>
          <span v-if="p.mine" class="badge">Added here</span>
        </span>
        <span v-if="selection.on" class="tick" aria-hidden="true">{{ selection.ids.has(p.id) ? "✓" : "" }}</span>
      </button>
    </div>
    <div v-else-if="S.photos" class="empty">
      <h3>{{ S.filter === "all" ? "Nothing here yet" : "No photos match" }}</h3>
      <p>Choose photos above, or drop them onto this page.</p>
    </div>

    <div v-if="selection.on" class="selbar" role="toolbar" aria-label="Selected photos">
      <div class="row">
        <strong>{{ count ? `${plural(count)} selected` : "Tap photos to select them" }}</strong>
        <button class="link" @click="selectAll">{{ allSelected ? "Select none" : "Select all" }}</button>
        <span class="grow"></span>
        <button class="ghost" @click="endSelection">Done</button>
      </div>
      <div v-if="pending.photos" class="row muted"><span class="spinner sm"></span>{{ pending.photos }}</div>
      <div v-else-if="count && on" class="row wrap">
        <button @click="favoriteSelected(S.filter !== 'favorites')">
          {{ S.filter === "favorites" ? "Remove from favorites" : "Add to favorites" }}
        </button>
        <span class="row tight"
          ><MatteSelect v-model="bulkMatte" :mattes="S.mattes" /><button @click="matteSelected(bulkMatte)">
            Set matte
          </button></span
        >
        <span class="grow"></span>
        <button class="danger" :class="{ primary: armed }" @click="remove">
          {{ armed ? `Really delete ${plural(count)}?` : "Delete" }}
        </button>
      </div>
      <div v-else-if="count" class="row muted small">Turn the TV on to change these photos.</div>
    </div>
  </section>
</template>
