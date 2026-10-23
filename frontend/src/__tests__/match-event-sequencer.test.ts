/**
 * Issue #33 — sequence-aware real-time match hub.
 *
 * Simulates duplicate, reordered, missed, and reconnecting event streams
 * against the match event sequencer and the cache merge helper.
 */

import {
  MatchEventSequencer,
  mergeMatchUpdate,
  type MatchCacheSlice,
} from "@/lib/matchEventSequencer";

interface SimulatedFrame {
  version?: number;
  status?: string;
  scorePlayer1?: number;
  scorePlayer2?: number;
  winnerId?: string;
}

/**
 * Replays a stream of frames the way the WebSocket hook does: check the
 * verdict, merge on apply, resync (adopting the given snapshot version) on gap.
 */
function replay(
  frames: SimulatedFrame[],
  options: { snapshotVersion?: number; snapshotOnResync?: number } = {},
): { cache: MatchCacheSlice; applied: SimulatedFrame[]; resyncs: number } {
  const sequencer = new MatchEventSequencer();
  if (options.snapshotVersion !== undefined) {
    sequencer.adoptSnapshot(options.snapshotVersion);
  }

  let cache: MatchCacheSlice = {
    status: "in_progress",
    scorePlayer1: 0,
    scorePlayer2: 0,
    version: options.snapshotVersion,
  };
  const applied: SimulatedFrame[] = [];
  let resyncs = 0;

  for (const frame of frames) {
    const verdict = sequencer.check(frame.version);
    if (verdict.action === "drop_stale") continue;
    if (verdict.action === "resync") {
      resyncs += 1;
      // Authoritative snapshot lands: cache replaced, baseline re-adopted.
      const snapshotVersion =
        options.snapshotOnResync ?? frame.version ?? cache.version;
      cache = { ...cache, version: snapshotVersion };
      sequencer.adoptSnapshot(snapshotVersion);
      continue;
    }
    cache = mergeMatchUpdate(cache, {
      status: frame.status,
      scorePlayer1: frame.scorePlayer1,
      scorePlayer2: frame.scorePlayer2,
      winnerId: frame.winnerId,
      version: verdict.version ?? undefined,
    })!;
    applied.push(frame);
  }

  return { cache, applied, resyncs };
}

describe("MatchEventSequencer", () => {
  it("applies an in-order stream and advances the baseline", () => {
    const sequencer = new MatchEventSequencer();
    expect(sequencer.check(1)).toEqual({ action: "apply", version: 1 });
    expect(sequencer.check(2)).toEqual({ action: "apply", version: 2 });
    expect(sequencer.check(3)).toEqual({ action: "apply", version: 3 });
    expect(sequencer.currentVersion).toBe(3);
  });

  it("bootstraps the baseline from the first sequenced frame", () => {
    const sequencer = new MatchEventSequencer();
    expect(sequencer.check(7)).toEqual({ action: "apply", version: 7 });
    expect(sequencer.currentVersion).toBe(7);
  });

  it("drops duplicates without moving the baseline", () => {
    const sequencer = new MatchEventSequencer();
    sequencer.check(4);
    expect(sequencer.check(4)).toEqual({ action: "drop_stale" });
    expect(sequencer.check(4)).toEqual({ action: "drop_stale" });
    expect(sequencer.currentVersion).toBe(4);
    expect(sequencer.check(5)).toEqual({ action: "apply", version: 5 });
  });

  it("requests a resync on a gap and suppresses the stream until a snapshot lands", () => {
    const sequencer = new MatchEventSequencer();
    sequencer.check(1);
    expect(sequencer.check(4)).toEqual({ action: "resync", reason: "gap" });
    expect(sequencer.isResyncing).toBe(true);
    // Frames that arrive while the refetch is in flight are dropped.
    expect(sequencer.check(5)).toEqual({ action: "drop_stale" });
    expect(sequencer.check(2)).toEqual({ action: "drop_stale" });
    // The authoritative snapshot (version 6) re-opens the stream.
    sequencer.adoptSnapshot(6);
    expect(sequencer.isResyncing).toBe(false);
    expect(sequencer.check(7)).toEqual({ action: "apply", version: 7 });
  });

  it("forces a resync after a stale streak (server counter reset)", () => {
    const sequencer = new MatchEventSequencer();
    sequencer.check(10);
    expect(sequencer.check(10)).toEqual({ action: "drop_stale" });
    expect(sequencer.check(9)).toEqual({ action: "drop_stale" });
    expect(sequencer.check(8)).toEqual({ action: "resync", reason: "stale_streak" });
    sequencer.adoptSnapshot(1);
    expect(sequencer.check(2)).toEqual({ action: "apply", version: 2 });
  });

  it("applies unsequenced frames without moving the baseline", () => {
    const sequencer = new MatchEventSequencer();
    expect(sequencer.check(undefined)).toEqual({ action: "apply", version: null });
    expect(sequencer.check("n/a")).toEqual({ action: "apply", version: null });
    expect(sequencer.currentVersion).toBeNull();
    expect(sequencer.check(3)).toEqual({ action: "apply", version: 3 });
  });

  it("accepts string versions and resets cleanly", () => {
    const sequencer = new MatchEventSequencer();
    expect(sequencer.check("2")).toEqual({ action: "apply", version: 2 });
    sequencer.reset();
    expect(sequencer.currentVersion).toBeNull();
    expect(sequencer.check(1)).toEqual({ action: "apply", version: 1 });
  });
});

