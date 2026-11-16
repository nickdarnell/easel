<script setup lang="ts">
// Server-wide settings, stored in the config folder.
import { computed, ref } from "vue";
import ModalDialog from "./ModalDialog.vue";
import type { Info, Settings, SettingsUpdate } from "../types";
import { S, act, api, closeDialog } from "../state";
import { toast } from "../toasts";
import { store } from "../util";

const props = defineProps<{ settings: Settings }>();
const s = props.settings;

const password = ref("");
const removePassword = ref(false);
const publicUrl = ref(s.public_url || "");
const localsend = ref(s.localsend);
const pin = ref(s.localsend_pin || "");
const origin = location.origin;
const aliases = computed(() => s.localsend_targets.map((t) => `“${t.alias}”`).join(", "));

const save = () =>
  act(async () => {
    const body: SettingsUpdate = {
      public_url: publicUrl.value.trim(),
      localsend: localsend.value,
      localsend_pin: pin.value.trim(),
    };
    let newPw: string | undefined;
    if (removePassword.value) newPw = "";
    else if (password.value) newPw = password.value;
    if (newPw !== undefined) body.password = newPw;
    await api("/api/settings", { method: "PUT", json: body });
    if (newPw !== undefined) {
      S.pw = newPw;
      store.set("pw", newPw);
    }
    S.info = await api<Info>("/api/info");
    closeDialog();
    toast(newPw ? "Saved. Use the new password on your other devices." : "Saved");
  });
</script>

<template>
  <ModalDialog title="App settings">
    <div class="form">
      <label>Admin password</label>
      <div class="stack">
        <input
          v-model="password"
          type="password"
          autocomplete="new-password"
          :placeholder="s.password_set ? 'New password (blank keeps the current one)' : 'Choose a password'"
        />
        <span class="muted small">{{
          s.password_set
            ? "Guests can still see and send photos without it."
            : "Without one, anyone on your network can delete photos or change settings. Guests never need it to send photos."
        }}</span>
        <label v-if="s.password_set" class="check"
          ><input v-model="removePassword" type="checkbox" /> Remove the password</label
        >
      </div>
      <label>Share address</label>
      <div class="stack">
        <div class="row">
          <input v-model="publicUrl" type="text" class="grow" :placeholder="origin" />
          <button @click="publicUrl = origin">Use this address</button>
        </div>
        <span class="muted small"
          >The start of guest links and QR codes. Leave blank to use the address this page was opened with.</span
        >
      </div>
      <label>LocalSend</label>
      <div class="stack">
        <label class="check"><input v-model="localsend" type="checkbox" /> List each TV in the LocalSend app</label>
        <input v-model="pin" type="text" placeholder="PIN senders must enter (optional)" />
        <span v-if="s.localsend_error" class="small" style="color: var(--danger)">{{ s.localsend_error }}</span>
        <span v-else-if="s.localsend_targets.length" class="muted small">Showing up as {{ aliases }}.</span>
        <span v-else-if="s.localsend" class="muted small">Starts once a TV is paired.</span>
      </div>
    </div>
    <p class="muted small" style="margin: 0">
      Saved in the server's config folder{{ s.in_docker ? " (the /config volume)" : "" }}.
    </p>
    <template #foot="{ close }">
      <button @click="close">Cancel</button>
      <button class="primary" @click="save">Save</button>
    </template>
  </ModalDialog>
</template>
