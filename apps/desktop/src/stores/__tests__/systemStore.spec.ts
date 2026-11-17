import { createPinia, setActivePinia } from "pinia";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({ health: vi.fn() }));
vi.mock("@/lib/backend/api", () => ({ health: mocks.health }));

import { useSystemStore } from "@/stores/systemStore";

describe("system store", () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    mocks.health.mockReset();
  });

  it("publishes a healthy core result", async () => {
    mocks.health.mockResolvedValue({
      service: "coshell-core",
      version: "0.1.0",
      state: "ready",
      components: [{ name: "core", state: "ready", detail: "shared application orchestration" }],
    });
    const store = useSystemStore();

    await store.refresh();

    expect(store.online).toBe(true);
    expect(store.health?.service).toBe("coshell-core");
    expect(store.error).toBe("");
    expect(store.loading).toBe(false);
  });

  it("keeps a transport failure visible for retry", async () => {
    mocks.health.mockRejectedValue(new Error("offline"));
    const store = useSystemStore();

    await store.refresh();

    expect(store.online).toBe(false);
    expect(store.health).toBeUndefined();
    expect(store.error).toBe("offline");
  });
});
