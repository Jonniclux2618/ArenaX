# Development

## Getting started

Install dependencies and run the local toolchain as described in the project README. Soroban contract tests run against the repository's Soroban test environment (`soroban-sdk` testutils).

## Cross-contract settlement conformance tests

- `src/` — application source.
- `src/auth/` — authentication service boundary (password, OTP, and passkey flows).
- `tests/` — automated coverage.
- `tests/contract/` — OpenAPI response-conformance contract tests.

### Shared fixture

A single fixture (in the settlement integration test module) sets up the token, escrow, match, and tournament contracts once and exposes helpers that replay the canonical settlement sequence:

1. token `approve` / `transfer` for the escrowed amount
2. escrow `deposit` (or `fund`) for the match
3. match `settle` / `finalize` referencing the escrow
4. tournament `record_result` / `payout` consuming the settled match

Every conformance test reuses this fixture so the call graph under test matches the one settlement actually performs.

### What the tests assert

- **Authorization** — each cross-contract boundary is called with the expected auth context; unauthorized callers are rejected at the token, escrow, match, and tournament boundaries.
- **Amount units** — the same amount is expressed consistently (stroops / base units) across token, escrow, match, and tournament calls; no silent scaling between boundaries.
- **Event emission** — each contract emits its expected event on the settlement path, and the events carry the amounts and identifiers the next contract consumes.
- **Failure rollback** — a failure injected at any boundary rolls back the whole call graph, leaving no partial state and preserving immutable financial history and explicit transaction finality.

### Failure injection and edge cases

Focused automated tests inject failures at each cross-contract boundary and cover:

- **Retry** — a failed settlement can be retried and succeeds once the injected failure is removed, without double-spending.
- **Failure** — a mid-sequence failure reverts all prior writes across contracts.
- **Duplicate delivery** — replaying the same settlement (or the same event) is idempotent and does not create duplicate payouts or records.

## Soroban resource budgets

CI tracks per-entrypoint Soroban resource use so a hot contract entrypoint cannot silently
exceed its expected CPU, memory, or ledger read/write budgets even when the overall build
succeeds.

### Recording usage

For each representative entrypoint, CI runs the deterministic fixtures on the pinned
toolchain and records the measured resources:

- CPU instructions,
- memory bytes,
- ledger reads,
- ledger writes.

Measurements are captured per entrypoint (not just per contract) so a regression in one
entrypoint is attributable. Fixtures must be deterministic and produce stable results across
repeated CI runs on the pinned toolchain; flaky or non-deterministic fixtures are not
accepted as budget evidence.

### Versioned budgets

Reviewed budgets live in version control alongside the fixtures. Each budget entry names the
entrypoint and the expected ceiling for every tracked resource. Budgets are versioned: any
change to a budget is a reviewed change, not an incidental side effect of a code edit.

### Regression handling

CI compares recorded usage against the reviewed, versioned budgets and fails when an
entrypoint exceeds its budget. A failure MUST report:

- the entrypoint that regressed, and
- the per-resource delta (measured vs. budgeted) for each exceeded resource.

Intentional increases require a documented review: update the versioned budget in the same
change and record the rationale in the review so the increase is explicit and auditable.

### Validation

Use deterministic fixtures and verify stable results across repeated CI runs on the pinned
toolchain. Include focused automated tests for retry, failure, and duplicate-delivery cases
so budget enforcement itself is covered, not only the happy path.

## Testing

Add focused automated coverage for passkey authorization, failure handling, and recovery
boundaries. At minimum cover:

- origin substitution attempts,
- challenge replay,
- cloned credential metadata (sign-counter regression),
- multi-device registration,
- fallback behavior for unsupported clients.

## API contract tests

The versioned OpenAPI specification is the source of truth for published operations. Contract
tests in `tests/contract/` exercise the real service boundary (not generated fixtures) and
assert, for each operation in scope:

- the response **status code**,
- **required fields** are present,
- **nullability** matches the schema (nullable fields may be `null`; non-nullable fields may not),
- **error response shapes** for the documented failure statuses.

A deliberate schema mismatch must fail the test and report the offending **operation** and
**field** so the failure is actionable. Generated fixtures are never a substitute for these
implementation-level checks.

### Adding an operation to the contract suite

1. Add or update the operation in the versioned OpenAPI specification.
2. Add a case under `tests/contract/` that calls the real handler/service for that operation and
   asserts status code, required fields, nullability, and error shapes against the spec.
3. Include at least one failure-path case (e.g. authorization or validation error) so error
   response shapes are covered.
4. Run the focused contract suite locally and confirm CI runs it on every change.

## Contributing

Keep changes scoped, follow existing patterns, and include tests for security-relevant
behavior. Do not weaken existing authentication flows when adding new ones.
