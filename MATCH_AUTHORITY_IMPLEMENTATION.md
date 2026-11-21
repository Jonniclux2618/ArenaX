# Match Authority Implementation

This document describes how match state is owned, mutated, and repaired in the
backend. All match-state changes flow through the canonical match service
boundary; direct database edits are not supported.

## Canonical service boundary

Match state transitions (create, start, score, finalize, settle) are applied by
the match service. Settlement guards (rating and payout reconciliation) run as
part of finalization and must not be bypassed.

## Authorized match-state repair

Support engineers can correct a stuck match through an explicit, audited repair
command exposed at the match service boundary. A repair command is rejected
unless **all** of the following preconditions hold:

- **Reason** — a non-empty human-readable reason is supplied and stored on the
  audit record.
- **Actor permission** — the acting principal must hold the repair permission
  (operator/support role). Unauthorized actors are rejected before any state is
  read or written.
- **Expected current version** — the caller supplies the version they believe
  the match is at. If the stored version differs, the repair is rejected as a
  stale-version conflict and no state changes.
- **Allowed transition** — the requested `from -> to` transition must be in the
  allow-list of repairable transitions. Forbidden transitions are rejected.

### Idempotency and replay

Each repair carries an idempotency key. Replaying the same command (same key and
payload) returns the original result without applying the transition again or
writing a second audit record. Replays against an already-applied repair are
no-ops.

### Immutable audit record

Every successful repair appends an immutable audit record capturing the actor,
reason, idempotency key, previous version, new version, and the transition
applied. Audit records are append-only and are never mutated or deleted.

### Finalized records stay immutable

A repair can never reopen or double-settle a finalized match. Transitions out of
`Finalized` are not in the allow-list, and settlement guards continue to run so
a finalized match cannot be settled twice. Finalized match and tournament
records remain immutable except through this explicit audited correction flow.

## Validation

Focused automated coverage exercises:

- stale-version rejection,
- forbidden transition rejection,
- successful replay (idempotency),
- authorization and failure handling,
- downstream rating/payout reconciliation after a repair.
