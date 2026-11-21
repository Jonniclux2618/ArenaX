# Feature-Journey SLOs

This document defines the login, match, and payout end-to-end indicators. Metrics
include `feature_journey_requests_total{journey,outcome,component}` and
`feature_journey_duration_seconds{journey,component}`.

| Journey | Availability | Latency | Owner | Component contributions |
| --- | --- | --- | --- | --- |
| Login | 99.9% successful completed login sessions over 30 days | p95 < 1 second | Identity team | auth API, session store, Redis, database |
| Match | 99.5% successful match completion/reporting over 30 days | p95 < 5 seconds | Gameplay team | match API, PostgreSQL, matchmaking, WebSocket |
| Payout | 99.9% successful terminal payout outcomes over 30 days | p95 < 10 seconds | Finance team | payout API, ledger, provider, Stellar |

Synthetic probes are separate from real user outcomes and must carry the
`component` label only for traceable contribution accounting. Exclude known
maintenance windows, cancelled payouts, and synthetic health requests from
real-user availability. Provider timeouts and dependency noise remain component
failures for attribution, but alerting requires the multi-window burn rules below.

## Alerts

- Fast burn: >5% journey failure for 5 minutes, critical severity.
- Latency burn: p95 above the journey target for 10 minutes, warning severity.
- Each alert links to the service on-call runbook and the corresponding
  dashboard filter: `journey`, `component`, and `outcome`.

## Trace replay validation

Replay synthetic success/failure traces for login, match, and payout; confirm
availability and latency accounting, fast-burn alert evaluation, component
attribution, and runbook links before a journey rule reaches production.
