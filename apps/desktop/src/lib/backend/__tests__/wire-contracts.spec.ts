import { describe, expect, expectTypeOf, it } from "vitest";
import type * as HttpTransport from "@/lib/backend/http";
import type * as TauriTransport from "@/lib/backend/tauri";
import type {
  Backend,
  ConnectionProfile,
  ErrorEnvelope,
  Metric,
  RiskAssessment,
  SessionEvent,
} from "@/lib/backend/types";

describe("shared backend wire contracts", () => {
  it("exposes the same backend interface on both transports", () => {
    expectTypeOf<typeof HttpTransport>().toMatchTypeOf<Backend>();
    expectTypeOf<typeof TauriTransport>().toMatchTypeOf<Backend>();
  });

  it("models a public connection without credential fields", () => {
    const connection: ConnectionProfile = {
      id: "c1",
      name: "server",
      host: "example.org",
      port: 22,
      username: "user",
      authentication: "password",
      groupId: null,
      notes: "",
      production: true,
      alwaysConfirm: false,
      version: 1,
    };
    expectTypeOf<"password">().not.toExtend<keyof ConnectionProfile>();
    expectTypeOf<"privateKey">().not.toExtend<keyof ConnectionProfile>();
    expect(Object.keys(connection)).not.toContain("password");
  });

  it("keeps unavailable metrics distinct from numeric zero", () => {
    const zero: Metric = { value: 0, unit: "%", unavailableReason: null };
    const missing: Metric = { value: null, unit: "%", unavailableReason: "unsupported" };
    expect(zero.value).toBe(0);
    expect(missing.value).toBeNull();
  });

  it("preserves event discrimination and stable risk and error values", () => {
    const event: SessionEvent = { sessionId: "s1", sequence: 17, kind: "state", payload: { state: "disconnected" } };
    const risk: RiskAssessment = { level: "dangerous", categories: ["recursive_delete"], ruleVersion: "1", displaySpans: [], redactedDigest: "sha256:abc" };
    const error: ErrorEnvelope = { code: "CONFIRMATION_REQUIRED", messageKey: "guard.confirm", safeDetail: null, operationId: "op1", outcome: "not_started", retryable: false };
    expect(event.payload.state).toBe("disconnected");
    expect(risk.level).toBe("dangerous");
    expect(error.outcome).toBe("not_started");
  });
});
