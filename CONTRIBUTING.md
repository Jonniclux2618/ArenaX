# Contributing

Thanks for contributing! This document covers the workflow, coding standards, and CI expectations for this repository.

## Getting started

1. Fork the repository and create a topic branch from `main`.
2. Install the pinned toolchain described in `rust-toolchain.toml` (or the project README).
3. Run the local checks below before opening a pull request.

## Local checks

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

## Soroban resource budgets

CI tracks the Soroban resource usage of representative contract entrypoints so that a hot entrypoint cannot silently exceed its expected CPU, memory, or ledger read/write budget.

### How it works

- A deterministic fixture suite exercises the representative entrypoints (payments, data access, and other Soroban boundary calls) on the pinned toolchain.
- For each entrypoint, CI records CPU instructions, memory bytes, and ledger reads/writes.
- Recorded usage is compared against the reviewed, versioned budget definitions checked into the repository.
- Results are stable across repeated CI runs because fixtures are deterministic and the toolchain is pinned.

### Budget definitions

Budgets live in the versioned budget file alongside the contract sources. Each entry names an entrypoint and its reviewed limits for CPU instructions, memory, and ledger reads/writes. Treat these values as reviewed contract: they are the agreed ceiling for that entrypoint.

### When a budget regression fails CI

If recorded usage exceeds a reviewed budget, CI fails and reports:

- the entrypoint that regressed, and
- the per-resource delta (recorded vs. budgeted) for CPU, memory, and ledger reads/writes.

To resolve a failure, either:

1. **Fix the regression** so usage returns within budget — preferred when the increase was unintentional.
2. **Intentionally raise the budget** by updating the versioned budget file. Intentional increases require a documented review: explain in the pull request why the higher budget is necessary, what was measured, and confirm the new ceiling was reviewed by a maintainer. The budget change and the code change should be reviewed together.

Do not raise budgets to make a red build green without that documented justification.

### Tests

Focused automated tests cover retry, failure, and duplicate-delivery cases for the budgeted entrypoints. Keep these tests deterministic; do not introduce nondeterministic inputs, wall-clock dependencies, or unpinned toolchain assumptions.

## Pull requests

- Keep changes focused and scoped to the linked issue.
- Describe the behavior change and how it was validated.
- Ensure CI is green, including the resource budget check.

## Commit messages

Use clear, imperative commit messages (e.g. `fix: track per-entrypoint Soroban resource budgets in CI`).
