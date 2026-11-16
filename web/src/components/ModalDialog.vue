<script setup lang="ts">
// The app's one modal dialog. Closing it (×, Escape, a click on the backdrop) clears the open dialog.
import { onMounted, ref } from "vue";
import { dialog, dialogClosed } from "../state";

defineProps<{ title: string; wide?: boolean; composer?: boolean }>();

const el = ref<HTMLDialogElement>();
const key = dialog.value!.key;
let downOn: EventTarget | null = null;

onMounted(() => el.value!.showModal());

const onDown = (e: PointerEvent) => (downOn = e.target);
const close = () => el.value?.close();
// Only a click that starts and ends on the backdrop closes it (not a drag that ends there).
const onClick = (e: MouseEvent) => {
  if (e.target === el.value && downOn === el.value) close();
};
</script>

<template>
  <dialog ref="el" :class="{ wide, composer }" @close="dialogClosed(key)" @pointerdown="onDown" @click="onClick">
    <div class="dlg-head">
      <h3>{{ title }}</h3>
      <button class="x" aria-label="Close" @click="close">×</button>
    </div>
    <div class="dlg-body"><slot /></div>
    <div v-if="$slots.foot" class="dlg-foot"><slot name="foot" :close="close" /></div>
  </dialog>
</template>
