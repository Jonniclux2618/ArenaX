# Match Authority Implementation

This document describes how the match authority service owns match state, how
critical domain events are delivered, and the guarantees consumers can rely on.

## Route ownership

The match authority remains the single authoritative write path for match state.
Wallet and tournament mutations are owned by their respective services. No second
authoritative write path is introduced by event delivery: events are derived from
the owning transaction, never written back into it.

## Transactional outbox

Critical domain events (match, wallet, tournament) are persisted in an `outbox`
table **inside the same database transaction** as the domain mutation. The row is
committed or rolled back atomically with the mutation, so a process failure can
never leave a committed mutation without its event, or an event without its
mutation.

Each outbox row carries:

- `id` — a stable, globally unique event ID (UUID) generated at write time. It is
  the idempotency key for downstream consumers and never changes across retries.
- `schema_version` — the event schema version, so consumers can evolve safely.
- `aggregate_type` / `aggregate_id` — the owning aggregate.
- `event_type` — the domain event name.
- `payload` — the serialized event body.
- `created_at`, `published_at`, `attempts` — delivery bookkeeping.

Example (pseudo-SQL, executed on the same connection/transaction as the mutation):

```sql
BEGIN;
UPDATE matches SET status = 'completed' WHERE id = $1;
INSERT INTO outbox (id, schema_version, aggregate_type, aggregate_id, event_type, payload, created_at)
VALUES ($2, 1, 'match', $1, 'match.completed', $3, now());
COMMIT;
```

## Publisher and restart safety

The publisher polls unpublished rows (`published_at IS NULL`) ordered by
`created_at`, publishes them, and marks them published. Because rows are durable
before the publisher runs, the publisher **resumes after a restart** and picks up
any rows that were committed but not yet delivered. Delivery is at-least-once.

## Duplicate delivery and idempotency

Delivery is at-least-once, so consumers may see the same event more than once.
Duplicate delivery is safe because:

- Every event has a stable `id` that is identical across retries.
- Consumers deduplicate on `id` (store processed IDs / use an upsert keyed by
  `id`) before applying side effects.
- Handlers are written to be idempotent for a given `id`.

## Retention and cleanup

Published outbox rows are retained for a bounded window (default 7 days) to allow
replay and auditing, then removed by a periodic cleanup job that deletes rows
where `published_at < now() - interval '7 days'`. Unpublished rows are never
removed by cleanup. Retention is configurable per deployment.

## Validation

- Crash/restart behavior is exercised against a local database: commit a
  mutation, kill the publisher before delivery, restart, and confirm the event is
  delivered exactly once to the consumer after deduplication.
- Focused automated coverage asserts that outbox rows roll back with a failed
  mutation, that the publisher resumes unpublished rows, and that replaying the
  same event ID is a no-op for consumers.
