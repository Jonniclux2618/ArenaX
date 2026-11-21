# Tournament Registration Waitlists and Promotion

This document describes the waitlist and promotion behavior for capacity-limited
tournaments. The behavior is implemented at the canonical registration service
boundary (`backend/src/services/tournament.rs`) so that all callers — API,
operator tooling, and background jobs — share the same guarantees.

## Waitlist ordering

- Players who register after capacity is reached are enqueued on the waitlist.
- Order is **stable FIFO**: entries are ordered by enqueue time, with the
  registration id as a deterministic tiebreaker so that concurrent enqueues
  never produce an ambiguous position.
- The order is not recomputed on read; it is derived from persisted enqueue
  metadata, so repeated reads and replays yield the same sequence.

## Promotion

When a spot opens (cancellation, capacity increase, or an expired offer):

1. The next eligible waitlisted player is selected by stable order.
2. **Eligibility is rechecked at promotion time** — a player who has since
   become ineligible (e.g. withdrawn, already registered, or failing current
   tournament eligibility rules) is skipped and the next candidate is
   considered.
3. The promoted player receives a **bounded acceptance window**. If they do not
   accept before the window expires, the offer is released and the next
   candidate is promoted.

## Atomicity and exactly-once advancement

- Capacity decrement and payment reservation are updated **atomically in a
  single transaction** together with the promotion decision. A promotion either
  fully commits (spot reserved, payment held, offer issued) or has no effect.
- Declined or expired offers advance the waitlist **exactly once**. Promotion
  jobs are idempotent: duplicate or replayed jobs observe the already-advanced
  state and do not promote a second player or double-reserve payment.
- Organizer capacity changes are applied transactionally; increasing capacity
  triggers promotion of eligible waitlisted players, decreasing capacity never
  overbooks existing confirmed registrations.

## Immutability

Finalized match and tournament records remain immutable. Any correction to a
finalized record must go through the explicit audited correction flow; waitlist
promotion never mutates finalized records.

## Validation coverage

Focused automated tests cover:

- simultaneous cancellations (no overbooking, deterministic promotion order),
- payment delay (reservation held until the acceptance window resolves),
- duplicate promotion jobs (exactly-once advancement),
- organizer capacity changes (atomic capacity + promotion),
- authorization and failure handling for promotion actions.
