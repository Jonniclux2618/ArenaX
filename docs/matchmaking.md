# Matchmaking: Queue Exit, Cancellation, and Reservation Expiry

This document specifies the canonical queue-exit state machine implemented at the
matchmaking service boundary (`backend/src/services/match_service.rs`). It defines
deterministic behavior for **queued**, **matched**, and **reserved** players, and
the guarantees clients can rely on.

## States

| State      | Meaning                                                                 |
|------------|-------------------------------------------------------------------------|
| `queued`   | Player is waiting in the fair queue.                                    |
| `matched`  | Player has been paired; a match record exists but is not yet reserved.  |
| `reserved` | Player holds a reservation (seat) for a match, pending acceptance.      |
| `cancelled`| Player exited the queue before being matched.                           |
| `expired`  | Player's reservation lapsed without acceptance.                         |
| `completed`| Player accepted and the match proceeded.                                |

Terminal states (`cancelled`, `expired`, `completed`) are immutable. Finalized
match and tournament records are immutable except through an explicit audited
correction flow.

## Cancellation

- A cancel request is **idempotent** keyed by `(player_id, queue_epoch)`.
  Repeating a cancel returns the same terminal outcome and never creates a
  duplicate queue entry.
- Cancelling while `queued` transitions to `cancelled` and removes the player
  from the fair queue without affecting other players' positions.
- Cancelling while `matched` or `reserved` releases the seat and transitions to
  `cancelled`; the released seat is offered to the next eligible player under
  the same fairness rules (no skipping).
- Cancelling an already-terminal entry is a no-op returning the stored outcome.

## Timeout

- Each state has a deterministic deadline. On expiry the entry transitions to
  `expired` (or `cancelled` for pre-match timeouts) exactly once.
- Timeout evaluation is monotonic and safe under concurrent workers: only one
  worker may finalize a given entry; others observe the terminal outcome.
- A timed-out reservation is cleaned up and the seat is restored to eligible
  players without bypassing fairness ordering.

## Retries and Idempotency

- Join and cancel operations carry a client-supplied idempotency key. Retries
  with the same key return the original result and cannot duplicate queue
  entries or reservations.
- Reconnect after a transient failure resumes the player's current state; it
  never re-enqueues a player who is already `queued`, `matched`, or `reserved`.

## Client Exposure

The API exposes the current queue-exit state and outcome for a player, including
the terminal reason (`cancelled`, `expired`, `completed`) and the timestamp of
the transition, so clients can render deterministic UI for every exit path.

## Validation

- Property tests exercise interleaved join, cancel, timeout, and reconnect
  operations across multiple workers, asserting no duplicate entries and no
  fairness violations.
- Focused tests cover authorization, failure handling, and concurrency/replay
  behavior for cancellation and reservation expiry.
