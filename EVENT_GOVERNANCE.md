# Event Governance

This document describes the operator-facing policies that govern finalized
tournament records. Finalized match and tournament records are immutable except
through the explicit, audited correction flows described below.

## Finalized bracket corrections

Manual bracket repairs can change advancement and prize eligibility, so a
finalized bracket may only be corrected through a two-person review flow.

### Correction proposal

A correction proposal is created at the canonical tournament service boundary
(not in ad-hoc API handlers) and must record:

- **Before state** — the finalized bracket state being corrected.
- **After state** — the proposed replacement state.
- **Reason** — why the correction is needed.
- **Evidence reference** — a link or identifier to supporting evidence.
- **Proposer** — the operator who created the proposal.
- **Bracket version** — the bracket version the proposal was built against.

### Approval policy

- A proposal requires an **independent approver**: the approver must not be the
  proposer. Same-actor approval is rejected.
- The approver is recorded on the correction alongside the proposer so the
  change is fully auditable.
- A proposal is **rejected if the bracket version has changed** since the
  proposal was created (stale proposal). The proposer must re-propose against
  the current version.
- Finalized payout transitions cannot be silently rewritten; any change to a
  finalized payout must go through this same audited correction flow.

### Validation expectations

Automated coverage should exercise:

- Same-actor approval rejection.
- Stale proposal handling when the bracket version changes.
- Replay behavior (a proposal cannot be applied twice).
- Downstream standings reconciliation after an approved correction.

## Dispute deadlines and escalation

Tournament result disputes follow a versioned deadline and escalation
lifecycle so that unresolved disputes cannot stall later rounds or payouts.
The lifecycle is enforced at the canonical tournament service boundary, not in
ad-hoc API handlers.

### Dispute record

Every dispute stores:

- **Opening time** — when the dispute was filed.
- **Evidence deadline** — the deadline by which supporting evidence must be
  submitted.
- **Assigned reviewer state** — who currently owns the dispute and whether it
  is unassigned, assigned, or escalated.
- **Resolution reason** — why the dispute was resolved (required on close).
- **Lifecycle version** — the dispute lifecycle version the record was created
  against, so deadline and escalation rules are applied consistently.

### Deadline and escalation policy

- Evidence submitted **after the evidence deadline** is recorded as late and
  does not by itself change the dispute outcome.
- Escalation reassigns the dispute to a higher reviewer state; it **does not
  auto-accept unverified claims**. An escalated dispute still requires an
  explicit resolution with a recorded reason.
- Reviewer reassignment is recorded so ownership changes remain auditable.
- Closing a dispute unlocks **only the valid next lifecycle transition** for the
  affected match or tournament; it does not skip ahead or reopen finalized
  records.
- Repeated resolution requests on an already-closed dispute are rejected
  (replay-safe); a closed dispute cannot be resolved twice.

### Validation expectations

Automated coverage should exercise:

- Deadline boundaries (evidence exactly at and just past the deadline).
- Late evidence handling.
- Reviewer reassignment and escalation without auto-acceptance.
- Repeated resolution requests and replay behavior.
- Authorization of who may escalate, reassign, and resolve a dispute.
