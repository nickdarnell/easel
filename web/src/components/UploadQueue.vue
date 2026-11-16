<script setup lang="ts">
import { computed } from "vue";
import { clearFinished, queue } from "../upload";

const title = computed(() => {
  const items = queue.items;
  const done = items.filter((i) => i.status === "done").length;
  const failed = items.filter((i) => i.status === "failed").length;
  if (items.length - done - failed) return `Sending ${done + failed + 1} of ${items.length}`;
  return failed ? `${failed} failed, ${done} sent` : `All ${done} sent`;
});
const multi = computed(() => new Set(queue.items.map((i) => i.tv)).size > 1);
</script>

<template>
  <div v-if="queue.items.length" class="queue">
    <div class="qhead">
      <span>{{ title }}</span><button class="x" aria-label="Close" @click="clearFinished">×</button>
    </div>
    <div class="qlist">
      <div v-for="(i, n) in queue.items" :key="n" class="qitem" :class="i.status">
        <div class="row">
          <span class="name"
            >{{ i.file.name }}<span v-if="multi" class="muted"> → {{ i.tv }}</span></span
          >
          <span class="st">{{ i.msg }}</span>
        </div>
        <div v-if="i.status === 'sending'" class="bar" :class="{ indet: i.pct >= 100 }">
          <i :style="{ width: i.pct + '%' }"></i>
        </div>
      </div>
    </div>
  </div>
</template>
