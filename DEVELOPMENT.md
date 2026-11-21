# Development

## Getting started

Install dependencies and run the local toolchain as described in the project README. Soroban contract tests run against the repository's Soroban test environment (`soroban-sdk` testutils).

## Cross-contract settlement conformance tests

Settlement spans four contracts — token, escrow, match, and tournament — and each can compile independently while disagreeing on authorization, units, or lifecycle semantics at the call boundary. To catch that, settlement is exercised through a shared integration fixture that drives the exact call sequences used in production.

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

### Running

Run the settlement conformance suite with the repository's Soroban test environment (the same command used for the rest of the contract tests). The suite must pass before merging changes that touch any of the token, escrow, match, or tournament boundaries.
