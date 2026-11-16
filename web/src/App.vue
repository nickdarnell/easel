<script setup lang="ts">
import { type Component, computed, onMounted, ref, watchEffect } from "vue";
import AddTvDialog from "./components/AddTvDialog.vue";
import AppSettingsDialog from "./components/AppSettingsDialog.vue";
import ComposerDialog from "./components/ComposerDialog.vue";
import HeroPanel from "./components/HeroPanel.vue";
import PasswordDialog from "./components/PasswordDialog.vue";
import PhotoDialog from "./components/PhotoDialog.vue";
import PhotoGrid from "./components/PhotoGrid.vue";
import ShareDialog from "./components/ShareDialog.vue";
import ToastList from "./components/ToastList.vue";
import TvSettingsDialog from "./components/TvSettingsDialog.vue";
import UploadQueue from "./components/UploadQueue.vue";
import { composer, composing, openComposer } from "./composer";
import { connectEvents } from "./events";
import {
  S,
  askPassword,
  curTv,
  dialog,
  isAdmin,
  loadInfo,
  loadTvs,
  openAppSettings,
  openDialog,
  selectTv,
  tvReady,
} from "./state";
import { errorMessage, store } from "./util";

const ready = ref(false);
const loadError = ref("");
const nagDone = ref(store.get("pwNagDone", false));

const dialogs: Record<string, Component> = {
  photo: PhotoDialog,
  tvSettings: TvSettingsDialog,
  appSettings: AppSettingsDialog,
  addTv: AddTvDialog,
  share: ShareDialog,
  password: PasswordDialog,
  composer: ComposerDialog,
};
const dialogProps = computed(() => {
  const d = dialog.value;
  if (d?.kind === "photo") return { id: d.id };
  if (d?.kind === "appSettings") return { settings: d.settings };
  return {};
});

const showNag = computed(() => isAdmin.value && !S.info?.password_set && !nagDone.value);
const showLock = computed(() => !!S.info?.password_set && !S.info.admin && !S.guestTv);
function dismissNag() {
  store.set("pwNagDone", true);
  nagDone.value = true;
}

watchEffect(() => {
  if (S.tv) document.title = S.guestTv ? `Send to ${S.tv}` : `${S.tv} · Easel`;
});

// ── drag & drop anywhere, and pasting images ─────────────────────────
const dragDepth = ref(0);
const hasFiles = (e: DragEvent) => [...(e.dataTransfer?.types || [])].includes("Files");
const canReceive = () => !!S.tv && tvReady.value && (!dialog.value || composing());
const targets = () => composer.value?.tvs || [S.tv!];

addEventListener("dragenter", (e) => {
  if (!hasFiles(e) || !canReceive()) return;
  e.preventDefault();
  dragDepth.value++;
});
addEventListener("dragover", (e) => {
  if (hasFiles(e)) e.preventDefault();
});
addEventListener("dragleave", () => {
  if (--dragDepth.value <= 0) dragDepth.value = 0;
});
addEventListener("drop", (e) => {
  if (!hasFiles(e)) return;
  e.preventDefault();
  dragDepth.value = 0;
  if (canReceive()) openComposer([...e.dataTransfer!.files], targets());
});
addEventListener("paste", (e) => {
  const files = [...(e.clipboardData?.files || [])];
  const inField = (e.target as Element | null)?.closest?.("input,textarea");
  if (files.length && !inField && canReceive()) openComposer(files, targets());
});

// ── start ────────────────────────────────────────────────────────────
onMounted(async () => {
  try {
    await loadInfo();
    if (S.pw && !S.info!.admin && S.info!.password_set) {
      S.pw = "";
      store.set("pw", "");
    }
    await loadTvs();
  } catch (e) {
    loadError.value = errorMessage(e);
    return;
  }
  ready.value = true;
  connectEvents();
  // Keep power states fresh (TVs go to sleep with their motion sensors).
  setInterval(() => loadTvs().catch(() => {}), 60000);
});
</script>

<template>
  <header class="top">
    <div class="brand">
      <svg viewBox="0 0 26 20" aria-hidden="true">
        <rect x="1" y="1" width="24" height="18" rx="1.5" fill="none" stroke="#c9a96e" stroke-width="2" />
        <rect x="5" y="5" width="16" height="10" fill="#3a4a5c" />
      </svg>
      <span>Easel</span>
    </div>
    <nav id="tabs">
      <template v-if="!S.guestTv">
        <button
          v-for="t in S.tvs"
          :key="t.name"
          class="tab"
          :class="{ active: t.name === S.tv }"
          @click="selectTv(t.name)"
        >
          <span class="dot" :class="t.power"></span>{{ t.name }}
        </button>
      </template>
    </nav>
    <div class="top-actions">
      <button v-if="showLock" class="ghost" @click="askPassword">Unlock</button>
      <template v-if="isAdmin">
        <button class="ghost" @click="openDialog({ kind: 'addTv' })">+ Add TV</button>
        <button class="ghost" title="App settings" aria-label="App settings" @click="openAppSettings">⚙</button>
      </template>
    </div>
  </header>

  <main>
    <div v-if="loadError" class="notice err">Couldn't reach the server: {{ loadError }}</div>
    <div v-else-if="!ready" class="empty"><div class="spinner" style="margin: 0 auto"></div></div>
    <template v-else>
      <div v-if="showNag" class="notice row">
        <span class="grow">No admin password yet, so anyone on your network can delete photos or change settings.</span>
        <button @click="openAppSettings">Set a password</button>
        <button class="ghost" @click="dismissNag">Not now</button>
      </div>
      <div v-if="!S.tvs.length" class="empty">
        <h3>No TVs yet</h3>
        <p v-if="S.info?.admin">Add your Frame TV to get started. You'll need to press <b>Allow</b> on the TV.</p>
        <p v-else>Nobody has paired a TV with this app yet.</p>
        <button v-if="isAdmin" class="primary big" @click="openDialog({ kind: 'addTv' })">Add a TV</button>
      </div>
      <div v-else-if="!curTv" class="empty">
        <h3>Unknown TV</h3>
        <p>There's no TV called “{{ S.guestTv }}”.</p>
      </div>
      <template v-else>
        <HeroPanel :tv="curTv" />
        <PhotoGrid :tv="curTv" />
      </template>
    </template>
    <footer v-if="S.info" class="version">
      Easel {{ S.info.version }}
      <template v-if="S.info.commit">
        ·
        <a :href="`https://github.com/nickdarnell/easel/commit/${S.info.commit}`" target="_blank" rel="noopener">{{
          S.info.commit
        }}</a>
      </template>
    </footer>
  </main>

  <div v-if="dragDepth > 0" class="drop">
    <div>
      Drop to send to<br /><b>{{ S.tv }}</b>
    </div>
  </div>
  <UploadQueue />
  <ToastList />
  <component :is="dialogs[dialog.kind]" v-if="dialog" :key="dialog.key" v-bind="dialogProps" />
</template>
