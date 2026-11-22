# Tournament Formats

This document defines the supported tournament formats, the rules used to
generate pairings and schedules, how those artifacts are persisted, and the
tie-break order used for standings. All generation happens at the canonical
tournament service boundary; clients must not compute pairings locally.

## Supported formats

- `single_elimination` — existing bracket format.
- `swiss` — fixed number of rounds, no elimination.
- `round_robin` — every participant meets every other participant.

## Versioned snapshots

Every generated pairing set or schedule is persisted as an immutable,
versioned snapshot. A snapshot records:

- `format` — one of the formats above.
- `version` — monotonically increasing integer per tournament.
- `rounds` — ordered list of rounds, each with ordered pairings.
- `byes` — participants receiving a bye in each round (explicit, never
  implicit).
- `rules` — the format rules in effect when the snapshot was generated.

Finalized match and tournament records are immutable. Corrections are only
possible through the explicit audited correction flow, which produces a new
snapshot version rather than mutating an existing one.

## Swiss pairing rules

1. Participants are seeded by current standings (see tie-break order below).
2. In each round, participants are paired top-down: the highest-ranked
   unpaired participant is paired with the highest-ranked unpaired participant
   they have not yet faced.
3. If no such opponent exists, the participant is paired with the next
   available unpaired participant (repeat pairing fallback), and the pairing is
   flagged in the snapshot.
4. When the participant count is odd, the lowest-ranked participant who has
   not yet received a bye is assigned the bye for that round. Byes are recorded
   explicitly in `byes` and count as a win for standings purposes.
5. The number of rounds is fixed at generation time and stored in `rules`.

## Round-robin schedule rules

1. Use the circle method: fix one participant and rotate the rest.
2. For an even participant count, every participant plays in every round.
3. For an odd participant count, add a virtual bye slot; the participant
   matched against it receives an explicit bye recorded in `byes`.
4. The schedule has `n - 1` rounds for even `n` and `n` rounds for odd `n`.
5. Home/away alternation follows the standard circle-method rotation so that
   repeated pairings do not occur within a single schedule.

## Tie-break order

The standings tie-break order is stable and applied in this sequence:

1. Total wins (byes count as wins).
2. Head-to-head result between tied participants, when they have played.
3. Buchholz score (sum of opponents' wins).
4. Median-Buchholz score (Buchholz excluding highest and lowest opponent).
5. Seed order assigned at tournament creation.

If all criteria remain tied, participants keep their seed order; the ordering
is deterministic and never depends on iteration order of unordered
collections.

## Concurrency and finalization

Result finalization is idempotent per match. When concurrent finalizations
arrive for the same round, exactly one advances the round; subsequent
finalizations for that round are treated as replays and do not advance the
round again. Round advancement is guarded by the snapshot version so that a
stale writer cannot overwrite a newer snapshot.

## Validation

- Property tests assert bracket and standings invariants for odd and even
  participant counts, including repeated team pairings.
- Focused tests cover authorization, failure handling, and concurrency or
  replay behavior for round advancement.
