export type BackendRuntime = "tauri" | "http";

export function isTauriRuntime(scope: typeof globalThis = globalThis): boolean {
  const host = scope as typeof globalThis & { __TAURI_INTERNALS__?: unknown; __TAURI__?: unknown };
  return Boolean(host.__TAURI_INTERNALS__ || host.__TAURI__);
}

export function detectBackendRuntime(scope: typeof globalThis = globalThis): BackendRuntime {
  return isTauriRuntime(scope) ? "tauri" : "http";
}
