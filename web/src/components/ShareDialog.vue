<script setup lang="ts">
import { ref } from "vue";
import ModalDialog from "./ModalDialog.vue";
import { S, act, api, closeDialog, isAdmin, shareUrl, tvPath } from "../state";
import { toast } from "../toasts";
import type { ShareCardRequest } from "../types";
import { enc } from "../util";

const tv = S.tv!;
const url = shareUrl();
const local = /^(https?:\/\/)(localhost|127\.)/.test(url);
const urlEl = ref<HTMLElement>();
const sending = ref(false);

async function copy() {
  try {
    await navigator.clipboard.writeText(url);
    toast("Link copied");
  } catch {
    if (urlEl.value) getSelection()?.selectAllChildren(urlEl.value);
  }
}

async function showOnTv() {
  sending.value = true;
  await act(async () => {
    await api(tvPath(tv, "/share-card"), { json: { url } satisfies ShareCardRequest });
    toast("Scan the TV with your phone camera");
    closeDialog();
  });
  sending.value = false;
}
</script>

<template>
  <ModalDialog :title="`Share ${tv}`">
    <p class="muted" style="margin: 0">Anyone on your Wi-Fi can open this to send photos to {{ tv }}. No app needed.</p>
    <div class="qr"><img :src="`/api/qr.svg?text=${enc(url)}`" alt="QR code" /></div>
    <div class="row">
      <div ref="urlEl" class="url grow">{{ url }}</div>
      <button @click="copy">Copy</button>
    </div>
    <div v-if="local" class="notice err">
      This link uses “localhost”, which only works on this computer. Open the app using this computer's network address,
      or set the share address in App settings (⚙).
    </div>
    <p v-if="S.info?.localsend?.length" class="muted small" style="margin: 0">
      With the LocalSend app, phones can also send straight to “{{ tv }}”.
    </p>
    <template #foot="{ close }">
      <button v-if="isAdmin" :disabled="sending" @click="showOnTv">
        {{ sending ? "Sending to the TV…" : "Show this QR code on the TV" }}
      </button>
      <button class="primary" @click="close">Done</button>
    </template>
  </ModalDialog>
</template>
