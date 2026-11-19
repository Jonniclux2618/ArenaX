# Security audit field policy

Audit records keep the minimum data needed to establish who performed an
action, what resource was affected, and when it happened. The write path applies
field redaction before computing/persisting snapshots, and audit reads and
exports redact legacy snapshots as well.

## Event schemas

| Event family | Required fields | Pseudonymized fields | Prohibited fields | Retention class |
|---|---|---|---|---|
| Authentication and administration | Actor, action, resource type, resource ID | None; attribution remains available to authorized investigators | Passwords/hashes, secrets/keys, tokens, authorization/cookies, full request bodies, email, phone, IP address, user agent | Security, 2,555 days |
| Financial and KYC | Actor, action, resource type, resource ID | None; ledger identity and amounts remain attributable to finance/auditors | Passwords/secrets/keys, tokens, authorization/cookies, full request bodies, email, phone, IP address, user agent | Financial, 2,555 days |
| Gameplay | Actor, action, resource type, resource ID | None | Passwords/secrets/keys, tokens, authorization/cookies, full request bodies, email, phone, IP address, user agent | Gameplay, 730 days |
| Other operational events | Actor, action, resource type, resource ID | None | Passwords/secrets/keys, tokens, authorization/cookies, full request bodies, email, phone, IP address, user agent | Operational, 365 days |

Database-triggered records use the `audit_event_policies` table. Application
records in the TypeScript service store their retention class and field policy
with the event. The Rust audit service applies the same prohibited-field
baseline. Any new event family must add a policy before its payload is logged.

The DB policy sets `retained_until` on each new trigger record, and archive
sweeps use that stored per-row deadline. Updating a policy does not change
existing rows' expiry dates or restore fields already removed. Older rows are
redacted at read/export time, even though their append-only stored
representation is preserved for chain verification.

## Data handling

- Redaction recursively omits prohibited fields before persistence.
- IP addresses and user-agent strings are not stored in these audit paths.
- Audit reads and exports apply compatibility redaction to historical
  snapshots; they do not expose raw request bodies or credential fields.
- Authorization is evaluated against the shared Redis session/blacklist state
  for JWTs and the owning database record for API keys; no in-process cache is
  used to continue accepting a revoked credential.
- Redis session revocation is shared across instances. Long-lived WebSockets
  revalidate the session and token expiry every 15 seconds and fail closed if
  the revocation store cannot be reached.
- API credentials are stored as hashes; revocation is checked against the
  owning database record on use. Redis outages cause session authorization to
  fail closed rather than extending access.

Account and security metadata remains restricted to authorized operations
staff; financial records retain the existing seven-year accounting exception.
Before running a retention sweep, operators must honor any legal or accounting
hold recorded in their system of record.
