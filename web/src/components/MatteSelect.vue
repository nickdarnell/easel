<script setup lang="ts">
// A matte picker: type (with "No matte", and optionally "Automatic") plus colour.
import { computed } from "vue";
import type { Mattes } from "../types";
import { cap, matteParts } from "../util";

const props = defineProps<{ mattes: Mattes | null; allowAuto?: boolean }>();
const model = defineModel<string>({ required: true });

const plain = (t: string) => t === "none" || t === "auto";
const types = computed(() => (props.mattes?.types ?? []).filter((t) => t !== "none"));
const colors = computed(() => props.mattes?.colors ?? []);
const type = computed(() => (plain(model.value) ? model.value : matteParts(model.value)[0]));
const color = computed(() => (plain(model.value) ? "" : matteParts(model.value)[1]));

function setType(t: string) {
  model.value = plain(t) ? t : `${t}_${color.value || colors.value[0] || "polar"}`;
}
</script>

<template>
  <select :value="type" @change="setType(($event.target as HTMLSelectElement).value)">
    <option v-if="allowAuto" value="auto">Automatic</option>
    <option value="none">No matte</option>
    <option v-for="t in types" :key="t" :value="t">{{ cap(t) }}</option>
  </select>
  <select v-if="!plain(type)" :value="color" @change="model = `${type}_${($event.target as HTMLSelectElement).value}`">
    <option v-for="c in colors" :key="c" :value="c">{{ c }}</option>
  </select>
</template>
