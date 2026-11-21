# Project

## Backup & Recovery Objectives

This section defines the tested recovery capability for the production data stores.

### Recovery Objectives

| Data store | RPO (max data loss) | RTO (max downtime) |
| --- | --- | --- |
| PostgreSQL (primary database) | 15 minutes | 2 hours |
| Required metadata (schema, migration state) | 15 minutes | 2 hours |
| Required secrets (app config, credentials) | 24 hours | 4 hours |

RPO is measured from the last verified backup to the point of failure. RTO is measured from incident declaration to restored, validated service with traffic cut over.

### Backup Requirements

- **Encrypted at rest and in transit.** Backups are encrypted with the managed KMS key before leaving the database host; transport uses TLS.
- **Access-limited.** Only the backup service identity and on-call operators (break-glass) can read backup artifacts. Artifacts are stored in a private bucket with least-privilege IAM and a defined retention window.
- **Independently verified.** A separate verification job (not the backup job) downloads each artifact, recomputes its checksum, and compares it against the recorded manifest checksum. Verification failures alert the on-call engineer.

### Restore Drill Procedure

Run this drill at least quarterly in staging and retain the report.

1. **Provision an isolated target.** Create a clean staging database instance; do not restore over a live environment.
2. **Restore PostgreSQL.** Fetch the latest verified backup, decrypt it, and restore into the isolated instance. Record start and end timestamps.
3. **Restore metadata and secrets.** Apply the metadata/schema snapshot and load required secrets from the secrets manager into the isolated environment.
4. **Verify integrity.** Recompute checksums on the restored data and confirm they match the backup manifest.
5. **Application validation.** Point a staging app instance at the restored database and run smoke checks: migrations applied, health endpoint green, authentication works, and a representative read/write round-trip succeeds.
6. **Safe traffic cutover.** Only after validation passes, update the staging routing/connection strings to the restored instance, confirm healthy traffic, and keep the previous instance available for rollback until the drill is signed off.
7. **Record results.** Capture measured RTO (incident start to validated cutover) and measured data loss (RPO gap) in the restore-drill report.

### Restore-Drill Report

Retain a report per drill containing:

- Drill date, operator, and environment
- Backup artifact ID and verification result
- Measured restore duration vs. RTO target
- Measured data loss vs. RPO target
- Application validation results
- Cutover outcome and any follow-up actions

A drill passes only when measured RTO and RPO are within the objectives above and application validation succeeds.

## Typed Soroban Client Bindings

Client bindings are generated from the checked-in contract interface artifact (`contracts/interfaces/soroban-interface.json`). The artifact is the single source of truth for Soroban function signatures, return values, emitted events, and contract error codes.

### Versioning and Pinning

- Every generated binding set is stamped with the artifact's `network` and `contractVersion`.
- Bindings refuse to construct a client when the requested network or contract version does not match the artifact they were generated from, so a consumer cannot accidentally mix network or contract versions.
- Regenerate bindings whenever the interface artifact changes; never hand-edit generated output.

### Generation and CI Verification

- Regenerate Rust and TypeScript bindings from the artifact with the project's binding generator.
- CI runs the generator in check mode and fails the build when the committed bindings are stale relative to the interface artifact, so generated output stays current.

### Consumer Validation

- A small Rust consumer and a small TypeScript consumer compile against the generated bindings.
- Focused automated tests exercise a deliberate interface change plus retry, failure, and duplicate-delivery cases to confirm typed arguments, return values, events, and contract errors stay in sync with the artifact.

## Handsoff notes

<!-- handsoff-issue-19 -->
- #19: Introduce balanced wallet ledger invariants

<!-- handsoff-issue-22 -->
- #22: Reconcile provider balances and unresolved payments

<!-- handsoff-issue-223 -->
- #223: Expose a stable payment lifecycle timeline to wallet clients

<!-- handsoff-issue-226 -->
- #226: Generate typed client bindings from deployed contract interfaces
