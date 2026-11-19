# Runtime Secret Consumer Inventory

Startup rejects unregistered secret-shaped environment variable names. Errors
identify variable names only; values are never included in diagnostics. Values
matching known placeholders are rejected in production and warned about in
non-production environments.

| Variable(s) | Consumer | Purpose | Rotation owner | Startup behavior |
|---|---|---|---|---|
| `DATABASE_URL` | Rust backend and TypeScript server | Database connection | Platform team | Required; URL validated |
| `REDIS_URL` | Rust backend and TypeScript server | Session, rate-limit, queue, and one-time proof state | Platform team | Rust required; TypeScript optional except for step-up verification |
| `JWT_SECRET`, `JWT_REFRESH_SECRET` | Rust backend | Access and refresh token signing | Identity team | Access secret required and strong; refresh secret optional but validated when set |
| `JWT_SECRET`, `JWT_PRIVATE_KEY`, `JWT_PUBLIC_KEY`, `JWT_PRIVATE_KEY_FILE`, `JWT_PUBLIC_KEY_FILE` | TypeScript server | Access-token signing and verification | Identity team | HS256 secret or complete RS256 keypair required |
| `SENSITIVE_OPERATION_SECRET` | TypeScript server and Rust backend | HMAC signing and verification of action-bound step-up proofs | Identity team | Required and at least 32 characters |
| `S3_ACCESS_KEY`, `S3_SECRET_KEY` | Rust backend | Object-storage access | Platform team | Both required; secret must be strong |
| `PAYSTACK_SECRET`, `FLUTTERWAVE_SECRET`, `FLUTTERWAVE_WEBHOOK_HASH` | Rust backend | Payment API and webhook verification | Finance team | Active provider required; optional configured values validated |
| `PAYSTACK_SECRET_KEY`, `FLUTTERWAVE_SECRET_KEY`, `FLUTTERWAVE_WEBHOOK_HASH` | TypeScript server | Payment API and webhook verification | Finance team | Optional outside production; placeholders rejected |
| `STELLAR_ADMIN_SECRET`, `ENCRYPTION_KEY_V1` | Rust backend | Stellar signing and encryption-key rotation | Platform team | Stellar key required; encryption key optional and validated |
| `ADMIN_SECRET_KEY`, `FEE_PAYER_SECRET_KEY`, `ENCRYPTION_SECRET`, `WALLET_MASTER_KEYS` | TypeScript server | Stellar signing and wallet encryption | Platform team | Optional where feature is disabled; placeholders rejected |
| `SENDGRID_API_KEY`, `EMAIL_API_KEY` | Rust backend | Outbound email | Communications team | Optional; email delivery is unavailable when absent |
| `EMAIL_PASSWORD`, OAuth client secrets | TypeScript server | Email and external identity providers | Identity team | Optional; OAuth credentials must be configured as pairs |
| `CDN_API_TOKEN` | TypeScript server | CDN cache management | Platform team | Optional; required only when CDN management is enabled |
| `SLACK_WEBHOOK_URL`, `OTEL_EXPORTER_OTLP_HEADERS` | Rust backend | Operational alerts and authenticated telemetry export | Platform team | Optional; unset disables the corresponding integration |

Variables ending in `_PREVIOUS` are not currently consumed by either runtime;
setting one is treated as an unapproved secret until its rotation consumer and
overlap behavior are implemented. The commented examples in
`backend/.env.example` describe a future rotation procedure, not active support.
Any new secret consumer must be added to this inventory and its service
allowlist with a purpose, owner, and required/optional startup rule.

## Rotation and diagnostics

- Never put production values in committed environment examples.
- Generate local signing secrets with `openssl rand -hex 32`; use a secret
  manager for deployed environments.
- Rotate at the provider, deploy the replacement, verify the consumer, then
  retire the prior value. Never log the secret itself.
- Unknown names and placeholders fail startup in production. Non-production
  placeholder warnings include only the variable name.
