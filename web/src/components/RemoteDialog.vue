<script setup lang="ts">
// A remote control for the selected TV. Buttons are pressed straight away (they don't wait in
// the TV's queue of changes). Holding power turns the TV off; volume, channel and the arrows
// repeat while held.
import { onMounted, onUnmounted, ref } from "vue";
import ModalDialog from "./ModalDialog.vue";
import { S, pressKey } from "../state";

/** How long a press must be to count as holding power. */
const HOLD_MS = 700;
/** How long power is held down on the TV to turn it off. */
const POWER_OFF_MS = 3500;
const REPEAT = new Set([
  "KEY_UP",
  "KEY_DOWN",
  "KEY_LEFT",
  "KEY_RIGHT",
  "KEY_VOLUP",
  "KEY_VOLDOWN",
  "KEY_CHUP",
  "KEY_CHDOWN",
]);

const last = ref("");
const holding = ref(false);
let downAt = 0;
let repeat: ReturnType<typeof setTimeout> | undefined;
let holdTimer: ReturnType<typeof setTimeout> | undefined;

async function send(key: string, label: string, holdMs?: number) {
  last.value = label;
  await pressKey(key, holdMs);
}

function down(key: string, label: string, e: PointerEvent) {
  (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  downAt = Date.now();
  if (key === "KEY_POWER") {
    holdTimer = setTimeout(() => (holding.value = true), HOLD_MS);
    return;
  }
  send(key, label);
  if (REPEAT.has(key)) {
    const again = () => {
      send(key, label);
      repeat = setTimeout(again, 220);
    };
    repeat = setTimeout(again, 500);
  }
}

function up(key: string, label: string) {
  clearTimeout(repeat);
  clearTimeout(holdTimer);
  if (key !== "KEY_POWER" || !downAt) return;
  const held = Date.now() - downAt >= HOLD_MS;
  downAt = 0;
  holding.value = false;
  if (held) send(key, "Power (held: turning off)", POWER_OFF_MS);
  else send(key, label);
}

function cancel() {
  clearTimeout(repeat);
  clearTimeout(holdTimer);
  downAt = 0;
  holding.value = false;
}

// The keyboard works too: arrows, Enter, and Backspace for back.
const KEYS: Record<string, [string, string]> = {
  ArrowUp: ["KEY_UP", "Up"],
  ArrowDown: ["KEY_DOWN", "Down"],
  ArrowLeft: ["KEY_LEFT", "Left"],
  ArrowRight: ["KEY_RIGHT", "Right"],
  Enter: ["KEY_ENTER", "OK"],
  Backspace: ["KEY_RETURN", "Back"],
};
function onKey(e: KeyboardEvent) {
  const k = KEYS[e.key];
  if (!k || (e.target as HTMLElement).closest("input, select, textarea")) return;
  e.preventDefault();
  send(...k);
}
onMounted(() => addEventListener("keydown", onKey));
onUnmounted(() => {
  cancel();
  removeEventListener("keydown", onKey);
});

type Btn = [key: string, label: string, text: string, cls?: string];
const top: Btn[] = [
  ["KEY_SOURCE", "Source", "Source"],
  ["KEY_HOME", "Home", "⌂ Home"],
  ["KEY_MENU", "Menu", "Menu"],
];
const pad: (Btn | null)[] = [
  null,
  ["KEY_UP", "Up", "▲"],
  null,
  ["KEY_LEFT", "Left", "◀"],
  ["KEY_ENTER", "OK", "OK", "ok"],
  ["KEY_RIGHT", "Right", "▶"],
  ["KEY_RETURN", "Back", "↩ Back", "small"],
  ["KEY_DOWN", "Down", "▼"],
  ["KEY_EXIT", "Exit", "Exit", "small"],
];
const rockers: [Btn, Btn, string][] = [
  [["KEY_VOLUP", "Volume up", "+"], ["KEY_VOLDOWN", "Volume down", "−"], "Vol"],
  [["KEY_CHUP", "Channel up", "▲"], ["KEY_CHDOWN", "Channel down", "▼"], "Ch"],
];
const media: Btn[] = [
  ["KEY_REWIND", "Rewind", "⏪"],
  ["KEY_PLAY", "Play", "▶"],
  ["KEY_PAUSE", "Pause", "⏸"],
  ["KEY_FF", "Fast forward", "⏩"],
];
</script>

<template>
  <ModalDialog :title="`${S.tv} remote`">
    <div class="remote" @pointercancel="cancel">
      <div class="rm-row">
        <button
          class="rm-power"
          :class="{ holding }"
          aria-label="Power"
          @pointerdown="down('KEY_POWER', 'Power', $event)"
          @pointerup="up('KEY_POWER', 'Power')"
          @contextmenu.prevent
        >
          ⏻
        </button>
        <span class="muted small grow">{{
          holding ? "Let go to turn it off" : "Tap: art mode ↔ TV. Hold: turn off."
        }}</span>
        <button
          class="rm"
          aria-label="Mute"
          @pointerdown="down('KEY_MUTE', 'Mute', $event)"
          @pointerup="up('KEY_MUTE', 'Mute')"
        >
          🔇
        </button>
      </div>
      <div class="rm-row">
        <button
          v-for="[k, l, t] in top"
          :key="k"
          class="rm grow"
          @pointerdown="down(k, l, $event)"
          @pointerup="up(k, l)"
        >
          {{ t }}
        </button>
      </div>
      <div class="rm-pad">
        <template v-for="(b, i) in pad" :key="i">
          <span v-if="!b"></span>
          <button
            v-else
            class="rm"
            :class="b[3]"
            :aria-label="b[1]"
            @pointerdown="down(b[0], b[1], $event)"
            @pointerup="up(b[0], b[1])"
            @contextmenu.prevent
          >
            {{ b[2] }}
          </button>
        </template>
      </div>
      <div class="rm-row">
        <div v-for="[a, b, name] in rockers" :key="name" class="rm-rocker">
          <button class="rm" :aria-label="a[1]" @pointerdown="down(a[0], a[1], $event)" @pointerup="up(a[0], a[1])">
            {{ a[2] }}
          </button>
          <span class="muted small">{{ name }}</span>
          <button class="rm" :aria-label="b[1]" @pointerdown="down(b[0], b[1], $event)" @pointerup="up(b[0], b[1])">
            {{ b[2] }}
          </button>
        </div>
      </div>
      <div class="rm-row">
        <button
          v-for="[k, l, t] in media"
          :key="k"
          class="rm grow"
          :aria-label="l"
          @pointerdown="down(k, l, $event)"
          @pointerup="up(k, l)"
        >
          {{ t }}
        </button>
      </div>
      <div class="muted small rm-last">{{ last ? `Sent: ${last}` : "Arrow keys, Enter and Backspace work too." }}</div>
    </div>
  </ModalDialog>
</template>
