import { execFileSync } from "node:child_process";
import path from "node:path";
import { describe, expect, it } from "vitest";
import type {
  BackupInfo, BackupScope, ConnectionGroup, ConnectionProfile, ConnectionTestResult,
  ErrorEnvelope, HostKeyChallenge, Metric, OperationStatus, RestorePlan, RestorePreview,
  RiskAssessment, ServerOverviewSnapshot, SessionEvent, SessionInfo, WebDavProfile,
  CommandHistoryEntry,
} from "@/lib/backend/types";

interface RustWireExample {
  connection: ConnectionProfile;
  group: ConnectionGroup;
  connectionTest: ConnectionTestResult;
  hostKey: HostKeyChallenge;
  session: SessionInfo;
  events: SessionEvent[];
  metric: Metric;
  overview: ServerOverviewSnapshot;
  history: CommandHistoryEntry;
  risk: RiskAssessment;
  operation: OperationStatus;
  error: ErrorEnvelope;
  scope: BackupScope;
  backup: BackupInfo;
  webdav: WebDavProfile;
  restorePlan: RestorePlan;
  restorePreview: RestorePreview;
}

describe("Rust wire contract", () => {
  it("matches JSON emitted by real Rust DTO serialization", () => {
    const output = process.env.COSHELL_WIRE_EXAMPLE
      ? execFileSync(process.env.COSHELL_WIRE_EXAMPLE, [], { encoding: "utf8" })
      : execFileSync("cargo", ["run", "--locked", "--quiet", "-p", "coshell-types", "--example", "wire_contract"], {
          cwd: path.resolve(process.cwd(), "../.."),
          encoding: "utf8",
        });
    const actual: RustWireExample = JSON.parse(output);
    // Every representative DTO is checked as a complete wire object, so a Rust
    // field rename, omitted null or enum change fails rather than being hidden by a cast.
    const expected: RustWireExample = {
      connection: { id: "c1", name: "server", host: "example.org", port: 22, username: "user", authentication: "password", groupId: null, notes: "", production: true, alwaysConfirm: false, version: 1 },
      group: { id: "g1", name: "group", parentId: null },
      connectionTest: { operationId: "op1", phase: "host_verification", state: "failed", serverIdentity: null, error: { code: "CONFIRMATION_REQUIRED", messageKey: "guard.confirm", safeDetail: null, operationId: "op1", outcome: "not_started", retryable: false } },
      hostKey: { host: "example.org", port: 22, algorithm: "ed25519", fingerprint: "SHA256:test" },
      session: { sessionId: "s1", connectionId: "c1", state: "connected", production: true },
      events: [
        { sessionId: "s1", sequence: 1, kind: "output", payload: { data: [65] } },
        { sessionId: "s1", sequence: 2, kind: "state", payload: { state: "disconnected" } },
        { sessionId: "s1", sequence: 3, kind: "error", payload: { error: { code: "CONFIRMATION_REQUIRED", messageKey: "guard.confirm", safeDetail: null, operationId: "op1", outcome: "not_started", retryable: false } } },
        { sessionId: "s1", sequence: 4, kind: "truncated", payload: { discardedBytes: 8 } },
      ],
      metric: { value: 0, unit: "%", unavailableReason: null },
      overview: { connectionId: "c1", collectedAt: "2026-01-01T00:00:00Z", online: true, hostname: "server", operatingSystem: null, kernel: null, architecture: null, bootTime: null, uptime: null, cpuUsage: { value: 0, unit: "%", unavailableReason: null }, logicalCores: null, memoryTotal: null, memoryUsed: null, load: null, diskTotal: null, diskUsed: null, swapTotal: null, swapUsed: null },
      history: { id: "h1", connectionId: "c1", connectionName: "server", production: true, command: "pwd", submittedAt: "2026-01-01T00:00:00Z", outcome: "unknown", exitCode: null, durationMs: null },
      risk: { level: "dangerous", categories: ["recursive_delete"], ruleVersion: "1", displaySpans: [{ start: 0, end: 2 }], redactedDigest: "sha256:abc" },
      operation: { operationId: "op1", state: "failed", error: { code: "CONFIRMATION_REQUIRED", messageKey: "guard.confirm", safeDetail: null, operationId: "op1", outcome: "not_started", retryable: false } },
      error: { code: "CONFIRMATION_REQUIRED", messageKey: "guard.confirm", safeDetail: null, operationId: "op1", outcome: "not_started", retryable: false },
      scope: { includeCredentials: false },
      backup: { id: "b1", formatVersion: 1, createdAt: "2026-01-01T00:00:00Z", includesCredentials: false },
      webdav: { id: "w1", url: "https://example.org", remoteDirectory: "/backup", username: "user", autoBackup: false },
      restorePlan: { backupId: "b1", items: [{ id: "c1", action: "create_copy" }] },
      restorePreview: { backupId: "b1", includesCredentials: false, connections: [{ id: "c1", name: "server", disposition: "conflict" }], groups: [] },
    };
    expect(actual).toEqual(expected);
    expect(JSON.stringify(actual)).not.toMatch(/passwordValue|privateKey|secretRef/);
  }, 120_000);
});
