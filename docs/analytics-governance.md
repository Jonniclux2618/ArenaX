# Analytics contracts, quality, and retention

The Rust backend exposes the governed analytics boundary at
`/api/analytics/governance`. The migration seeds a contract and quality policy
for completed matches, but does not silently classify other legacy analytics
tables. Register each other dataset before publishing it.

The existing `POST /api/analytics/match` path uses the seeded
`match_completion` contract. Its governed payload contains only `game_id`,
`duration_secs`, and `player_count`; wager/reward totals stay in the existing
aggregate table and player or match identifiers are not included in the event.
Accepted events update game and platform aggregates transactionally, once per
match ID. A SHA-256 digest is retained as a minimal idempotency receipt so
retries cannot count the same match again; the raw quality event is retained
for 90 days under the policy below. The per-game response includes the latest
quality state (`unknown` for aggregates that predate quality checks).

## Dataset contracts and field approvals

An administrator registers or updates a contract with `PUT /datasets`:

```json
{
  "dataset_name": "match_summary",
  "owner": "Platform Analytics",
  "schema": {
    "$schema": "http://json-schema.org/draft-07/schema#",
    "type": "object",
    "additionalProperties": false,
    "properties": {
      "match_count": {
        "type": "integer",
        "minimum": 0,
        "x-data-classification": "internal"
      }
    },
    "required": ["match_count"]
  },
  "approved_purpose": "Platform reliability and product usage reporting",
  "access_tier": "internal",
  "retention_days": 90,
  "deidentification_rule": "Publish aggregate counts only; omit player and match identifiers."
}
```

Each declared field must carry an approved `x-data-classification` of `public`,
`internal`, or `confidential`. Restricted fields are not accepted into this
publication stream. Schemas must set `additionalProperties` to `false`; data
that violates the approved schema is quarantined without retaining its payload.
Changes to a contract replace its approval metadata and are attributed to the
admin making the change.

Grant access with `POST /access/grant`, revoke it with
`POST /access/revoke`, and review the grant history with `GET /access`.
Every grant has an admin owner and a review deadline no more than one year out;
expired or revoked grants cannot publish events. All contract, grant, threshold,
purge, and exception management endpoints require an admin or super-admin role.

## Data-quality checks

Administrators configure per-dataset, per-event thresholds with
`PUT /thresholds`. Thresholds define the rolling volume window and expected
minimum/maximum, maximum null rate, freshness age, numeric range constraints,
and escalation severity. An authenticated publisher must have a current access
grant to call `POST /events`.

The publisher validates the contract schema, repeated source event IDs,
null rate, configured ranges, volume, and event freshness before accepting an
event. Schema, duplicate, null-rate, or range failures are quarantined. Volume
and freshness failures are recorded as degraded; only healthy and degraded
events count toward volume checks. Quality results contain check names and
status, not the rejected payload. The configured severity is emitted as
structured logs: `critical` is an error-level escalation, `warning` a warning,
and `info` informational. Operators must route critical backend errors to the
on-call alert channel; lower severities do not page.

## Retention and purge evidence

Each registered dataset has a retention policy for the governed
`analytics_quality_events` stream, derived from its contract's owner and
retention period. The backend retries due policies hourly and purges at most
10,000 rows per run. Operators can preview and manually run a bounded purge
with `POST /retention/purge`:

```json
{
  "dataset_name": "match_summary",
  "dry_run": true,
  "batch_limit": 1000
}
```

`GET /retention/runs` returns status, examined/deleted counts, timestamps,
retryability, and watermarks; it never includes deleted content. Failed runs
are recorded for retry and logged. Retention exceptions are submitted with
`POST /retention/exceptions` and require a different administrator to approve
them through `/retention/exceptions/{id}/review`. Their object scope is limited
to a dataset and optionally an event type, and they expire within 30 days.

This job only governs records in `analytics_quality_events`; policies for other
legacy tables require an explicitly reviewed target and are not implied by
these APIs. The match aggregation receipt contains only the digest and game ID,
not the source identifier or event payload; it is retained while lifetime
aggregates require idempotency.
