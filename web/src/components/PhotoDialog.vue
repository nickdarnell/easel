<script setup lang="ts">
import { computed, ref } from "vue";
import ModalDialog from "./ModalDialog.vue";
import MatteSelect from "./MatteSelect.vue";
import { openReframe } from "../composer";
import { S, act, api, closeDialog, isAdmin, loadStatus, thumbUrl, tvPath } from "../state";
import { toast } from "../toasts";
import type { Deleted, MatteUpdate, Toggle } from "../types";
import { enc, prettyDate, prettyMatte } from "../util";

const props = defineProps<{ id: string }>();

const photo = computed(() => S.photos?.find((x) => x.id === props.id));
const matte = ref(photo.value?.matte || "none");
const armed = ref(false);
const path = (rest = "") => tvPath(S.tv!, `/photos/${enc(props.id)}${rest}`);
const fav = S.filter === "favorites";

const show = () =>
  act(async () => {
    await api(path("/show"), { method: "POST" });
    closeDialog();
    toast("Showing it now");
    setTimeout(loadStatus, 800);
  });
const applyMatte = () =>
  act(async () => {
    const m = matte.value;
    await api(path("/matte"), { json: { matte: m } satisfies MatteUpdate });
    if (photo.value) photo.value.matte = m;
    toast("Matte changed");
  });
const favorite = () =>
  act(async () => {
    await api(path("/favorite"), { json: { on: !fav } satisfies Toggle });
    toast(fav ? "Removed from favorites" : "Added to favorites");
    closeDialog();
  });
function remove() {
  if (!armed.value) {
    armed.value = true;
    return;
  }
  act(async () => {
    const r = await api<Deleted>(path(), { method: "DELETE" });
    closeDialog();
    toast(r.moved_source_to ? "Deleted (the file was moved to .removed in the folder)" : "Deleted");
    if (S.photos) S.photos = S.photos.filter((x) => x.id !== props.id);
  });
}
</script>

<template>
  <ModalDialog v-if="photo" :title="photo.source || 'Photo'" wide>
    <div class="detail">
      <div class="pic"><img :src="thumbUrl(S.tv!, photo.id)" alt="" /></div>
      <div class="stack">
        <dl class="kv">
          <template v-if="photo.date"
            ><dt>Taken</dt>
            <dd>{{ prettyDate(photo.date) }}</dd></template
          >
          <template v-if="photo.width"
            ><dt>Size</dt>
            <dd>{{ photo.width }}×{{ photo.height }}</dd></template
          >
          <dt>Matte</dt>
          <dd>{{ prettyMatte(photo.matte) }}</dd>
          <template v-if="photo.framing">
            <dt>Framing</dt>
            <dd>
              {{ photo.framing.fit === "cover" ? "Fills the screen" : "Whole photo"
              }}{{ photo.framing.crop ? ", cropped" : "" }}
            </dd>
          </template>
          <template v-if="photo.source"
            ><dt>File</dt>
            <dd>{{ photo.source }}</dd></template
          >
          <dt>ID</dt>
          <dd class="muted">{{ photo.id }}</dd>
        </dl>
        <template v-if="isAdmin">
          <div class="row">
            <button class="primary grow" @click="show">Show on the TV</button>
            <button v-if="photo.reframe" @click="openReframe(photo)">Reframe…</button>
          </div>
          <div class="stack">
            <span class="muted small">Matte</span>
            <div class="row">
              <MatteSelect v-model="matte" :mattes="S.mattes" /><button @click="applyMatte">Apply</button>
            </div>
          </div>
          <div class="row">
            <button @click="favorite">{{ fav ? "Remove from favorites" : "Add to favorites" }}</button>
            <span class="grow"></span>
            <button class="danger" :class="{ primary: armed }" @click="remove">
              {{ armed ? "Really delete?" : "Delete" }}
            </button>
          </div>
        </template>
      </div>
    </div>
  </ModalDialog>
</template>
