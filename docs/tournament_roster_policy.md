# Team Roster Lock Policy

This document defines when a team roster becomes immutable during a tournament and how the few allowed substitutions are recorded. It is the operator-facing companion to the roster lock enforcement at the canonical tournament service boundary.

## Lock boundary

A roster is **open** (editable) until the tournament reaches the boundary defined by its rules, then becomes **locked**. The boundary is explicit per tournament and is one of:

| Rule | Roster locks when |
| --- | --- |
| `registration_close` | Registration for the tournament closes. |
| `round_assignment` | The first round assignment (seeding snapshot) is published. |
| `match_start` | The team's first match starts. |

Once locked, the roster is immutable. The lock is evaluated at the service boundary, not in the UI, so every caller (API, operator tooling, batch jobs) observes the same rule.

## Allowed substitutions

After the lock boundary, the only permitted roster change is a **recorded substitution**. Each substitution:

- is recorded against a specific **team** and a specific **match**;
- replaces one participant identity with another without mutating the historical record of any prior match;
- is rejected if the target match is already **active** or **finalized**.

A substitution never rewrites the participant identity of a match that has started or finished. Historical roster rendering always reflects the roster as it stood for that match.

## Immutability of finalized records

Finalized match and tournament records are immutable. Corrections are only possible through the explicit, audited correction flow, which records who made the change, why, and when. No roster update path may bypass this flow.

## Validation expectations

Automated coverage should exercise:

- **Authorization** — only authorized operators may record a substitution.
- **Failure handling** — substitutions against active or finalized matches are rejected.
- **Concurrency / replay** — a substitution racing a match start resolves deterministically; replaying a substitution is idempotent and does not double-apply.
- **Historical rendering** — rosters for finalized matches render the identities that participated, unaffected by later substitutions.
