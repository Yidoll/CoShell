import { computed, ref } from "vue";
import { defineStore } from "pinia";
import { health as loadHealth, type HealthStatus } from "@/lib/backend/api";

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export const useSystemStore = defineStore("system", () => {
  const health = ref<HealthStatus>();
  const error = ref("");
  const loading = ref(false);
  const lastCheckedAt = ref("");
  let requestId = 0;
  let activeRequest: AbortController | undefined;

  const online = computed(() => health.value?.state === "ready" && !error.value);

  async function refresh(): Promise<void> {
    const currentRequest = ++requestId;
    activeRequest?.abort();
    activeRequest = new AbortController();
    loading.value = true;
    error.value = "";
    try {
      const result = await loadHealth({ signal: activeRequest.signal });
      if (currentRequest !== requestId) return;
      health.value = result;
      lastCheckedAt.value = new Intl.DateTimeFormat("zh-CN", {
        hour: "2-digit",
        minute: "2-digit",
        second: "2-digit",
      }).format(new Date());
    } catch (cause) {
      if (currentRequest !== requestId || (cause instanceof DOMException && cause.name === "AbortError")) return;
      health.value = undefined;
      error.value = errorMessage(cause);
    } finally {
      if (currentRequest === requestId) loading.value = false;
    }
  }

  return { health, error, loading, lastCheckedAt, online, refresh };
});
