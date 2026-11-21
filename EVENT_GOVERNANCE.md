# Event Governance

This document describes how authoritative state changes are recorded, retained,
and accessed for support and dispute review.

## Match event timeline

Every finalized match persists a bounded, append-only timeline of authoritative
state changes at the canonical service boundary. Each entry records:

- **actor class** — the category of the actor that caused the change (e.g.
  `system`, `player`, `operator`, `correction`). Raw identities are not stored.
- **event type** — a stable, enumerated label for the change (e.g.
  `match.finalized`, `score.corrected`, `rating.applied`).
- **sequence** — a monotonically increasing integer scoped to the match, used to
  order entries deterministically.
- **timestamp** — the server-assigned time the change was recorded.

### Redaction

Timeline entries never store secrets, credentials, tokens, or unrestricted chat
content. Free-form fields are limited to bounded, redacted summaries; anything
that could carry sensitive payloads is excluded by construction.

### Immutability and replay

Finalized match and tournament records are immutable except through an explicit
audited correction flow. Late or replayed events cannot rewrite the finalized
record: entries are append-only, sequence numbers are enforced to be strictly
increasing, and duplicate or out-of-order events are rejected rather than
applied.

### Authorization

Timeline access and export are authorized. Only callers with the appropriate
role (support/operator review or the owning participant, per policy) may read or
export a match timeline; unauthorized callers are denied.

### Retention

Timeline retention is explicit and bounded. Entries expire after the configured
retention window; expiry is enforced so that access and export of expired
entries is refused. Retention policy is documented here and applied uniformly at
the service boundary.

## Validation

Coverage for a completed match exercises ordering, redaction, retention expiry,
and export behavior, plus authorization, failure handling, and concurrency or
replay behavior.
