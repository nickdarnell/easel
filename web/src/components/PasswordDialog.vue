<script setup lang="ts">
import { onMounted, ref } from "vue";
import ModalDialog from "./ModalDialog.vue";
import type { Info } from "../types";
import { S, api, closeDialog, loadTv } from "../state";
import { toast } from "../toasts";
import { store } from "../util";

const pw = ref("");
const input = ref<HTMLInputElement>();
onMounted(() => setTimeout(() => input.value?.focus(), 50));

async function unlock() {
  S.pw = pw.value;
  const info = await api<Info>("/api/info").catch(() => null);
  if (info?.admin) {
    store.set("pw", S.pw);
    S.info = info;
    closeDialog();
    loadTv();
    toast("Unlocked");
  } else {
    S.pw = "";
    toast("Wrong password", true);
  }
}
</script>

<template>
  <ModalDialog title="Admin password">
    <p class="muted" style="margin: 0">
      Managing the TVs needs the password set on the server. Anyone can still send photos without it.
    </p>
    <input
      ref="input"
      v-model="pw"
      type="password"
      placeholder="Password"
      autocomplete="current-password"
      @keydown.enter="unlock"
    />
    <template #foot="{ close }">
      <button @click="close">Cancel</button>
      <button class="primary" @click="unlock">Unlock</button>
    </template>
  </ModalDialog>
</template>
