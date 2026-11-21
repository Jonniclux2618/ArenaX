# Backend

Rust (Axum) API service for the evidence and media platform.

## Running locally

## Tech Stack

- **Language**: Rust
- **Web Framework**: Actix-web + Axum
- **Async Runtime**: Tokio
- **Database**: PostgreSQL (with sharding)
- **Cache & Messaging**: Redis (Pub/Sub, leaderboards)
- **Storage**: S3/MinIO
- **Blockchain**: Stellar Rust SDK + Soroban smart contracts
- **Payments**: Paystack, Flutterwave
- **AI**: TensorFlow Lite (via `tract` crate)

## Architecture

### Microservices Design
- **Auth Service**: User authentication and Stellar account management
- **Tournament Service**: Tournament lifecycle and prize pool management
- **Match Service**: Matchmaking, scoring, and dispute handling
- **Wallet Service**: Payment processing and Stellar integration
- **Anti-Cheat Service**: AI-powered cheating detection
- **Leaderboard Service**: Real-time rankings and reputation tracking

### Stellar Integration
- **Account Management**: Create and manage user Stellar accounts
- **Asset Issuance**: ArenaX Tokens and Reputation Tokens
- **Smart Contracts**: Soroban contracts for automated operations
- **Transaction Processing**: Prize pool management and payouts

## Backup & Recovery Objectives

PostgreSQL is the single source of truth for financial, match, session, and wallet state (see the Redis policy below). Backups therefore cover **PostgreSQL plus the required metadata and secrets** needed to bring the service back: database credentials, JWT signing keys, Stellar signing keys, and payment provider keys.

### Recovery objectives

| Data set | RPO (max data loss) | RTO (max downtime) |
| --- | --- | --- |
| PostgreSQL (all schemas) | **15 minutes** (continuous WAL archiving + 15-min base backups) | **60 minutes** |
| Required metadata (migrations, config) | 0 (versioned in git) | 15 minutes |
| Required secrets (JWT, Stellar, payment keys) | 0 (stored in the secrets manager with versioning) | 15 minutes |

These objectives are the contract the backup and restore-drill procedures below are measured against. A drill that misses either objective is a failed drill and must be fixed before the next release.

### Backup workflow

- **Encrypted at rest:** every PostgreSQL dump and WAL segment is encrypted with `age` (or `gpg`) using a public recipient key before leaving the host. Plaintext backups are never written to shared storage.
- **Access-limited:** backups are written to a dedicated bucket/prefix with least-privilege IAM — the backup writer can only `PutObject`, and only the restore role can `GetObject`. Bucket versioning and object lock are enabled so backups cannot be silently deleted or overwritten.
- **Retention:** 35 days of daily base backups plus 7 days of WAL, matching the RPO above. Retention is enforced by lifecycle policy, not by the backup job.
- **Independently verified:** a separate verification job (not the backup job) downloads each new backup, recomputes its SHA-256 checksum, compares it against the checksum recorded at creation time, and performs a `pg_restore --list` / `pg_verifybackup` integrity check. Verification failures page on-call and mark the backup as untrusted.

### Restore-drill procedure

Run this drill at least monthly and before any release that changes the schema. Retain the resulting report.

