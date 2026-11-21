# Event Governance

This document defines the operator-facing rules for scheduling tournaments and
other events. It is the canonical reference for how start times, deadlines, and
timezones are handled.

## Timezone and DST semantics

Tournament scheduling separates the **event instant** from the **organizer's
display preference**. Both are stored independently.

### Creation requirements

- Every tournament creation request MUST include a valid IANA timezone
  identifier (e.g. `America/New_York`, `Europe/Berlin`, `UTC`).
- Invalid or unknown identifiers are rejected at the service boundary; the
  tournament is not created.
- The organizer's local start time is interpreted in the supplied timezone.

### Ambiguous and nonexistent local times

Daylight-saving transitions make some local times ambiguous (fall-back, the
clock repeats an hour) or nonexistent (spring-forward, the clock skips an
hour). The service resolves these with an explicit, user-visible rule:

- **Spring-forward (nonexistent local time):** the requested local time does
  not exist. It is shifted forward by the size of the gap (typically one hour)
  to the first valid instant after the transition. The response reports the
  resolved instant and the applied rule so the organizer can confirm it.
- **Fall-back (ambiguous local time):** the requested local time occurs twice.
  The **earlier** (first) occurrence — the pre-transition offset — is chosen.
  The response reports the resolved instant, the chosen offset, and the applied
  rule so the organizer can confirm it.

These rules are deterministic and surfaced in the API response; organizers are
never left with a silently shifted or ambiguous start time.

### Execution deadlines vs. display

- All execution deadlines and job schedules are persisted and executed as
  **UTC instants**. Tournament jobs never depend on a local wall-clock time.
- Displays (API serialization, notifications, operator views) preserve the
  configured timezone and the resolved UTC offset at the event instant, so the
  organizer sees the intended local time.

### Immutability and corrections

Finalized match and tournament records are immutable. Start times, timezones,
and deadlines may only be changed through an explicit, audited correction flow
that records the previous and new values along with the acting operator.

## Validation expectations

Changes to scheduling behavior must be covered by focused automated tests for:

- DST spring-forward and fall-back dates across API serialization,
  notifications, and tournament jobs.
- Authorization of the correction flow.
- Failure handling for invalid timezone identifiers.
- Concurrency and replay behavior of scheduled jobs.
