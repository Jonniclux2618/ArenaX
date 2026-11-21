# Development

## Getting started

1. Install dependencies with your package manager of choice.
2. Copy the example environment file and fill in local values.
3. Run the development server and open the app in a browser.

## Project layout

- `src/` — application source.
- `src/auth/` — authentication service boundary (password, OTP, and passkey flows).
- `tests/` — automated coverage.

## Authentication

### Passkeys (WebAuthn)

Passkeys are implemented at the auth service boundary in `src/auth/`. Registration and login
rely on platform WebAuthn verification and MUST enforce all of the following server-side:

- **Origin** — the asserted origin must match the configured allowed origin(s).
- **Challenge** — challenges are generated per ceremony, stored server-side, single-use, and
  rejected on replay or expiry.
- **RP-ID** — the relying party ID must match the configured RP-ID and be a registrable suffix
  of the origin.
- **User verification** — require `userVerification: "required"` and verify the UV flag in the
  authenticator data.

Credential metadata (credential ID, public key, sign counter, transports, AAGUID) is persisted
per user and per device so multi-device registration is supported. Sign counters are checked to
detect cloned credentials; a counter regression must fail the assertion and be audited.

### Recovery and credential removal

Removing a passkey or recovering an account MUST require a separate verified factor (for
example a verified email/OTP or password re-authentication). A passkey alone must never
authorize its own removal or account recovery. Recovery actions are explicit, logged for
auditability, and never silently weaken existing factors.

### Fallback

Clients that do not support WebAuthn keep the existing password and OTP flows unchanged.
Passkey support is additive and must not relax the guarantees of the existing flows.

## Testing

Add focused automated coverage for passkey authorization, failure handling, and recovery
boundaries. At minimum cover:

- origin substitution attempts,
- challenge replay,
- cloned credential metadata (sign-counter regression),
- multi-device registration,
- fallback behavior for unsupported clients.

## Reproducible local environment

Start the supported PostgreSQL, PgBouncer, Redis, object-storage, and message
queue dependencies with:

```sh
make local-up
./scripts/local-dev.sh seed
```

The first command creates an ignored `.env.local` containing development-only
defaults and waits for Docker health checks. Repeat `make local-up` safely;
it only reconciles containers. `make local-seed` seeds deterministic synthetic
data once. Use `make local-down` for a teardown that preserves named volumes.
No production credentials or real identities are required or committed.

## Contributing

Keep changes scoped, follow existing patterns, and include tests for security-relevant
behavior. Do not weaken existing authentication flows when adding new ones.