1. **Provision an isolated staging target.** Use a throwaway database instance and a separate secrets scope; never restore over production.
2. **Restore PostgreSQL.** Fetch the latest verified base backup and the WAL needed to reach the target point in time, decrypt, and restore. Record the wall-clock start and end.
3. **Restore metadata and secrets.** Apply migrations to the restored database and load the required secrets into the staging scope from the secrets manager.
4. **Verify integrity.** Confirm the restored database passes `pg_verifybackup`/checksum validation and that row counts for critical tables (`wallets`, `transactions`, `matches`, `tournaments`, `sessions`) match the source snapshot.
5. **Validate the application.** Start the backend against the restored database and secrets, then check `GET /health` and `GET /ready`, run the smoke test suite, and confirm a representative read (leaderboard) and a representative write (a test match result) succeed.
6. **Measure against objectives.** Record the measured RTO (time from drill start to a passing application validation) and the measured data loss (difference between the source's latest committed transaction and the restored point). Both must be within the objectives table above.
7. **Safe traffic cutover (production only).** If this drill is being used for an actual recovery: put the service in maintenance mode, drain in-flight requests, point the application at the restored database, verify `/ready`, then shift traffic gradually (canary → full) while watching error rates. Roll back by repointing to the previous database if validation fails.
8. **Retain the report.** Store the drill report (date, operator, measured RTO, measured data loss, pass/fail, and any remediation) alongside the backup records.

### Automated isolated failover drills

The scheduled staging drill is implemented by
`backend/scripts/run-dr-failover-drill.sh`. It refuses every non-staging target,
records detection, promotion, connection recovery, and point-in-time integrity,
and compares measured RPO/RTO with the 15-minute/60-minute PostgreSQL contract.
CI or the infrastructure scheduler must retain the JSON report and assign a
remediation owner whenever the drill fails. A drill is not a production cutover.

## Redis Failure Semantics & Recovery Policy

Redis is used for **derived, rebuildable state only**. PostgreSQL (and the Stellar ledger for on-chain effects) is the single source of truth for all financial, match, session, and wallet state. No authoritative state may exist only in Redis.

### Redis usage inventory

| Redis-backed operation | Classification | Durable backing |
| --- | --- | --- |
| Leaderboard / ranking sorted sets | Cache (derived) | `matches`, `tournaments`, reputation rows in Postgres |
| Matchmaking queue | Ephemeral coordination | Match records in Postgres; queue is re-derivable from open matches |
| Pub/Sub event fan-out (match/session/wallet events) | Transport (at-most-once) | Event rows in Postgres; consumers reconcile from durable records |
| Session / auth token cache | Cache (derived) | `sessions` table in Postgres; JWT is authoritative |
| Wallet balance / payout cache | Cache (derived) | `wallets`, `transactions` tables + Stellar ledger |
| Rate-limit counters | Ephemeral | None required; loss only loosens limits temporarily |

**Invariant:** financial and final match state are always committed to Postgres (and, where applicable, confirmed on-chain) *before* any Redis write. Redis is never the commit point.

### Degraded behavior per operation

- **Leaderboard reads/writes:** on Redis error, serve rankings computed directly from Postgres (slower path). Writes are best-effort; a failed write is logged and the entry is rebuilt on next read. No duplicate scoring — scores are recomputed from durable match results, not incremented blindly.
- **Matchmaking queue:** if Redis is unavailable, new matchmaking requests are rejected with `503` and a `Retry-After` header rather than silently queued in memory (avoids split-brain duplicate matches). Existing matches continue from Postgres.
- **Pub/Sub:** publishers treat delivery as best-effort; on publish failure the event is already persisted in Postgres and consumers pick it up via the durable event log. Consumers are idempotent (keyed by event id) so replays never create duplicate work.
- **Session cache:** cache miss or Redis outage falls back to validating against the `sessions` table. Auth never fails open.
- **Wallet cache:** reads fall back to Postgres/Stellar; writes go to Postgres first and invalidate the cache. A stale cache is never trusted for balance checks.
- **Rate limiting:** on Redis outage, fall back to a conservative in-process limiter; counters reset on recovery.

### Recovery policy

After a Redis restart or partial connectivity:

1. **Reconnect** with bounded exponential backoff; the process stays up and serves degraded paths meanwhile.
2. **Rebuild derived state** from durable records: leaderboards are recomputed from Postgres match results, session and wallet caches are lazily repopulated on read, and the matchmaking queue is reconstructed from open matches.
3. **Replay** any Pub/Sub events missed during the outage by draining the durable event log; consumers dedupe by event id so replay is idempotent.
4. **No data loss for authoritative state** — because it was never only in Redis, recovery never needs to reconstruct financial or final match state from the cache.

### Health & readiness signals

- `GET /health` reports process liveness.
- `GET /ready` reports readiness and includes a `redis` component status (`ok` / `degraded` / `down`). Redis being `degraded` does **not** fail readiness for operations that have a durable fallback; it is surfaced so operators can act. Operations with no safe fallback (matchmaking enqueue) return `503` while Redis is down.

### Validation

Integration tests exercise Redis restart, timeout, and partial-connectivity scenarios to confirm: no authoritative state is lost, degraded paths behave as documented, and recovery rebuilds derived state without duplicate work.

## Project Structure

```
backend/
├── src/
│   ├── api_error.rs       # Centralized error handling and API error types
│   ├── config.rs          # Configuration management and environment variables
│   ├── db.rs              # Database connection and connection pooling
│   ├── lib.rs             # Library root and public API exports
│   ├── main.rs            # Application entry point and server startup
│   ├── middleware.rs      # Custom middleware implementations
│   ├── telemetry.rs       # Logging, tracing, and observability setup
│   ├── auth/              # Authentication and authorization modules
│   │   ├── mod.rs         # Auth module exports
│   │   ├── jwt.rs         # JWT token handling
│   │   ├── stellar.rs     # Stellar account management
│   │   └── otp.rs         # OTP verification system
│   ├── http/              # HTTP handlers and route definitions
│   │   ├── mod.rs         # HTTP module exports
│   │   ├── auth.rs        # Authentication endpoints
│   │   ├── tournaments.rs # Tournament management endpoints
│   │   ├── matches.rs     # Match and matchmaking endpoints
│   │   ├── wallet.rs      # Payment and Stellar integration endpoints
│   │   ├── leaderboard.rs # Rankings and reputation endpoints
│   │   └── health.rs      # Health check and monitoring endpoints
│   ├── service/           # Business logic and service layer
│   │   ├── mod.rs         # Service module exports
│   │   ├── auth_service.rs    # Authentication business logic
│   │   ├── tournament_service.rs # Tournament management logic
│   │   ├── match_service.rs     # Match and matchmaking logic
│   │   ├── wallet_service.rs    # Payment processing logic
│   │   ├── anti_cheat_service.rs # AI-powered cheating detection
│   │   ├── leaderboard_service.rs # Rankings and reputation logic
│   │   └── stellar_service.rs   # Stellar blockchain integration
│   └── utils/             # Shared utilities and helper functions
│       ├── mod.rs         # Utils module exports
│       ├── crypto.rs      # Cryptographic utilities
│       ├── validation.rs  # Input validation helpers
│       ├── stellar_utils.rs # Stellar-specific utilities
│       └── constants.rs   # Application constants
├── migrations/            # Database migrations (SQLx)
├── scripts/              # Deployment and utility scripts
│   ├── deploy.sh         # Production deployment script
│   ├── migrate.sh        # Database migration script
│   └── test_stellar.sh   # Stellar integration testing
├── tests/                # Integration and unit tests
│   ├── common/           # Shared test utilities
│   ├── integration/      # End-to-end integration tests
│   └── unit/             # Unit tests for individual modules
├── target/               # Rust build artifacts
├── Cargo.toml           # Rust dependencies and workspace configuration
├── Cargo.lock           # Locked dependency versions
├── Dockerfile           # Container configuration
├── test_ci_locally.sh   # Local CI testing script
└── README.md            # This documentation
```

## Setup & Development

### Prerequisites
- Rust toolchain (`cargo`, `rustup`) - latest stable version
- PostgreSQL (v15+) with sharding support
- Redis (v7+) with Pub/Sub enabled
- `sqlx-cli` (v0.8.6)
- MinIO (or AWS S3) for file storage
- Docker and Docker Compose for local development
- Stellar CLI and SDK for blockchain integration
- Node.js (v18+) for testing scripts

### Build & CI Prerequisites
Building all targets and running tests requires reachable PostgreSQL and Redis instances, as SQLx query macros perform compile-time SQL verification against the migrated schema:

- `DATABASE_URL`: Connection string to migrated PostgreSQL database (e.g. `postgres://arenax:arenax@localhost:5432/arenax_ci`).
- `REDIS_URL`: Connection string to Redis server (e.g. `redis://localhost:6379`).

CI runs without `continue-on-error` on `backend-build`, strictly enforcing that compiler checks, type validation, and tests pass against live services.

### Installation

```bash
# Requires a Docker daemon for testcontainers.
cargo test --all-features -- --test-threads=8
```

### Development & CI Commands

```bash
# Type check all targets (lib, bin, tests, examples) against migrated database
cargo check --all-targets --locked

# Run tests against live database and Redis
cargo test --all-targets --locked

# Run tests with coverage
cargo test --coverage

# Run integration tests
cargo test --test integration

# Run specific test module
cargo test auth::tests

# Check code formatting
cargo fmt

## Service SLOs, Alerts & Runbooks

Operational targets are tied to player-impacting workflows rather than generic health
indicators. Each SLO below has a measured signal, evaluation window, owner, and an
error-budget response. Signals are emitted by `src/telemetry.rs` (structured request
metrics) and surfaced in the dashboards described in [docs/OBSERVABILITY.md](../docs/OBSERVABILITY.md).

### SLO Definitions

| Workflow | SLI (measured signal) | SLO target | Window | Owner |
| --- | --- | --- | --- | --- |
| **Login** | Ratio of `POST /auth/login` responses with status < 500 and p95 latency | 99.9% success, p95 < 400 ms | 30 days (rolling) | Auth team |
| **Tournament join** | Ratio of `POST /tournaments/{id}/join` responses with status < 500 and p95 latency | 99.5% success, p95 < 800 ms | 30 days (rolling) | Tournament team |
| **Match updates** | Ratio of `PATCH /matches/{id}` (score/state) responses with status < 500 and p95 latency | 99.5% success, p95 < 500 ms | 30 days (rolling) | Match team |
| **Settlement** | Ratio of settlement jobs completing without error and p95 job duration | 99.0% success, p95 < 5 s | 30 days (rolling) | Wallet team |

### SQLx Offline Compilation & Query Metadata Workflow

ArenaX supports SQLx offline mode so developers and CI can build the backend targets without needing a live, persistent PostgreSQL database.

#### 1. Building in Offline Mode
To compile the backend without connecting to a database:
```bash
# Enable offline mode using the committed .sqlx query metadata
SQLX_OFFLINE=true cargo check --all-targets --locked
```

#### 2. Generating & Updating Query Metadata
Whenever you write new queries, edit existing `sqlx::query!` / `sqlx::query_as!` macros, or add/alter database migrations:
1. Ensure your local PostgreSQL service is running and migrations are applied:
   ```bash
   export DATABASE_URL="postgres://arenax:arenax@localhost:5432/arenax_ci"
   ./scripts/migrate.sh
   ```
2. Generate reproducible query metadata across all targets:
   ```bash
   cargo sqlx prepare -- --all-targets
   ```
3. Commit the updated query metadata files in the `.sqlx/` directory to version control.

#### 3. CI Detection of Stale or Missing Metadata
CI validates that committed query metadata exactly matches the real migration schema:
```bash
cargo sqlx prepare --check -- --all-targets
```
If a query macro has changed without updating `.sqlx/`, or if metadata is missing, this check will fail CI with actionable errors identifying the out-of-sync queries.

#### 4. When Online Database Validation is Still Required
- **Schema & Query Changes**: Modifying table definitions, adding migrations, or writing new SQL queries **must** be validated against a live database using `cargo sqlx prepare -- --all-targets` or online `cargo check`. Offline mode only replays previously validated query signatures.
- **Runtime Testing**: Running integration tests (`cargo test`) that execute database transactions requires a real PostgreSQL database instance.

### Environment Variables

```env
# Database
DATABASE_URL=postgres://user:pass@localhost:5432/arenax
BACKEND_MIGRATION_MODE=run
REDIS_URL=redis://localhost:6379

# Storage
S3_ENDPOINT=http://localhost:9000
S3_ACCESS_KEY=minio

# ── Secrets ──────────────────────────────────────────────────────────────────
# Every secret must be at least 32 characters long and must not be a known
# placeholder (`supersecretkey`, `changeme`, ...). The backend refuses to start
# otherwise. Generate real values with `openssl rand -base64 48`.
S3_SECRET_KEY=dev-only-s3-secret-replace-me-0123456789

# Payments
PAYSTACK_SECRET=dev-only-paystack-secret-replace-me-0123456789
FLUTTERWAVE_SECRET=dev-only-flutterwave-secret-replace-me-0123456789

# Authentication
JWT_SECRET=dev-only-jwt-secret-replace-me-0123456789abcdef
# Optional: dedicated refresh-token secret (falls back to JWT_SECRET when unset)
JWT_REFRESH_SECRET=

# Stellar Configuration
STELLAR_NETWORK_URL=https://horizon-testnet.stellar.org
STELLAR_ADMIN_SECRET=dev-only-stellar-admin-secret-replace-me
SOROBAN_CONTRACT_PRIZE=CAXXX...
SOROBAN_CONTRACT_REPUTATION=CBXXX...

# AI
AI_MODEL_PATH=./models/anti_cheat.tflite
```

Each SLO carries a 30-day error budget (`1 - SLO target`). Response is burn-rate driven:

- **< 50% budget consumed**: normal operation, no action.
- **50–75% consumed**: owner reviews recent error sources weekly; file follow-ups for top contributors.
- **75–100% consumed**: freeze non-critical changes to the affected workflow; owner prioritizes reliability fixes.
- **Budget exhausted**: feature work on the workflow pauses until the budget recovers; incident review required.

### Alert Thresholds

Alerts are derived from the SLOs and use multi-window burn rates so transient noise does
not page. A page fires only when **both** a fast (5 min) and slow (1 h) window exceed the
threshold; single-window spikes route to the team channel instead of paging.

| Alert | Condition | Severity |
| --- | --- | --- |
| `LoginErrorBudgetFastBurn` | 5 min error rate > 14.4x budget **and** 1 h error rate > 14.4x budget | Page |
| `LoginLatencyDegraded` | p95 > 400 ms sustained for 10 min | Ticket |
| `TournamentJoinFastBurn` | 5 min error rate > 14.4x budget **and** 1 h error rate > 14.4x budget | Page |
| `MatchUpdateFastBurn` | 5 min error rate > 14.4x budget **and** 1 h error rate > 14.4x budget | Page |
| `SettlementFastBurn` | 5 min failure rate > 14.4x budget **and** 1 h failure rate > 14.4x budget | Page |
| `SettlementBacklog` | pending settlements > 100 for 15 min | Ticket |

Transient single-request failures, brief latency blips, and deploy-window noise are
excluded via the sustained-window requirement and a 5-minute deploy suppression.

### Runbooks

Each alert links to a runbook covering first checks, safe mitigations, escalation, and
recovery verification.

**Login (`LoginErrorBudgetFastBurn` / `LoginLatencyDegraded`)**
1. *First checks*: confirm auth service health, DB connection pool saturation, and recent deploys in the last 30 min.
2. *Safe mitigations*: roll back the last auth deploy if correlated; scale auth replicas; verify Redis session store reachable.
3. *Escalation*: page Auth on-call; escalate to platform on-call if DB or Redis is the root cause.
4. *Recovery verification*: error rate back under budget for 30 min and p95 < 400 ms on the dashboard.

**Tournament join (`TournamentJoinFastBurn`)**
1. *First checks*: tournament service health, DB write latency, and lock contention on tournament rows.
2. *Safe mitigations*: shed load via rate limiting; disable non-essential join side effects; roll back recent tournament deploy.
3. *Escalation*: Tournament on-call; platform on-call for DB issues.
4. *Recovery verification*: join success rate ≥ 99.5% over 30 min and p95 < 800 ms.

**Match updates (`MatchUpdateFastBurn`)**
1. *First checks*: match service health, Redis Pub/Sub lag, and downstream anti-cheat latency.
2. *Safe mitigations*: decouple anti-cheat scoring to async if blocking; roll back recent match deploy; scale match replicas.
3. *Escalation*: Match on-call; anti-cheat owner if AI service is the bottleneck.
4. *Recovery verification*: update success rate ≥ 99.5% over 30 min and p95 < 500 ms.

**Settlement (`SettlementFastBurn` / `SettlementBacklog`)**
1. *First checks*: Stellar Horizon reachability, wallet service health, and settlement job queue depth.
2. *Safe mitigations*: pause new settlement submissions; retry failed jobs idempotently; fail over to backup Horizon endpoint.
3. *Escalation*: Wallet on-call; escalate to Stellar integration owner for chain-side failures.
4. *Recovery verification*: settlement success rate ≥ 99.0% over 30 min, backlog drained, and p95 job duration < 5 s.

### Staging Validation

To validate alerts and runbooks, simulate one dependency outage in staging:

1. Block egress to Stellar Horizon (or stop Redis) in the staging environment.
2. Confirm the corresponding burn-rate alert fires within the fast window and pages the correct owner.
3. Walk the runbook end-to-end: perform first checks, apply a safe mitigation, and verify escalation routing.
4. Restore the dependency and confirm recovery verification steps pass (error rate and latency back within SLO).
5. Record gaps found in the runbook and update it before the next drill.

## Project Structure

```
backend/
├── src/
│   ├── api_error.rs       # Centralized error handling and API error types
│   ├── config.rs          # Configuration management and environment variables
│   ├── db.rs              # Database connection and connection pooling
│   ├── lib.rs             # Library root and public API exports
│   ├── main.rs            # Application entry point and server startup
│   ├── middleware.rs      # Custom middleware implementations
│   ├── telemetry.rs       # Logging, tracing, and observability setup
│   ├── auth/              # Authentication and authorization modules
│   │   ├── mod.rs         # Auth module exports
│   │   ├── jwt.rs         # JWT token handling
│   │   ├── stellar.rs     # Stellar account management
│   │   └── otp.rs         # OTP verification system
│   ├── http/              # HTTP handlers and route definitions
│   │   ├── mod.rs         # HTTP module exports
│   │   ├── auth.rs        # Authentication endpoints
│   │   ├── tournaments.rs # Tournament management endpoints
│   │   ├── matches.rs     # Match and matchmaking endpoints
│   │   ├── wallet.rs      # Payment and Stellar integration endpoints
│   │   ├── lead

| Status | Meaning |
| --- | --- |
| `400` | Invalid request (validation failure) |
| `401` | Missing or invalid bearer token |
| `403` | Authenticated but not authorized for this action |
| `404` | Resource not found |
| `409` | Conflict (e.g. duplicate resource) |
| `429` | Rate limited |
| `500` | Internal server error |

# Increase case count locally
PROPTEST_CASES=10000 cargo test --test property_brackets
```

/* … truncated 3893 chars — edit only what you need near the top … */
