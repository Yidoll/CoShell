import { afterEach, describe, expect, it, vi } from "vitest";
import { health } from "@/lib/backend/http";

describe("HTTP backend", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("loads the shared health endpoint", async () => {
    const payload = {
      service: "coshell-core",
      version: "0.1.0",
      state: "ready" as const,
      components: [{ name: "core", state: "ready" as const, detail: "shared application orchestration" }],
    };
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify(payload), { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);

    await expect(health()).resolves.toEqual(payload);
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/health",
      expect.objectContaining({ method: "GET", headers: { Accept: "application/json" } }),
    );
  });

  it("surfaces a useful status error", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(null, { status: 503 })));
    await expect(health()).rejects.toThrow("HTTP 503");
  });
});
