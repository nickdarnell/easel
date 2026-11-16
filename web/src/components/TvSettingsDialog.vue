<script setup lang="ts">
import { computed, ref } from "vue";
import ModalDialog from "./ModalDialog.vue";
import MatteSelect from "./MatteSelect.vue";
import { S, act, api, closeDialog, curTv, loadTvs, tvPath } from "../state";
import { toast } from "../toasts";
import type { SyncRequest, TvUpdate } from "../types";

const tv = curTv.value!;
const fitOptions: [string, string][] = [
  ["", "Automatic: fill the screen when the photo is close to its shape"],
  ["cover", "Fill the screen (crop, no border)"],
  ["contain", "Show the whole photo (with a border)"],
  ["original", "Leave as is"],
];
const fit = ref(tv.fit || "");
const folder = ref(tv.folder || "");
const matte = ref(tv.matte || "auto");
const portrait = ref(tv.portrait_matte || "shadowbox_polar");
const syncDelete = ref(false);
const forgetArmed = ref(false);
// Mattes are only offered once the TV has listed them (they arrive after the dialog opens).
const mattes = computed(() => S.mattes);

const save = () =>
  act(async () => {
    const body: TvUpdate = { fit: fit.value, folder: folder.value };
    if (mattes.value) {
      body.matte = matte.value;
      body.portrait_matte = portrait.value;
    }
    await api(tvPath(tv.name), { method: "PATCH", json: body });
    closeDialog();
    toast("Saved");
    await loadTvs();
  });
const sync = () =>
  act(async () => {
    await api(tvPath(tv.name, "/sync"), { json: { delete: syncDelete.value } satisfies SyncRequest });
    closeDialog();
    toast("Syncing…");
  });
function forget() {
  if (!forgetArmed.value) {
    forgetArmed.value = true;
    return;
  }
  act(async () => {
    await api(tvPath(tv.name), { method: "DELETE" });
    closeDialog();
    S.tv = null;
    await loadTvs();
  });
}
</script>

<template>
  <ModalDialog :title="`${tv.name} settings`">
    <div class="form">
      <label>Address</label>
      <div>
        {{ tv.host }} <span class="muted small">{{ tv.model || "" }} · {{ tv.resolution?.join("×") || "" }}</span>
      </div>
      <label>Usual framing</label>
      <select v-model="fit">
        <option v-for="[v, l] in fitOptions" :key="v" :value="v">{{ l }}</option>
      </select>
      <template v-if="mattes">
        <label>Default matte</label>
        <div class="row"><MatteSelect v-model="matte" :mattes="mattes" allow-auto /></div>
        <label>Portrait matte</label>
        <div class="row"><MatteSelect v-model="portrait" :mattes="mattes" /></div>
      </template>
      <label>Photo folder</label>
      <div class="stack">
        <input v-model="folder" type="text" placeholder="e.g. /photos/living-room" />
        <span class="muted small"
          >A folder on the server. Sent photos are saved here, and “Sync” uploads anything new in it.</span
        >
      </div>
    </div>
    <div v-if="tv.folder" class="row">
      <button @click="sync">Sync folder now</button>
      <label class="check"
        ><input v-model="syncDelete" type="checkbox" /> also remove photos deleted from the folder</label
      >
    </div>
    <template #foot="{ close }">
      <button class="danger" @click="forget">{{ forgetArmed ? "Really forget it?" : "Forget this TV" }}</button>
      <span class="grow"></span>
      <button @click="close">Cancel</button>
      <button class="primary" @click="save">Save</button>
    </template>
  </ModalDialog>
</template>
