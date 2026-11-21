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

## API Boundary Conventions

All canonical service responses use explicit boundary representations so clients never have to infer formatting.

### Money

- Monetary amounts are returned as an **exact decimal string** (e.g. `"1234.50"`) paired with an explicit ISO 4217 `currency` code (e.g. `"USD"`).
- Amounts are never returned as floats or as bare minor units; clients must not assume a scale.

### Timestamps

- All timestamps are emitted as **unambiguous UTC instants** in RFC 3339 form with a `Z` offset (e.g. `"2024-03-10T07:30:00Z"`).
- Local times and offset-less values are never returned; daylight-saving transitions therefore cannot change the meaning of a stored instant.

### Locale

- `locale` is an explicit client **preference** (BCP 47, e.g. `"en-US"`), not a server-inferred formatting decision.
- When a requested locale is unsupported, the service falls back to the documented default locale and reports the resolved locale in the response.

### Validation

- Invalid or unsupported `locale`, `currency`, or `timezone` input is rejected with a **typed validation error** (a stable error code plus the offending field) rather than being silently coerced or formatted server-dependently.

## Handsoff notes

<!-- handsoff-issue-19 -->
- #19: Introduce balanced wallet ledger invariants

<!-- handsoff-issue-22 -->
- #22: Reconcile provider balances and unresolved payments

<!-- handsoff-issue-175 -->
- #175: Correlate audit records across a complete user action

<!-- handsoff-issue-176 -->
- #176: Add contract tests for every external provider adapter

<!-- handsoff-issue-177 -->
- #177: Normalize locale, currency, and timezone at API boundaries

<!-- handsoff-issue-178 -->
- #178: Complete the Rust party lifecycle API
