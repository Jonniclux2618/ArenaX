# Handsoff

Handsoff is a production GitHub issue runner that proposes surgical, scope-faithful changes to real repositories.

## API contract tests

The published API operations are gated on OpenAPI response conformance. Contract tests live alongside the service boundary and validate, for each operation in scope:

- status codes
- required fields
- nullability
- error response shapes

A deliberate schema mismatch fails the test with the operation name and the offending field, so regressions are easy to locate. Generated fixtures are never used as a substitute for implementation-level checks — the tests exercise the real handlers.

### Adding an operation to the contract suite

1. Add the operation to the versioned OpenAPI specification.
2. Add a contract test for the operation next to the existing ones, asserting status codes, required fields, nullability, and error response shapes against the real handler.
3. Run the focused contract suite locally and confirm it passes in CI.

## API deprecation and sunset policy

Deprecated aliases are never removed without measurable usage and a documented warning window. The versioning flow at the canonical service boundary emits the following response headers on deprecated routes only:

- `Deprecation` — the date the alias was marked deprecated (RFC 7231 / IMF-fixdate).
- `Sunset` — the date the alias is scheduled for removal (RFC 7231 / IMF-fixdate).
- `Link` — a stable migration link, e.g. `<https://docs.example.com/migrations/v1-to-v2>; rel="deprecation"`.

Canonical (non-deprecated) routes must not emit any of these headers. Contract tests assert the headers are present on aliases and absent on canonical routes.

### Usage telemetry

Each deprecated operation records per-operation usage telemetry so removal is driven by data rather than guesswork. Each record captures:

- route version (e.g. `v1`)
- client class (e.g. `browser`, `server`, `unknown`)
- usage count

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

## Economic Simulation

The token and marketplace economics are exercised by a deterministic simulation that models multiple user archetypes, adversarial behavior, emission schedules, and liquidity shocks. It is a decision aid, not proof of safety.

### Scenario Inputs

Scenarios are checked in under `simulation/scenarios/` as versioned JSON. Each scenario pins the seed, archetype mix, adversarial actors, emission schedule, and liquidity-shock timeline so a run is reproducible from the checked-in inputs alone. Changing a scenario is a reviewed change to that file.

### Report Thresholds

The simulation report surfaces three classes of thresholds:

- **Solvency** — treasury/emission runway must stay positive across the full horizon under every scenario.
- **Concentration** — no single actor or archetype may exceed the reviewed share of supply or marketplace volume.
- **Sensitivity** — the report flags parameters whose small perturbation flips a solvency or concentration outcome.

### CI Smoke Scenarios

CI runs a small set of smoke scenarios with reviewed bounds. A smoke run fails when a scenario breaches its reviewed solvency, concentration, or sensitivity bounds. Focused tests cover retry, failure, and duplicate-delivery cases so the simulation harness itself is trusted.

### Updating Assumptions

Maintainers update assumptions by editing the checked-in scenario inputs and the reviewed bounds, then re-running the simulation and attaching the report to the change. Simulation results inform review; they do not replace audits, monitoring, or on-chain invariants.

## Handsoff notes

## Contributing

Keep changes surgical and inside the scope of the assigned issue. Do not install dependencies, run builds, or reformat unrelated code as part of a fix.

## Handsoff notes

<!-- handsoff-issue-22 -->
- #22: Reconcile provider balances and unresolved payments

<!-- handsoff-issue-167 -->
- #167: Propagate request deadlines and cancellation through service calls

<!-- handsoff-issue-169 -->
- #169: Build retry and dead-letter controls for background jobs
