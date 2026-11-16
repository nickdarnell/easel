<script setup lang="ts">
import { ref } from "vue";
import ModalDialog from "./ModalDialog.vue";
import type { AddTv, Discovered, Paired } from "../types";
import { S, act, api, closeDialog, loadTvs, selectTv } from "../state";
import { errorMessage } from "../util";

const host = ref("");
const name = ref("");
const searching = ref(false);
const searched = ref(false);
const found = ref<Discovered[]>([]);
const busy = ref(false);
const pairing = ref(false);
const paired = ref<Paired | null>(null);
const error = ref("");

const pair = (h: string, n: string | null) =>
  act(async () => {
    pairing.value = busy.value = true;
    paired.value = null;
    error.value = "";
    try {
      const r = await api<Paired>("/api/tvs", { json: { host: h, name: n } satisfies AddTv });
      paired.value = r;
      await loadTvs();
      if (!S.guestTv) selectTv(r.name);
      setTimeout(closeDialog, 1500);
    } catch (e) {
      error.value = errorMessage(e);
      throw e;
    } finally {
      pairing.value = busy.value = false;
    }
  });

function pairTyped() {
  const h = host.value.trim();
  if (h) pair(h, name.value.trim() || null);
}

const find = () =>
  act(async () => {
    searching.value = true;
    try {
      found.value = await api<Discovered[]>("/api/discover", { method: "POST" });
      searched.value = true;
    } finally {
      searching.value = false;
    }
  });
</script>

<template>
  <ModalDialog title="Add a TV">
    <div class="row">
      <button :disabled="searching || busy" @click="find">Find TVs on the network</button>
      <span class="muted small">
        <template v-if="searching">Searching… (about 5 seconds)</template>
        <template v-else-if="searched && !found.length">
          No TVs found. If the server runs in Docker, use host networking or enter the address below.
        </template>
      </span>
    </div>
    <div class="found">
      <div v-for="f in found" :key="f.host" class="item">
        <span class="dot" :class="f.power"></span>
        <div class="grow">
          <b>{{ f.name }}</b>
          <span class="muted small"> {{ f.host }} · {{ f.model || "" }}{{ f.frame ? " · Frame" : "" }}</span>
        </div>
        <span v-if="f.paired" class="muted small">Paired</span>
        <button v-else :disabled="busy" @click="pair(f.host, f.name)">Pair</button>
      </div>
    </div>
    <div class="stack">
      <span class="muted small"
        >Or enter its address (TV: Settings → General → Network → Network Status → IP Settings)</span
      >
      <div class="row">
        <input
          v-model="host"
          type="text"
          placeholder="192.168.1.50"
          class="grow"
          inputmode="decimal"
          @keydown.enter="pairTyped"
        />
        <input v-model="name" type="text" placeholder="Name (optional)" class="grow" />
        <button class="primary" :disabled="busy" @click="pairTyped">Pair</button>
      </div>
    </div>
    <div v-if="pairing" class="pairing">
      <div class="spinner"></div>
      <div>
        <b>Look at the TV</b> and choose <b>Allow</b> when it asks.
        <div class="muted small">Waiting up to a minute…</div>
      </div>
    </div>
    <div v-else-if="paired" class="pairing">
      ✓
      <div>
        <b>{{ paired.name }}</b> is ready{{ paired.photos != null ? `, with ${paired.photos} photos` : "" }}.
        <div v-if="!paired.frame" class="muted small">
          It doesn't report art mode support, so some features may not work.
        </div>
      </div>
    </div>
    <div v-else-if="error" class="notice err">{{ error }}</div>
  </ModalDialog>
</template>
