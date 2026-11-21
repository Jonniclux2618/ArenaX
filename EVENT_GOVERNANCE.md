# ArenaX Event Governance Policy

> Version: 1.0 · Status: Active · Owner: Backend Platform Team

This document defines the rules, conventions, and processes every engineer must follow when producing or consuming Kafka events on the ArenaX platform.

---

## 1. Core Principles

- **Events are immutable facts.** Once published, an event record is never edited. Corrections are published as new events.
- **The event store is the source of truth.** All domain events are persisted to `arenax.event.store` before (or alongside) any database write.
- **Loose coupling.** Producers must not know about consumers. Never call a consumer API from a producer service.
- **At-least-once delivery.** Every event may be delivered more than once. All consumers must be idempotent.

---

## 2. Topic Naming Convention

```
arenax.<domain>.<event-noun>s        # primary topic
arenax.<domain>.<event-noun>s.dlq    # dead-letter topic
arenax.event.store                   # global append-only store
```

| Domain segment | Examples |
|---|---|
| `user` | `arenax.user.events` |
| `match` | `arenax.match.events` |
| `achievement` | `arenax.achievement.events` |
| `wallet` | `arenax.wallet.events` |

**Rules:**
- Lowercase only. Use `.` as separator, never `-` or `_`.
- Every primary topic MUST have a corresponding `.dlq` topic.
- Topics are created with `auto.create.topics.enable=true` in dev; use Terraform/scripts in production with explicit partition counts.

---

## 3. Event Schema Standards

Every event MUST be wrapped in `EventEnvelope<T>`:

```typescript
interface EventEnvelope<T> {
  eventId: string;       // UUID v4 — unique per message
  eventType: string;     // Dot-separated noun e.g. "match.ended"
  version: number;       // Integer, starts at 1
  occurredAt: string;    // ISO-8601 UTC
  source: string;        // Emitting service name
  traceId: string;       // Distributed trace correlation id
  causationId?: string;  // eventId of the event that caused this one
  payload: T;
}
```

### 3.1 Versioning Strategy

| Change type | Action |
|---|---|
| Add optional field | No version bump — backwards-compatible |
| Rename / remove field | **Bump version** (e.g. `version: 2`) |
| Change field type | **Bump version** |
| New event type | New `eventType` string — no version change needed |

- Both `version: 1` and `version: 2` schemas must be handled by consumers during the transition window (minimum 2 sprints).
- Old schema versions are retired only after all consumer groups confirm migration.

### 3.2 Event Type Registry

All `eventType` strings must be registered in `server/src/services/kafka/domain-events.ts` before use. **Unregistered event types are rejected at code review.**

| Event type | Topic | Version | Owner |
|---|---|---|---|
| `user.registered` | `arenax.user.events` | 1 | Auth Service |
| `match.ended` | `arenax.match.events` | 1 | Match Service |
| `achievement.unlocked` | `arenax.achievement.events` | 1 | Achievement Service |
| `wallet.credited` | `arenax.wallet.events` | 1 | Wallet Service |

---

## 4. Producer Rules

1. **Always set a meaningful partition key** — use the aggregate's primary id (e.g. `userId`, `matchId`) to preserve ordering within an aggregate.
2. **Publish to both the domain topic and `arenax.event.store`** via `EventStore.append()`.
3. **Inject `traceId`** from the originating HTTP request (via `eventTracingMiddleware`). Never generate a new one mid-flow.
4. **Use idempotent producer** (`idempotent: true`) — already set in `KafkaClient`.
5. Log the `eventId` and `traceId` at `INFO` level after a successful publish.
6. **Critical domain events go through the transactional outbox** (see §4.1). Never publish a match, wallet, or tournament event directly after `COMMIT`.

### 4.1 Transactional Outbox

Publishing after committing a domain mutation loses events if the process dies
between `COMMIT` and the broker `ack`. Critical events (match, wallet,
tournament) are therefore written to an outbox table **inside the same database
transaction** as the mutation, and a separate publisher drains the table to
Kafka.

**Write path (atomic with the mutation):**

```typescript
await db.transaction(async (tx) => {
  await tx.matches.update(matchId, { status: 'ended' });
  await outbox.enqueue(tx, {
    eventId: randomUUID(),          // stable id, generated once
    eventType: 'match.ended',
    version: 1,                     // schema version, see §3.1
    aggregateId: matchId,           // partition key
    traceId: req.traceId,
    payload: { matchId, winnerId },
  });
});
```

- The outbox row commits or rolls back **with** the domain mutation — there is no
  window where one exists without the other.
- `eventId` is generated at enqueue time and never regenerated, so a retried
  publish reuses the same id (stable across restarts).
- `version` is the schema version from §3.1 and is carried on the row so the
  publisher emits the exact schema that was current at write time.