describe("match stream simulations (#33 acceptance)", () => {
  it("in-order stream converges scores and terminal status", () => {
    const { cache, resyncs } = replay([
      { version: 2, scorePlayer1: 1, scorePlayer2: 0 },
      { version: 3, scorePlayer1: 2, scorePlayer2: 0 },
      { version: 4, status: "completed", winnerId: "p1", scorePlayer1: 3 },
    ]);
    expect(cache).toMatchObject({
      status: "completed",
      scorePlayer1: 3,
      scorePlayer2: 0,
      winnerId: "p1",
      version: 4,
    });
    expect(resyncs).toBe(0);
  });

  it("duplicate stream cannot roll the UI backward", () => {
    const { cache } = replay([
      { version: 2, scorePlayer1: 2 },
      { version: 2, scorePlayer1: 2 },
      { version: 1, scorePlayer1: 0, status: "in_progress" },
      { version: 2, scorePlayer1: 2 },
    ]);
    expect(cache.scorePlayer1).toBe(2);
    expect(cache.version).toBe(2);
  });

  it("reordered stream applies newest-first state and drops the stale replay", () => {
    // Server sent 1 then 2; network delivered 2 then 1.
    const { cache } = replay([
      { version: 2, status: "disputed", scorePlayer1: 3, scorePlayer2: 1 },
      { version: 1, status: "in_progress", scorePlayer1: 2, scorePlayer2: 1 },
    ]);
    expect(cache.status).toBe("disputed");
    expect(cache.scorePlayer1).toBe(3);
    expect(cache.version).toBe(2);
  });

  it("missed event triggers authoritative resync and converges on the snapshot", () => {
    const { cache, resyncs, applied } = replay(
      [
        { version: 1, scorePlayer1: 1 },
        { version: 4, status: "completed", scorePlayer1: 3 }, // 2 and 3 missed
        { version: 5, status: "completed", scorePlayer1: 3 }, // suppressed mid-resync
      ],
      { snapshotOnResync: 4 },
    );
    expect(resyncs).toBe(1);
    expect(applied).toHaveLength(1); // only the pre-gap frame was applied
    expect(cache.version).toBe(4); // snapshot adopted as the new baseline
  });

  it("reconnecting stream: snapshot baseline + resumed stream converge", () => {
    const sequencer = new MatchEventSequencer();
    let cache: MatchCacheSlice = {
      status: "in_progress",
      scorePlayer1: 1,
      scorePlayer2: 0,
      version: 3,
    };

    // Initial REST snapshot adopted when the socket opens.
    sequencer.adoptSnapshot(3);

    // Live frames continue from the snapshot.
    for (const frame of [
      { version: 4, scorePlayer1: 2 },
      { version: 5, scorePlayer1: 3 },
    ]) {
      const verdict = sequencer.check(frame.version);
      expect(verdict.action).toBe("apply");
      cache = mergeMatchUpdate(cache, frame as MatchCacheSlice)!;
    }

    // Connection drops; server keeps publishing 6 and 7 meanwhile.
    // On reconnect the client refetches (snapshot now at 7).
    cache = {
      status: "completed",
      scorePlayer1: 3,
      scorePlayer2: 1,
      winnerId: "p1",
      version: 7,
    };
    sequencer.adoptSnapshot(cache.version);

    // Replayed frames from before the drop are stale.
    expect(sequencer.check(5)).toEqual({ action: "drop_stale" });
    // The stream resumes exactly after the snapshot.
    expect(sequencer.check(8)).toEqual({ action: "apply", version: 8 });

    expect(cache).toMatchObject({ status: "completed", scorePlayer1: 3, version: 7 });
  });

  it("unsequenced frames never override a terminal cached status", () => {
    const cache = mergeMatchUpdate(
      { status: "completed", scorePlayer1: 3, winnerId: "p1" },
      { status: "in_progress", scorePlayer1: 2 },
    )!;
    expect(cache.status).toBe("completed");
    expect(cache.scorePlayer1).toBe(2);
  });

  it("partial frames only overwrite fields they carry", () => {
    const cache = mergeMatchUpdate(
      { status: "disputed", scorePlayer1: 3, scorePlayer2: 1, version: 5 },
      { scorePlayer2: 2, version: 6 },
    )!;
    expect(cache).toEqual({
      status: "disputed",
      scorePlayer1: 3,
      scorePlayer2: 2,
      version: 6,
    });
  });
});
