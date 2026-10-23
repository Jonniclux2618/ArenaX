// ---------------------------------------------------------------------------
// Sequence-aware match event handling (#33)
//
// Live match frames carry a monotonic per-match `version`. This module decides
// what to do with each frame so that duplicate, stale, reordered, or missed
// deliveries can never roll match status, scores, or dispute state backwards,
// and so that a gap triggers an authoritative refetch instead of a silent
// (and corrupting) patch.
// ---------------------------------------------------------------------------

/** Verdict produced for every incoming live frame. */
export type MatchEventVerdict =
  /** Frame is trustworthy — merge it into the query cache. */
  | { action: "apply"; version: number | null }
  /** Duplicate or stale frame — discard silently. */
  | { action: "drop_stale" }
  /** Stream is untrusted — refetch the authoritative snapshot. */
  | { action: "resync"; reason: "gap" | "stale_streak" };

/** How many consecutive stale frames are tolerated before assuming the server
 * sequence counter reset (e.g. Redis flush) and forcing a resync. */
const STALE_STREAK_LIMIT = 3;

function normalizeVersion(raw: unknown): number | null {
  if (typeof raw === "number" && Number.isFinite(raw) && raw >= 0) {
    return Math.floor(raw);
  }
  if (typeof raw === "string" && raw !== "") {
    const parsed = Number(raw);
    if (Number.isFinite(parsed) && parsed >= 0) return Math.floor(parsed);
  }
  return null;
}

/**
 * Tracks the last applied event version for one match stream.
 *
 * Merge contract (mirrors `RealtimeEvent::MatchStateSync` on the server):
 *  - `version <= baseline`  → drop (duplicate / reordered stale frame).
 *  - `version == baseline+1` → apply and advance.
 *  - `version > baseline+1` → gap: suppress the stream and resync from REST.
 *  - unsequenced frames      → apply without moving the baseline.
 *
 * While a resync is in flight every frame is dropped; the authoritative
 * snapshot's version re-opens the stream via {@link adoptSnapshot}.
 */
export class MatchEventSequencer {
  private baseline: number | null = null;
  private suppressing = false;
  private staleStreak = 0;

  /** Decide what to do with an incoming frame's `version`. */
  check(version: unknown): MatchEventVerdict {
    if (this.suppressing) {
      // A gap already forced a resync — live frames are untrusted until the
      // authoritative snapshot lands.
      return { action: "drop_stale" };
    }

    const parsed = normalizeVersion(version);

    if (parsed === null) {
      // Unsequenced (legacy server or version allocation failure): apply,
      // but never let it move the baseline.
      return { action: "apply", version: null };
    }

    if (this.baseline === null) {
      this.baseline = parsed;
      this.staleStreak = 0;
      return { action: "apply", version: parsed };
    }

    if (parsed <= this.baseline) {
      this.staleStreak += 1;
      if (this.staleStreak >= STALE_STREAK_LIMIT) {
        // Sustained stale delivery usually means the server counter reset
        // beneath us — re-anchor from an authoritative snapshot.
        this.suppressing = true;
        return { action: "resync", reason: "stale_streak" };
      }
      return { action: "drop_stale" };
    }

    if (parsed === this.baseline + 1) {
      this.baseline = parsed;
      this.staleStreak = 0;
      return { action: "apply", version: parsed };
    }

    // Missed event(s): applying across the gap would corrupt local state.
    this.suppressing = true;
    return { action: "resync", reason: "gap" };
  }

  /**
   * Adopt the version of an authoritative REST snapshot, re-opening the live
   * stream. Snapshots are always trusted, so a *lower* version (server counter
   * reset) is accepted as the new baseline.
   */
  adoptSnapshot(version: unknown): void {
    this.suppressing = false;
    this.staleStreak = 0;
    const parsed = normalizeVersion(version);
    if (parsed !== null) {
      this.baseline = parsed;
    }
    // Snapshot without a version: keep the existing baseline; the stream
    // continues sequencing from wherever it was.
  }

  /** Forget the baseline entirely (fresh socket, new match view). */
  reset(): void {
    this.baseline = null;
    this.suppressing = false;
    this.staleStreak = 0;
  }

  /** Current baseline, or `null` before any sequenced frame was applied. */
  get currentVersion(): number | null {
    return this.baseline;
  }

  /** True while waiting for an authoritative snapshot after a gap. */
  get isResyncing(): boolean {
    return this.suppressing;
  }
}

/** Minimal shape the cache merge understands. */
export interface MatchCacheSlice {
  status?: string;
  scorePlayer1?: number;
  scorePlayer2?: number;
  winnerId?: string;
  version?: number;
}

/** Statuses that no live event may move away from without an authoritative snapshot. */
const TERMINAL_STATUSES = new Set(["completed", "cancelled"]);

/**
 * Merge an accepted live update into the cached match snapshot.
 *
 * Only fields present on the update overwrite the cache, so partial events
 * never blank out state. Terminal statuses are frozen against unsequenced
 * events (sequenced events are already ordered by version).
 */
export function mergeMatchUpdate<T extends MatchCacheSlice>(
  previous: T | undefined,
  update: MatchCacheSlice,
): T | undefined {
  if (!previous) return previous;

  const next: MatchCacheSlice = { ...previous };

  if (update.status !== undefined) {
    const currentIsTerminal = previous.status
      ? TERMINAL_STATUSES.has(previous.status)
      : false;
    const incomingIsTerminal = TERMINAL_STATUSES.has(update.status);
    // Never un-finish a match from the live stream; anything may move *into*
    // a terminal status.
    if (!currentIsTerminal || incomingIsTerminal) {
      next.status = update.status;
    }
  }
  if (update.scorePlayer1 !== undefined) next.scorePlayer1 = update.scorePlayer1;
  if (update.scorePlayer2 !== undefined) next.scorePlayer2 = update.scorePlayer2;
  if (update.winnerId !== undefined) next.winnerId = update.winnerId;
  if (update.version !== undefined) next.version = update.version;

  return { ...previous, ...next } as T;
}
