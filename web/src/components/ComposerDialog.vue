<script setup lang="ts">
// The framing composer: a to-scale preview of the TV with each photo filling the screen or inside a matte.
import { computed, onMounted, onUnmounted, ref } from "vue";
import ModalDialog from "./ModalDialog.vue";
import { MATTE_COLORS, MATTE_OPENING, type Mode, clampCenter, cropOf, stageLayout, trimmedPercent } from "../framing";
import { composer, disposeComposer, removeItem, saveReframe, send, useForAll } from "../composer";
import { S, getMattes } from "../state";
import { toast } from "../toasts";
import { cap, clamp, matteParts, prettyMatte } from "../util";

const c = composer.value!;
const isSend = c.kind === "send";
const title = isSend ? (c.tvs.length > 1 ? `Send to ${c.tvs.length} TVs` : `Send to ${c.tvs[0]}`) : "Reframe";
const saving = ref(false);
const stage = ref<HTMLElement>();

onMounted(() => {
  if (!S.mattes) getMattes();
});
onUnmounted(() => disposeComposer(c));

const sel = computed(() => clamp(c.sel, 0, c.items.length - 1));
const it = computed(() => c.items[sel.value]);
const many = computed(() => c.items.length > 1);
const layout = computed(() => (it.value?.url ? stageLayout(it.value, c.pa) : null));
const pct = (v: number) => v * 100 + "%";
const place = (r: { x: number; y: number; w: number; h: number }) => ({
  left: pct(r.x),
  top: pct(r.y),
  width: pct(r.w),
  height: pct(r.h),
});
const imgShadow = computed(() =>
  !layout.value || layout.value.fill || layout.value.shadowbox || it.value?.mode === "blur"
    ? "none"
    : "0 0 0 1px rgba(0,0,0,.12)",
);

const modes: [Mode, string][] = [
  ["fill", "Fill the screen"],
  ["whole", "Show the whole photo"],
  ["blur", "Blurred background"],
];
const types = computed(() =>
  (S.mattes?.types.length ? S.mattes.types : Object.keys(MATTE_OPENING)).filter((t) => t !== "none"),
);
const colors = computed(() => (S.mattes?.colors.length ? S.mattes.colors : Object.keys(MATTE_COLORS)));
const matteType = computed(() => matteParts(it.value?.matte)[0]);
const matteColor = computed(() => matteParts(it.value?.matte)[1]);
const usual = computed(() => {
  const p = it.value?.pref;
  return p && p !== "auto" && p !== "none" ? ` This TV usually uses ${prettyMatte(p).toLowerCase()}.` : "";
});
const fillNote = computed(() => {
  const lost = it.value?.url ? trimmedPercent(it.value, c.pa) : 0;
  return lost > 1
    ? `No border. Drag the photo to choose what stays on screen (about ${lost}% is trimmed).`
    : "No border. This photo already matches the screen's shape.";
});
const goLabel = computed(() => {
  if (!isSend) return saving.value ? "Saving to the TV…" : "Save framing";
  const n = c.items.length;
  return `Send ${n > 1 ? n + " photos" : "photo"}`;
});
const badge = (i: (typeof c.items)[number]) =>
  i.error ? "Usual" : i.mode === "blur" ? "Blur" : i.mode === "fill" ? "Fill" : i.mode ? "Border" : "…";

function setZoom(z: number) {
  const i = it.value!;
  i.zoom = clamp(z, 1, 4);
  clampCenter(i, c.pa);
}
function reset() {
  const i = it.value!;
  i.zoom = 1;
  i.cx = i.cy = 0.5;
}
const setMode = (m: Mode) => (it.value!.mode = m);
const setType = (t: string) => (it.value!.matte = `${t}_${matteColor.value || "polar"}`);
const setColor = (col: string) => (it.value!.matte = `${matteType.value}_${col}`);
const valueOf = (e: Event) => (e.target as HTMLInputElement | HTMLSelectElement).value;

function allLikeThis() {
  useForAll(c);
  const i = it.value!;
  toast(
    `All ${c.items.length} photos: ${i.mode === "blur" ? "blurred background" : i.mode === "fill" ? "fill the screen" : prettyMatte(i.matte)}`,
  );
}

async function go() {
  if (isSend) {
    send(c);
  } else {
    saving.value = true;
    await saveReframe(c);
    saving.value = false;
  }
}

// ── dragging, pinching and wheel-zooming the photo when it fills the screen ──
const pointers = new Map<number, { x: number; y: number }>();
const panning = computed(() => !!it.value?.url && it.value.mode === "fill");

