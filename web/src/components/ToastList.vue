<script setup lang="ts">
import { toasts } from "../toasts";
</script>

<template>
  <div id="toasts">
    <div v-for="t in toasts" :key="t.id" class="toast" :class="{ err: t.err }">{{ t.text }}</div>
  </div>
</template>
