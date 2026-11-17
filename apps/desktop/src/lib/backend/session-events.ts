import type { SessionEvent } from "./types";

export type EventDecision =
  | { kind: "apply"; event: SessionEvent }
  | { kind: "ignore" }
  | { kind: "invalid" }
  | { kind: "resync"; generation: number };
export type SnapshotDecision =
  | { kind: "accepted" }
  | { kind: "retry"; generation: number }
  | { kind: "ignore" }
  | { kind: "invalid" };

/** One cursor per session. The caller must replace its view with the matching
 * atomic snapshot before accepting later events; this module does no I/O.
 * A snapshot restores state and scrollback, not individual error or truncated
 * events lost in a gap; the view must indicate that notifications were lost. */
export class SessionEventCursor {
  private sequence = 0;
  private generation = 0;
  private pending = false;
  private observed = 0;

  constructor(private readonly sessionId: string) {
    if (!sessionId) throw new Error("sessionId is required");
  }

  receive(event: SessionEvent): EventDecision {
    if (event.sessionId !== this.sessionId) return { kind: "ignore" };
    if (!Number.isSafeInteger(event.sequence) || event.sequence < 1) return { kind: "invalid" };
    if (event.sequence <= this.sequence) return { kind: "ignore" };
    if (this.pending) {
      this.observed = Math.max(this.observed, event.sequence);
      return { kind: "ignore" };
    }
    if (event.sequence !== this.sequence + 1) {
      this.observed = event.sequence;
      return this.requireResync();
    }
    this.sequence = event.sequence;
    return { kind: "apply", event };
  }

  /** Call when the transport or server signals a gap without an event. */
  requireResync(): EventDecision {
    if (this.pending) return { kind: "ignore" };
    this.pending = true;
    this.observed = Math.max(this.observed, this.sequence);
    this.generation += 1;
    return { kind: "resync", generation: this.generation };
  }

  /** On accepted, replace the view synchronously; on retry fetch a new snapshot.
   * Invalid responses need transport error handling; stale generations are ignored. */
  applySnapshot(generation: number, sessionId: string, watermark: number): SnapshotDecision {
    if (!this.pending || generation !== this.generation) return { kind: "ignore" };
    if (sessionId !== this.sessionId || !Number.isSafeInteger(watermark) || watermark < 0) {
      return { kind: "invalid" };
    }
    if (watermark < this.observed) {
      this.generation += 1;
      return { kind: "retry", generation: this.generation };
    }
    this.sequence = watermark;
    this.observed = watermark;
    this.pending = false;
    return { kind: "accepted" };
  }
}
