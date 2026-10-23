# Contributing to ArenaX

## Branches

| Pattern | Purpose |
|---|---|
| `main` | Production-ready code |
| `dev` | Integration branch |
| `feat/<name>` | New features |
| `fix/<name>` | Bug fixes |
| `chore/<name>` | Tooling, deps, config |
| `contract/<name>` | Smart contract changes |
| `migration/<name>` | DB schema changes |

All PRs target `dev`. Only `dev` → `main` merges go to production.

## Commit messages

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
feat(contracts): add match escrow vault dispute resolution
fix(backend): correct varchar/integer comparison in tournament view
chore(deps): bump next to 14.2.25
migration: add governance tables
```

Types: `feat`, `fix`, `chore`, `docs`, `refactor`, `test`, `migration`, `contract`

## Development setup

### JavaScript toolchain

Use Node.js 22.x and npm 10.9.3 for both `frontend/` and `server/`. The root
`.nvmrc` and each `package.json` declare the supported toolchain. npm is the
only package manager for these workspaces; commit changes to their respective
`package-lock.json` files and do not create a pnpm lockfile.

### Backend (Rust)
```bash
cd backend
cp env.example .env          # fill in DATABASE_URL, JWT_SECRET, etc.
cargo sqlx migrate run
cargo build
cargo test
```

### Contracts (Soroban)
```bash
cd contracts
rustup target add wasm32-unknown-unknown
cargo test
cargo build --target wasm32-unknown-unknown --release
```

### Server (TypeScript)
```bash
cd server
npm ci
npm run prisma:generate
npm run build
```

### Frontend (Next.js)
```bash
cd frontend
npm ci
npm run build
```

## Pull requests

- Keep PRs focused — one concern per PR.
- All CI checks must pass before merge
- Configure `Backend Build & Test` and `Backend Migrations` as required status
  checks on protected branches. The Rust backend job runs migrations, SQLx
  metadata validation, `cargo check`, and tests with PostgreSQL and Redis
  readiness checks; it is blocking in the workflow. Repository branch
  protection is maintained in GitHub settings, not by this workflow file.
- Smart contract changes require two approvals
- DB migrations must include a `.down.sql`
- Never force-push to `main` or `dev`

## Smart contract guidelines

- All public functions must have tests in `test.rs`
- Use `#[contracttype]` for all storage keys
- Bump the contract version constant on any interface change
- Document breaking changes in the PR description

## Database migrations

- Migrations are append-only — never edit an existing migration file
- Test both `up` and `down` migrations locally before opening a PR
- Column/type changes require a multi-step migration (add → backfill → drop)

## Dependency & supply-chain gate

CI runs a lockfile-aware dependency and software-supply-chain gate across every
package ecosystem in the repo (Rust `backend/Cargo.lock`, Node lockfiles in
`server/` and `frontend/`, and the `contracts/` workspace). The gate reports the
identity of both direct and transitive dependencies, checks them against known
advisories, and reports the license of each detected dependency.

- **High-severity findings fail the build.** A high/critical advisory blocks the
  PR or release unless a reviewed exception is recorded (see below).
- **Scan results are retained** as CI artifacts tied to release provenance
  (commit SHA, ref, and run id) so a release can be traced back to the exact
  dependency set that was scanned.
- **Normal builds stay offline-friendly.** The gate reads committed lockfiles and
  does not make ordinary `cargo build` / `npm ci` steps depend on network
  advisory lookups.

### Exception process

When a high-severity finding cannot be fixed immediately, record a time-bounded,
reviewed exception instead of disabling the gate:

1. Add an entry to the advisory exception file (e.g. `.github/security/advisory-exceptions.yml`)
   with the advisory ID, the affected package/ecosystem, a justification, and an
   **expiry date**.
2. Get review from a maintainer (security-sensitive — treat like a contract change).
3. The gate honors the exception only until its expiry date; once expired the
   finding fails the build again and the exception must be renewed or the
   dependency upgraded/removed.

Exceptions are scoped to a single advisory and package — never blanket-disable
the gate. Prefer upgrading or removing the vulnerable dependency over adding an
exception.
