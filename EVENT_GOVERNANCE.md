# Event Governance

This document defines the operator-facing rules for scheduling tournaments and
other events. It is the canonical reference for how start times, deadlines, and
timezones are handled.

## Timezone and DST semantics

Tournament scheduling separates the **event instant** from the **organizer's
display preference**. Both are stored independently.

### Creation requirements

- Every tournament creation request MUST include a valid IANA timezone
  identifier (e.g. `America/New_York`, `Europe/Berlin`, `UTC`).
- Invalid or unknown identifiers are rejected at the service boundary; the
  tournament is not created.
- The organizer's local start time is interpreted in the supplied timezone.

### Ambiguous and nonexistent local times

Daylight-saving transitions make some local times ambiguous (fall-back, the
clock repeats an hour) or nonexistent (spring-forward, the clock skips an
hour). The service resolves these with an explicit, user-visible rule:

- **Spring-forward (nonexistent local time):** the requested local time does
  not exist. It is shifted forward by the size of the gap (typically one hour)
  to the first valid instant after the transition. The response reports the
  resolved instant and the applied rule so the organizer can confirm it.
- **Fall-back (ambiguous local time):** the requested local time occurs twice.
  The **earlier** (first) occurrence — the pre-transition offset — is chosen.
  The response reports the resolved instant, the chosen offset, and the applied
  rule so the organizer can confirm it.

These rules are deterministic and surfaced in the API response; organizers are
never left with a silently shifted or ambiguous start time.

### Execution deadlines vs. display

- All execution deadlines and job schedules are persisted and executed as
  **UTC instants**. Tournament jobs never depend on a local wall-clock time.
- Displays (API serialization, notifications, operator views) preserve the
  configured timezone and the resolved UTC offset at the event instant, so the
  organizer sees the intended local time.

### Immutability and corrections

Finalized match and tournament records are immutable. Start times, timezones,
and deadlines may only be changed through an explicit, audited correction flow
that records the previous and new values along with the acting operator.

## Validation expectations

Replay is **explicit and audited**. It is never triggered automatically by a
decoder upgrade.

1. Deploy the upgraded decoder.
2. An operator selects the quarantined records to replay (typically those whose
   recorded decoder version is older than the current one).
3. The replay is recorded in the **audit trail**: who triggered it, when, which
   records, and the decoder version used.
4. Successfully decoded events are applied; events that still fail remain in
   quarantine with an updated decoder version.

Duplicate delivery is handled idempotently: replaying an event that was already
applied is a no-op, and retries of a failed replay do not create duplicate
projection entries.

## Testing expectations

### Validation expectations

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
