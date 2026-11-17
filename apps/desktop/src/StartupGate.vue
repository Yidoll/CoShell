<script setup lang="ts">
import { onMounted, ref } from "vue";
import App from "@/App.vue";
import { useSystemStore } from "@/stores/systemStore";

const system = useSystemStore();
const ready = ref(false);

onMounted(async () => {
  try {
    await system.refresh();
  } finally {
    ready.value = true;
  }
});
</script>

<template>
  <main v-if="!ready" class="startup" aria-busy="true" aria-live="polite">
    <div class="startup__mark" aria-hidden="true">C</div>
    <div>
      <h1>CoShell</h1>
      <p>正在连接共享 Core</p>
    </div>
  </main>
  <App v-else />
</template>
