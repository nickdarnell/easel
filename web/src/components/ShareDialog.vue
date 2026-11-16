<script setup lang="ts">
import { ref } from "vue";
import ModalDialog from "./ModalDialog.vue";
import { S, closeDialog, isAdmin, pending, shareUrl, showShareCard } from "../state";
import { toast } from "../toasts";
import { enc } from "../util";

const tv = S.tv!;
const url = shareUrl();
const local = /^(https?:\/\/)(localhost|127\.)/.test(url);
const urlEl = ref<HTMLElement>();

async function copy() {
  try {
    await navigator.clipboard.writeText(url);
    toast("Link copied");
  } catch {
    if (urlEl.value) getSelection()?.selectAllChildren(urlEl.value);
  }
}

async function showOnTv() {
  if (await showShareCard(url)) {
    closeDialog();
    toast("The QR code is on the TV. Press “Done, back to my photos” when you're finished.");
  }
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
      <button v-if="isAdmin" :disabled="!!pending.share" @click="showOnTv">
        <template v-if="pending.share === 'showing'"><span class="spinner sm"></span>Putting it on the TV…</template>
        <template v-else>Show this QR code on the TV</template>
      </button>
      <button class="primary" @click="close">Done</button>
    </template>
  </ModalDialog>
</template>
