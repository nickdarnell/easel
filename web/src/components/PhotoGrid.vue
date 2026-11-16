<script setup lang="ts">
// The photos on the selected TV.
import { computed } from "vue";
import type { Filter, Tv } from "../types";
import { S, openPhoto, setFilter, thumbUrl } from "../state";

defineProps<{ tv: Tv }>();

const filters: [Filter, string][] = [
  ["all", "All"],
  ["mine", "Added here"],
  ["favorites", "Favorites"],
];
const list = computed(() => (S.filter === "mine" ? (S.photos ?? []).filter((p) => p.mine) : (S.photos ?? [])));
const current = computed(() => S.status?.current?.content_id);
const loaded = (e: Event) => (e.target as HTMLImageElement).classList.add("loaded");
</script>

<template>
  <section v-if="tv.power === 'on'">
    <div class="section-head">
      <h2>On this TV</h2>
      <div class="filters">
        <button v-for="[k, l] in filters" :key="k" :class="{ active: S.filter === k }" @click="setFilter(k)">
          {{ l }}
        </button>
      </div>
    </div>
    <div v-if="S.photosErr" class="notice err">{{ S.photosErr }}</div>
    <div v-else-if="!S.photos" class="grid"><div v-for="n in 8" :key="n" class="tile"></div></div>
    <div v-else-if="list.length" class="grid">
      <button
        v-for="p in list"
        :key="p.id"
        class="tile"
        :class="{ current: p.id === current }"
        :title="p.source || p.id"
        @click="openPhoto(p.id)"
      >
        <span class="ph">{{ p.width && p.height ? `${p.width}×${p.height}` : "" }}</span>
        <img alt="" :src="p.thumb ? thumbUrl(tv.name, p.id) : undefined" @load="loaded" />
        <span class="badges">
          <span v-if="p.id === current" class="badge gold">On now</span>
          <span v-if="p.mine" class="badge">Added here</span>
        </span>
      </button>
    </div>
    <div v-else class="empty">
      <h3>{{ S.filter === "all" ? "Nothing here yet" : "No photos match" }}</h3>
      <p>Choose photos above, or drop them onto this page.</p>
    </div>
  </section>
</template>
