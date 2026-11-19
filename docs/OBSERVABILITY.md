# Observability & Telemetry

This document defines how service health is instrumented across the TypeScript
gateway, the Rust backend, PostgreSQL, Redis, payment providers, and Stellar RPC.
It standardizes trace propagation, resource attributes, dependency spans,
redaction, and the sampling/cardinality limits that keep telemetry cost bounded.

## 1. Trace propagation (end-to-end correlation)

- The gateway is the trace root for inbound requests. It accepts an inbound W3C
  `traceparent`/`tracestate` when present, otherwise starts a new trace.
- The gateway MUST forward `traceparent` and `tracestate` on every outbound call
to the Rust backend (HTTP header) and to downstream dependencies.
- The Rust backend MUST extract `traceparent`/`tracestate` from inbound requests
  and continue the same trace (no new root).
- Every span carries the propagated `trace_id` so a single authenticated
tournament or payment request can be followed gateway -> backend -> dependency.

Correlation is by `trace_id` only. Never place credentials, auth headers, payment
secrets, or evidence contents in span attributes, logs, or Sentry/Datadog
payloads (see section 4).

## 2. Resource / service attributes

Both gateway and backend MUST attach the same resource attribute set:

| Attribute                 | Source                                             |
|---------------------------|----------------------------------------------------|
| `service.name`            | `gateway` / `backend`                              |
| `service.version`         | build/commit version injected at deploy            |
| `deployment.environment`  | `staging` / `production`                           |
| `service.instance.id`     | stable per-process/pod identifier                  |

These attributes are set once on the OpenTelemetry `Resource` and are inherited
by every span, metric, and log record exported through OTel, Sentry, and Datadog.

## 3. Dependency spans

Dependencies are instrumented as client spans, separate from the inbound
request span, so latency and failure are measurable per dependency:

- **PostgreSQL** - one span per query/transaction with `db.system=postgresql`,
  `db.operation`, and `db.statement` (parameterized only, never with values).
- **Redis** - one span per command with `db.system=redis` and `db.operation`.
  Queue depth is exported as a gauge metric (`queue.depth`) rather than a span
  attribute so it is separately measurable.
- **Payment providers** - one span per provider call with `peer.service` set to
  the provider name; record latency and outcome.
- **Stellar RPC** - one span per RPC call with `peer.service=stellar-rpc` and the
  RPC method name.

Each dependency span records:

- latency (span duration),
- failure status via span status (`ERROR`) and an `error.type` attribute,
- retry count where applicable.

Dependency latency/failure and queue depth are therefore measurable separately
from request-level latency.

## 4. Redaction

Redaction is applied at the exporter boundary so it covers OTel spans, logs, and
Sentry/Datadog payloads uniformly. The following MUST be redacted or dropped:

- `authorization`, `cookie`, `set-cookie`, and any auth/bearer headers,
- API keys, payment provider secrets, and signing keys,
- payment card data and provider tokens,
- evidence contents and any user-submitted payload bodies.

Rules:

- Never log or attach raw request/response bodies for authenticated or payment
  routes; log identifiers only.
- SQL spans use parameterized statements with placeholders, never bound values.
- Sentry `beforeSend` and Datadog scrubbing rules strip the fields above before
  export.

## 5. Sampling & cardinality limits

To prevent runaway telemetry cost:

- **Head sampling:** default 10% of traces in production, 100% in staging.
- **Tail sampling:** always keep traces with `ERROR` status or latency above the
  SLO threshold, regardless of head sampling.
- **Cardinality:** do not use unbounded values (user IDs, trace IDs, raw URLs,
  free-form error messages) as metric labels. Metric labels are limited to the
  resource attributes in section 2 plus a small fixed set of low-cardinality
  dimensions (dependency name, operation, status class).
- **Queue depth** is a gauge, not a per-event counter, to keep series bounded.

## 6. Validation

Run a staging trace through an authenticated tournament or payment request and
confirm:

1. A single `trace_id` spans gateway -> backend -> dependency spans.
2. Resource attributes are present on all spans.
3. Dependency spans show latency and failure status separately from the request
   span, and `queue.depth` is exported.
4. No credentials, auth headers, payment secrets, or evidence contents appear in
   spans, logs, or Sentry/Datadog payloads.

## 7. Critical user-journey SLOs

The Rust backend exposes route-template request counters and duration
histograms. `server/infra/monitoring/backend-alert.rules.yml` uses those
metrics for these initial service-level objectives:

| Journey | Availability objective | Latency objective | Included route templates | Owner |
|---|---|---|---|---|
| Login | 99.9% successful service responses over 30 days | p95 below 1s | `POST /api/auth/login` | Identity |
| Match | 99.9% successful service responses over 30 days | p95 below 2s | `POST /api/matches/{id}/start`, `POST /api/matches/{id}/complete` | Gameplay |
| Payout | 99.9% successful service responses and settlements over 30 days | p95 below 5s | `POST /api/tournaments/{id}/distribute-prizes` plus payout settlement attempts | Payments |

Availability counts HTTP 429 and 5xx responses as bad events; other 4xx
responses (including invalid user credentials and invalid requests) are
excluded because they indicate a completed service response. Requests are
counted only when traffic exists. Latency is measured at the HTTP route
boundary and for payout settlement from the settlement attempt to database
commit. The payout SLI includes both the initiation route and the asynchronous
settlement attempt, using bounded outcome labels and no player or tournament IDs.
The current Prometheus alerts page on a fast error-budget burn (14.4x over
both 5m and 1h) and warn when route p95 exceeds its threshold for 10m.

The journey SLI is the user-impact signal. Existing database, provider,
match-authority, and transaction metrics are component diagnostics used to
attribute failures; they must not be added as independent journey failures
unless the user-facing operation also fails. Route templates and settlement
outcomes are bounded labels; user, match, tournament, and payment identifiers
are not included.
