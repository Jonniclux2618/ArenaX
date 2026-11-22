# Deterministic Historical Backfill for Contract Event Consumers

This document describes how new projections rebuild from retained chain events
without racing live processing or double-applying updates.

## Scope

Backfill runs at the canonical Soroban event-consumer boundary. It reuses the
same event decoding and deduplication rules as live processing so that a full
replay and live processing converge to identical projection state.

## Capturing a range

A backfill job is defined by an explicit, immutable descriptor:

- `contract_id` — the contract whose events are replayed.
- `from_ledger` / `to_ledger` — the inclusive ledger range to replay.
- `code_version` — the projection code version the replay was produced with.

Both the ledger range and the code version are persisted with the job so a
resumed run replays exactly the same inputs. Financial history is never mutated;
backfill only writes projection state derived from retained events.

## Progress, resume, and cancel

- Progress is stored as the last fully applied ledger (and event cursor within
  that ledger). Each checkpoint is written in the same transaction as the
  projection updates it covers, so a crash never advances progress past
  unapplied events.
- **Resume** restarts from the stored checkpoint. Events at or before the
  checkpoint are skipped, so resuming is idempotent.
- **Cancel** stops the job without deleting its checkpoint. A cancelled job can
  be resumed later or discarded; either way the projection state remains
  consistent because every applied batch was committed atomically.

## Deduplication

Live and backfill consumers share one deduplication rule keyed on the event's
stable identity (contract id, ledger, transaction hash, and event index). An
event already recorded as applied is skipped regardless of whether it arrived
via live streaming or replay. This guarantees:

- Duplicate delivery (live + backfill overlap) applies an event at most once.
- Retried batches after a transient failure do not double-apply updates.
- A full replay from `from_ledger` to `to_ledger` yields the same projection
  state as live processing of the same event sequence.

## Failure and retry

- Transient failures (RPC timeouts, rate limits) are retried with backoff from
  the last committed checkpoint.
- Permanent failures (malformed event, unknown code version) halt the job and
  leave the checkpoint intact for operator inspection.
- Because checkpoints and projection writes commit together, a failure never
  leaves a partially applied ledger recorded as complete.

## Validation

Focused automated tests cover:

- A seeded event sequence replayed in full versus processed live, asserting
  identical projection state.
- Retry after a simulated transient failure resumes from the checkpoint.
- Duplicate delivery of the same event applies it exactly once.
- Cancellation followed by resume produces the same final state as an
  uninterrupted run.

## Safe production rollout

1. Deploy the projection code version that will be recorded on the job.
2. Start the backfill against a staging projection and compare its state with a
   live-processed copy of the same range.
3. Once states match, run the backfill in production while live processing
   continues; shared deduplication prevents double application.
4. Monitor checkpoint progress and error rates; cancel and resume if needed.
5. After the range completes, verify the projection matches live state before
   retiring the backfill job.
