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

## Handsoff notes

<!-- handsoff-issue-19 -->
- #19: Introduce balanced wallet ledger invariants

<!-- handsoff-issue-22 -->
- #22: Reconcile provider balances and unresolved payments
