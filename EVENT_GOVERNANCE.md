# Event Governance

This document describes how contract events flow through the indexer, how
decoding failures are handled, and the operator procedures for diagnosing and
replaying quarantined events.

## Event ingestion pipeline

1. **Fetch** — the Soroban indexer pulls contract events from the RPC in ledger
   order, starting from the last committed checkpoint.
2. **Decode** — each event payload (topic + value XDR) is decoded by the
   versioned decoder. The decoder version is recorded alongside every decoded
   event.
3. **Apply** — decoded events are applied to the projection and the checkpoint
   advances only after the ledger's events are durably committed.

## Quarantine policy

A new event shape or a malformed payload must never silently halt indexing and
must never be dropped without trace. When an event cannot be decoded:

- The raw payload is retained in a **bounded quarantine** (oldest entries are
  evicted once the bound is reached).
- Each quarantine record carries **source coordinates**: ledger sequence,
  transaction hash, event index, and contract id.
- Each record carries the **decoder version** that failed to decode it, so
  records can be re-evaluated after a decoder upgrade.
- Sensitive payload fields are **redacted** before the record is persisted or
  surfaced in operator diagnostics.

### Processing continues

When policy permits, a decode failure is quarantined and indexing **continues**
with the next event. Known-good later events in the same ledger (and subsequent
ledgers) are processed normally. The checkpoint still advances; the quarantined
event is not applied to the projection until it is explicitly replayed.

Policy may be configured to halt instead of continue for deployments that
require strict all-or-nothing application. The default is to quarantine and
continue.

## Operator diagnostics

Operators can inspect the quarantine through the diagnostics surface, which
reports for each record:

- source coordinates (ledger, tx hash, event index, contract id)
- decoder version
- a redacted view of the payload
- the decode error

Redaction is applied consistently in both the stored record and the diagnostic
output; sensitive fields are never exposed in logs or API responses.

## Replay after a decoder upgrade

Replay is **explicit and audited**. It is never triggered automatically by a
decoder upgrade.

1. Deploy the upgraded decoder.
2. An operator selects the quarantined records to replay (typically those whose
   recorded decoder version is older than the current one).
3. The replay is recorded in the **audit trail**: who triggered it, when, which
   records, and the decoder version used.
4. Successfully decoded events are applied; events that still fail remain in
   quarantine with an updated decoder version.

Duplicate delivery is handled idempotently: replaying an event that was already
applied is a no-op, and retries of a failed replay do not create duplicate
projection entries.

## Testing expectations

Focused automated tests cover:

- unknown topics
- invalid XDR payloads
- replay after a decoder update
- redaction of sensitive payload fields
- retry, failure, and duplicate-delivery cases
