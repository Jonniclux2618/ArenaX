# Development

## Getting started

1. Install dependencies with your package manager of choice.
2. Copy the example environment file and fill in local values.
3. Run the development server and open the app in a browser.

## Project layout

- `src/` — application source.
- `src/auth/` — authentication service boundary (password, OTP, and passkey flows).
- `tests/` — automated coverage.
- `tests/contract/` — OpenAPI response-conformance contract tests.

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
