<script setup lang="ts">
import { computed, onUnmounted, ref, watch } from "vue";
import ModalDialog from "./ModalDialog.vue";
import MatteSelect from "./MatteSelect.vue";
import { openReframe } from "../composer";
import {
  S,
  act,
  api,
  changesTo,
  closeDialog,
  isAdmin,
  pending,
  showPhoto,
  thumbRev,
  thumbUrl,
  thumbsArrived,
  tvPath,
  wantedMatte,
} from "../state";
import { toast } from "../toasts";
import type { MatteUpdate, Toggle } from "../types";
import { enc, prettyDate, prettyMatte, sleep } from "../util";

const props = defineProps<{ id: string }>();

const photo = computed(() => S.photos?.find((x) => x.id === props.id));
const matte = ref((photo.value && wantedMatte(photo.value)) || "none");
/** Changes to it that the TV hasn't made yet. */
const waiting = computed(() => changesTo(props.id));
const armed = ref(false);
const path = (rest = "") => tvPath(S.tv!, `/photos/${enc(props.id)}${rest}`);
const fav = S.filter === "favorites";

// The preview: shown, being waited for (the TV didn't send it yet), or one the TV doesn't have.
const pic = ref<"ok" | "waiting" | "none">("ok");
const why = ref("");
let open = true;
onUnmounted(() => (open = false));
async function previewFailed() {
  if (pic.value !== "ok") return;
  pic.value = "waiting";
  // Keep asking (the server waits for the TV) until it comes, or the TV says it has none.
  while (open && pic.value === "waiting") {
    const r = await fetch(thumbUrl(S.tv!, props.id), { cache: "no-store" }).catch(() => null);
    if (pic.value !== "waiting") return;
    if (r?.ok) return thumbsArrived([props.id]);
    if (r?.status === 404) {
      pic.value = "none";
      return;
    }
    why.value = r ? ((await r.json().catch(() => null))?.error ?? "") : "";
    await sleep(4000);
  }
}
// It came (from here, or the server got it late): show it.
watch(
  () => thumbRev.get(props.id),
  () => (pic.value = "ok"),
);
const shown = computed(() => S.status?.current?.content_id === props.id);
const show = () => showPhoto(props.id);
const applyMatte = () =>
  act(async () => {
    const m = matte.value;
    await api(path("/matte"), { json: { matte: m } satisfies MatteUpdate });
    toast("Changing the matte");
  });
const favorite = () =>
  act(async () => {
    await api(path("/favorite"), { json: { on: !fav } satisfies Toggle });
    toast(fav ? "Removing from favorites" : "Adding to favorites");
    closeDialog();
  });
function remove() {
  if (!armed.value) {
    armed.value = true;
    return;
  }
  act(async () => {
    await api(path(), { method: "DELETE" });
    closeDialog();
    toast("Deleting it");
  });
}
</script>

<template>
  <ModalDialog v-if="photo" :title="photo.source || 'Photo'" wide>
    <div class="detail">
      <div class="pic">
        <img v-if="pic === 'ok'" :src="thumbUrl(S.tv!, photo.id)" alt="" @error="previewFailed" />
        <span v-else-if="pic === 'waiting'" class="muted small waiting" :title="why"
          ><span class="spinner sm"></span>Waiting for {{ S.tv }} to send the preview…</span
        >
        <span v-else class="muted small">The TV has no preview of this photo.</span>
      </div>
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
          <dd>{{ prettyMatte(wantedMatte(photo)) }}</dd>
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
        <div v-for="c in waiting" :key="c.id" class="notice" :class="{ err: c.state === 'failed' }">
          <span v-if="c.state === 'working'" class="spinner sm"></span>{{ c.label }}:
          {{
            c.state === "failed"
              ? `failed (${c.error})`
              : c.state === "working"
                ? "doing it now"
                : `waiting for ${S.tv}`
          }}
        </div>
        <template v-if="isAdmin">
          <div class="row">
            <button class="primary grow" :disabled="!!pending.show || shown" @click="show">
              <template v-if="pending.show === photo.id"><span class="spinner sm"></span>Putting it on…</template>
              <template v-else>{{ shown ? "On the TV now" : "Show on the TV" }}</template>
            </button>
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
