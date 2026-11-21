# Tournament Seeding: Reproducible Input Snapshots

This document describes how tournament seeding is made reproducible from a
recorded input set (issue #209). A seed must be explainable and rebuildable even
after source ratings or manual overrides change later.

## Canonical service boundary

Seeding is performed by the tournament service at a single canonical boundary.
When a bracket is seeded, the service captures and persists an **input snapshot**
before computing any placement. The snapshot is the sole source of truth for a
seed run; later changes to ratings or overrides do not affect an existing seed.

## Recorded input snapshot

Every seed run persists the following, immutably:

- **Algorithm version** — the identifier of the seeding algorithm used (e.g.
  `seeding-v1`). Re-running with the same snapshot and version produces the same
  placement.
- **Eligibility snapshot** — the exact set of eligible participants and their
  eligibility status at capture time.
- **Source ratings** — the rating values (and their source) used for ordering at
  capture time.
- **Authorized overrides** — manual overrides that were applied, each recorded
  with the authorizing principal and the reason. Overrides not authorized for the
  acting principal are rejected and never enter the snapshot.
- **Deterministic tie-break key** — a stable key derived from the snapshot (not
  from wall-clock time or process state) used to break rating ties, so equal
  ratings always resolve to the same placement.

## Seed output

The seed output includes, alongside the placement:

- the algorithm/version,
- the eligibility snapshot reference,
- the deterministic tie-break key, and
- the list of authorized overrides applied.

This makes the seed auditable and reproducible.

## Reproducibility guarantees

- Re-running seeding with the same recorded snapshot produces **identical
  placement**, across process versions.
- Re-running seeding **never modifies a started bracket**. Once a bracket has
  started, its placement is fixed; re-seeding is a no-op against it.
- Finalized match and tournament records are immutable except through an
  explicit, audited correction flow.

## Audited correction flow

Any change to a finalized match or tournament record (including a corrected
seed) must go through the explicit correction flow, which records the acting
principal, the reason, and the before/after state. Direct mutation of finalized
records is not permitted.

## Seed audit report

A human-readable seed audit report can be produced from a recorded snapshot. It
lists the algorithm version, the eligibility snapshot, the source ratings, the
deterministic tie-break key, the authorized overrides, and the resulting
placement, so an operator can explain and rebuild any seed.

## Validation

Focused automated coverage verifies:

- stable output across process versions for the same snapshot,
- authorization of overrides (unauthorized overrides are rejected),
- failure handling when a snapshot is missing or malformed, and
- replay/concurrency behavior: re-running against a started bracket does not
  modify it.
