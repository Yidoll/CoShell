import { describe, expect, it } from "vitest";
import { detectBackendRuntime, isTauriRuntime } from "@/lib/backend/runtime";

describe("backend runtime detection", () => {
  it("uses HTTP for a normal browser global", () => {
    const scope = {} as typeof globalThis;
    expect(isTauriRuntime(scope)).toBe(false);
    expect(detectBackendRuntime(scope)).toBe("http");
  });

  it("uses Tauri when its runtime marker exists", () => {
    const scope = { __TAURI_INTERNALS__: {} } as unknown as typeof globalThis;
    expect(isTauriRuntime(scope)).toBe(true);
    expect(detectBackendRuntime(scope)).toBe("tauri");
  });
});