function down(e: PointerEvent) {
  if (!panning.value) return;
  stage.value!.setPointerCapture(e.pointerId);
  pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
}
function move(e: PointerEvent) {
  const prev = pointers.get(e.pointerId);
  if (!prev) return;
  const i = it.value!;
  const now = { x: e.clientX, y: e.clientY };
  const r = stage.value!.getBoundingClientRect();
  if (pointers.size === 1) {
    const k = cropOf(i, c.pa);
    i.cx -= ((now.x - prev.x) * k.w) / r.width;
    i.cy -= ((now.y - prev.y) * k.h) / r.height;
    clampCenter(i, c.pa);
  } else if (pointers.size === 2) {
    const other = [...pointers].find(([id]) => id !== e.pointerId)![1];
    const d0 = Math.hypot(prev.x - other.x, prev.y - other.y);
    const d1 = Math.hypot(now.x - other.x, now.y - other.y);
    if (d0 > 0) setZoom((i.zoom * d1) / d0);
  }
  pointers.set(e.pointerId, now);
}
const up = (e: PointerEvent) => pointers.delete(e.pointerId);
function wheel(e: WheelEvent) {
  if (!panning.value) return;
  e.preventDefault();
  setZoom(it.value!.zoom * Math.exp(-e.deltaY * 0.0015));
}
</script>

<template>
  <ModalDialog :title="title" composer>
    <div class="cmp">
      <div class="bezel">
        <div
          ref="stage"
          class="stage"
          :class="{ pan: panning }"
          :style="{ '--pa': c.pa }"
          @pointerdown="down"
          @pointermove="move"
          @pointerup="up"
          @pointercancel="up"
          @wheel="wheel"
        >
          <div v-if="!it?.url || !layout" class="stage-msg">
            <template v-if="it?.error">{{ it.error }}</template>
            <div v-else class="spinner"></div>
          </div>
          <template v-else>
            <div class="mat" :style="{ background: layout.fill ? 'transparent' : layout.background }"></div>
            <img v-if="it.mode === 'blur'" class="blur-background" :src="it.url" alt="" draggable="false" />
            <img :src="it.url" alt="" draggable="false" :style="{ ...place(layout.img), boxShadow: imgShadow }" />
            <div v-if="!layout.fill && layout.shadowbox" class="shade" :style="place(layout.img)"></div>
          </template>
        </div>
      </div>

      <div class="cmp-tools">
        <template v-if="it?.url">
          <div class="seg">
            <button v-for="[m, label] in modes" :key="m" :class="{ on: it.mode === m }" @click="setMode(m)">
              {{ label }}<span v-if="it.suggest === m" class="sugg">Suggested</span>
            </button>
          </div>
          <template v-if="it.mode === 'fill'">
            <div class="row">
              <span class="muted small">Zoom</span>
              <input
                type="range"
                class="grow"
                min="1"
                max="4"
                step="0.01"
                :value="it.zoom"
                @input="setZoom(+valueOf($event))"
              />
              <button class="ghost" @click="reset">Reset</button>
            </div>
            <div class="muted small">{{ fillNote }}</div>
          </template>
          <template v-else-if="it.mode === 'whole'">
            <div class="row">
              <span class="muted small">Border</span>
              <select :value="matteType" @change="setType(valueOf($event))">
                <option v-for="t in types" :key="t" :value="t">{{ cap(t) }}</option>
              </select>
            </div>
            <div class="swatches">
              <button
                v-for="col in colors"
                :key="col"
                class="sw"
                :class="{ on: col === matteColor }"
                :title="col"
                :style="{ background: MATTE_COLORS[col] || '#888' }"
                @click="setColor(col)"
              ></button>
            </div>
            <div class="muted small">Nothing is cropped; the TV adds a border around it.{{ usual }}</div>
          </template>
          <div v-else class="muted small">Keep the whole photo sharp, with a blurred copy filling the background.</div>
        </template>
      </div>

      <div v-if="many" class="strip">
        <button
          v-for="(item, i) in c.items"
          :key="item.urls[0] || item.name + i"
          class="sthumb"
          :class="{ on: i === sel }"
          :title="item.name"
          @click="c.sel = i"
        >
          <img v-if="item.url" :src="item.url" alt="" />
          <span v-else-if="item.error" class="muted small">?</span>
          <span v-else class="spinner"></span>
          <span class="sbadge">{{ badge(item) }}</span>
          <span class="srm" title="Leave this one out" @click.stop="removeItem(c, i)">×</span>
        </button>
      </div>
    </div>

    <template #foot="{ close }">
      <template v-if="isSend">
        <span class="grow"></span>
        <button v-if="many" @click="allLikeThis">Use this framing for all</button>
      </template>
      <span v-else class="grow"></span>
      <button @click="close">Cancel</button>
      <button class="primary" :disabled="saving" @click="go">{{ goLabel }}</button>
    </template>
  </ModalDialog>
</template>
