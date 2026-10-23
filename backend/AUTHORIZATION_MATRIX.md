# Backend Authorization Matrix

All routes are protected by `AuthMiddleware` unless listed as public below. Protected routes require a valid access token; refresh tokens are not accepted as bearer credentials. Authorization failures are denied by default.

| Resource / operation | Player | Moderator | Admin / super-admin | Public |
| --- | --- | --- | --- | --- |
| Health and CSRF token retrieval | No | No | No | Read |
| Authentication registration, login, refresh | No | No | No | Write |
| API documentation | No | No | No | Read |
| Tournament and leaderboard reads | No | No | No | Read |
| Tournament participation, wallet, profile, notifications, matchmaking, staking | Own account/resources only | Own account/resources only | All where the handler permits | No |
| Disputes and player suspensions | Own dispute/appeal only | Moderate / review | Moderate / review | No |
| IP lists, feature flags, cache controls, idempotency administration, bad-actor management, auth analytics | No | No | Read/write | No |
| Payment provider callbacks | No | No | No | Write with provider signature verification |
| Metrics scrape | No | No | No | Read |
| WebSocket user and match channels | Own user channel; participant match channels | As participant or moderator | All authorized channels | No |

## Enforcement

- `AuthMiddleware` is mounted at the application layer, so it covers both `/api`-scoped handlers and modules mounted at the app level.
- The middleware's public-route allowlist is method-and-path based. Any route not explicitly listed requires authentication.
- Admin-only and staff-only route families are checked centrally in `required_roles`; handler-level ownership checks remain required for user-owned records and resource-specific decisions.
- Session listing and revocation are user-scoped. API-key authentication must use `authenticate_key_for_scope`, which rejects missing scopes and atomically enforces expiration and usage limits.
- Public webhook handlers must validate their provider signature before changing state.

When adding a route, update the matrix and public allowlist together. Do not infer ownership from request-supplied user IDs; derive the caller from claims and scope database queries to that identity.
