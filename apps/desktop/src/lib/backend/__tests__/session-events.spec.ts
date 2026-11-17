import { describe, expect, it } from "vitest";
import { SessionEventCursor } from "@/lib/backend/session-events";
import type { SessionEvent } from "@/lib/backend/types";

const event = (sessionId: string, sequence: number): SessionEvent => ({
  sessionId, sequence, kind: "output", payload: { data: [sequence] },
});

describe("session event cursor", () => {
  it("accepts consecutive events and ignores duplicates and older events", () => {
    const cursor = new SessionEventCursor("a");
    expect(cursor.receive(event("a", 1))).toEqual({ kind: "apply", event: event("a", 1) });
    expect(cursor.receive(event("a", 1))).toEqual({ kind: "ignore" });
    expect(cursor.receive(event("a", 2))).toEqual({ kind: "apply", event: event("a", 2) });
    expect(cursor.receive(event("a", 1))).toEqual({ kind: "ignore" });
    expect(cursor.receive(event("b", 3))).toEqual({ kind: "ignore" });
  });

  it("halts on a gap until an atomic snapshot replaces output and watermark", () => {
    const cursor = new SessionEventCursor("a");
    cursor.receive(event("a", 1));
    expect(cursor.receive(event("a", 3))).toEqual({ kind: "resync", generation: 1 });
    expect(cursor.receive(event("a", 2))).toEqual({ kind: "ignore" });
    expect(cursor.applySnapshot(1, "a", 3)).toEqual({ kind: "accepted" });
    expect(cursor.receive(event("a", 3))).toEqual({ kind: "ignore" });
    expect(cursor.receive(event("a", 4))).toEqual({ kind: "apply", event: event("a", 4) });
  });

  it("rejects stale snapshots and malformed sequence numbers", () => {
    const cursor = new SessionEventCursor("a");
    for (const n of [0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1, NaN]) {
      expect(cursor.receive(event("a", n))).toEqual({ kind: "invalid" });
    }
    cursor.receive(event("a", 1));
    cursor.receive(event("a", 3));
    expect(cursor.applySnapshot(1, "a", 0)).toEqual({ kind: "retry", generation: 2 });
    expect(cursor.applySnapshot(2, "a", 3)).toEqual({ kind: "accepted" });
    expect(cursor.receive(event("a", 5))).toEqual({ kind: "resync", generation: 3 });
    expect(cursor.applySnapshot(1, "a", 8)).toEqual({ kind: "ignore" });
    expect(cursor.applySnapshot(3, "b", 5)).toEqual({ kind: "invalid" });
    expect(cursor.applySnapshot(3, "a", Number.MAX_SAFE_INTEGER + 1)).toEqual({ kind: "invalid" });
    expect(cursor.applySnapshot(3, "a", 5)).toEqual({ kind: "accepted" });
  });

  it("does not resume from a snapshot older than events discarded during recovery", () => {
    const cursor = new SessionEventCursor("a");
    cursor.receive(event("a", 1));
    expect(cursor.requireResync()).toEqual({ kind: "resync", generation: 1 });
    expect(cursor.receive(event("a", 2))).toEqual({ kind: "ignore" });
    expect(cursor.applySnapshot(1, "a", 1)).toEqual({ kind: "retry", generation: 2 });
    expect(cursor.applySnapshot(2, "a", 2)).toEqual({ kind: "accepted" });
    expect(cursor.receive(event("a", 3))).toEqual({ kind: "apply", event: event("a", 3) });
  });
});