- The outbox is the **only** authoritative write path for these events; do not
  add a second direct-publish path alongside it.

**Publish path (resumable):**

- The publisher polls unpublished rows (`published_at IS NULL`) ordered by
  `id`, publishes each to its domain topic and `arenax.event.store`, then marks
  the row published.
- On restart the publisher resumes from the first unpublished row — nothing is
  lost because the row was already durable before the crash.
- A row may be published more than once (crash after `ack`, before the mark).
  This is expected: delivery is at-least-once and consumers dedupe on `eventId`
  (§5.2).

**Retention & cleanup:**

- Published rows are retained for **7 days** (matching the longest consumer
  replay window) and then purged by a scheduled job.
- Unpublished rows are never purged; a growing backlog is an alert (see §8).
- Purge only rows with `published_at < now() - interval '7 days'`.

---

## 5. Consumer Rules

1. **Consumer group ids** follow the pattern `arenax.<domain>.consumers`. One group per logical consumer (not per instance).
2. **Idempotency is mandatory.** Use the `eventId` as an idempotency key when writing to any database.
3. **Never `throw` from `handle()` without a meaningful log.** The base `EventConsumer` will retry and eventually route to DLQ.
4. **DLQ consumers** must be set up for every primary topic. A DLQ message must trigger a PagerDuty/Slack alert and be reviewed within 24 hours.
5. **Do not block the event loop.** All I/O inside `handle()` must be awaited.

### 5.2 Duplicate-safe consumption

Because the outbox publisher (and Kafka itself) delivers at-least-once, a
consumer may see the same `eventId` twice. Consumers MUST make redelivery a
no-op:

- Record processed `eventId`s (unique constraint or a `processed_events` table)
  in the same transaction as the consumer's own write.
- A duplicate `eventId` is skipped, not reprocessed — the second delivery must
  not double-apply a wallet credit or re-emit a downstream event.

### 5.3 Durable consumer checkpoints

Deduplication is keyed by **durable event identity** and **consumer identity**,
not by in-memory state. Each logical consumer owns a checkpoint row per event:

```sql
CREATE TABLE consumer_checkpoints (
  consumer_id  text        NOT NULL,  -- e.g. 'achievement.progression'
  event_id     uuid        NOT NULL,  -- EventEnvelope.eventId
  applied_at   timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (consumer_id, event_id)
);
```

- The checkpoint insert and the consumer's side effects run in **one database
  transaction**. If the side effect fails, the checkpoint rolls back too, so a
  retry re-applies cleanly. If the checkpoint insert conflicts on the primary
  key, the delivery is a duplicate and the transaction is aborted before any
  side effect is applied.
- Because the checkpoint is durable, deduplication survives **process
  restarts** and consumer rebalances — a redelivered event after a crash is
  still recognised as already applied.
- Concurrent duplicate deliveries race on the `(consumer_id, event_id)` primary
  key: exactly one transaction commits, the others observe the conflict and
  skip. The business effect is applied exactly once.
- Consumers that cannot share a transaction with their side effect (e.g. an
  external HTTP call) must use an equivalent idempotent operation keyed by
  `(consumer_id, event_id)` and record the checkpoint after the effect succeeds.
- Checkpoints are retained for at least the longest replay window (7 days,
  matching §4.1) so replays within that window are still deduplicated.

**Deduplication metrics.** Every consumer exposes counters so duplicate
suppression is observable:

| Metric | Meaning |
|---|---|
| `consumer.events.applied` | Events whose side effects committed |
| `consumer.events.duplicate` | Deliveries skipped because the checkpoint already existed |
| `consumer.events.failed` | Transactions rolled back and will be retried |

A sustained rise in `consumer.events.duplicate` is expected after a publisher
restart; a rise in `consumer.events.failed` is an alert (see §8).

---

## 6. Dead Letter Queue Policy

| Trigger | Action |
|---|---|
| Parse failure (invalid JSON) | Immediately route to DLQ |
| `handle()` throws after `maxRetries` (default 3) | Route to DLQ |
| Unknown event type | Route to DLQ with `reason: unknown_event_type` |

- DLQ messages are retained for **14 days**.
- Engineers are responsible for inspecting and replaying or discarding DLQ messages weekly.
- Use `EventReplay` to re-process a DLQ batch after the underlying bug is fixed.

---

## 7. Event Sourcing & Replay

The `arenax.event.store` topic is the immutable audit log of all domain events.

- **Retention:** infinite (log compaction enabled, no size limit in production).
- **Access control:** read-only for all services except the EventStore writer.
- **Replay:** use `EventReplay.run({ fromTimestamp, toTimestamp, eventTypes, onEvent })` to reconstruct state or backfill a new service.
- Replay runs should use a unique `replayGroupId` (auto-generated if unset) to avoid interfering with 

/* … truncated 2577 chars — edit only what you need near the top … */
