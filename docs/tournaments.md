# Tournaments

This document describes how tournaments are registered and operated. For format-specific rules (Swiss, round-robin, single/double elimination) see [tournament-formats.md](./tournament-formats.md). For waitlist behavior see [tournament_waitlist.md](./tournament_waitlist.md).

## Individual registration

Individual tournaments register each participant independently. Registration is handled at the canonical service boundary (`backend/src/services/tournament_registration.rs`); API handlers and operators must not bypass it.

## Team registration

Team tournament formats register a **team** rather than a set of unrelated individuals. Team registration is performed at the same canonical service boundary and captures, at registration time:

- **Eligible roster snapshot** — the exact set of players registered for the team, frozen at registration. Later roster edits do not mutate this snapshot.
- **Captain** — a single designated captain drawn from the roster. The captain is the accountable contact for the team.
- **Substitutions policy** — whether and how substitutes may be used for this team (e.g. none, named substitutes only, or open substitution under the audited rule below).
- **Rules version** — the version of the tournament rules the team registered under. This is recorded so that later rule changes do not retroactively alter a team's obligations.

### Roster size limits

Roster size limits (minimum and maximum) are enforced at registration time. A registration that does not satisfy the format's roster bounds is rejected; it is never silently truncated or padded.

### Roster lock and substitutions

Once registration closes the roster is **locked**. After lock, roster changes are only permitted through the explicit audited substitution rule:

- A substitution replaces a rostered player with an eligible substitute and is recorded as an audited event (who, when, reason, and the affected roster entry).
- Substitutions never rewrite already-played rounds or finalized match records. Finalized matches and tournament records are immutable except through an explicit audited correction flow.
- Captain departure is handled as a substitution of the captain role under the same audited rule; the team must designate a new captain from the locked roster (or an eligible substitute) and the change is recorded.
- Last-minute replacements follow the same audited substitution rule and are subject to the format's roster bounds.

### Withdrawal

A team withdrawal is settled at the service boundary: the team's remaining fixtures are resolved according to the format's withdrawal settlement rules, and already-played rounds are left untouched.

## Operator notes

- Always register teams through the canonical service boundary so roster snapshots, captain, substitutions policy, and rules version are captured consistently.
- Do not edit finalized match or tournament records directly; use the audited correction flow.
- When in doubt about a roster change after lock, prefer the audited substitution rule over ad-hoc edits.
