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

Telemetry never records credentials (authorization headers, tokens, cookies) or sensitive query values; query parameters are reduced to their names before recording.

### Removal threshold and review

An alias may only be removed once it has been deprecated for at least the published sunset window **and** recorded usage has stayed below the agreed threshold (default: zero recorded calls over the trailing 30 days). Removal requires a documented review sign-off from the owning team, and the migration link must remain live for the full sunset window. Until both conditions are met, the alias stays in place and continues to emit the headers above.

## Contributing

Keep changes surgical and inside the scope of the assigned issue. Do not install dependencies, run builds, or reformat unrelated code as part of a fix.

## Handsoff notes

<!-- handsoff-issue-166 -->
- #166: Add cursor pagination for high-volume API collections
