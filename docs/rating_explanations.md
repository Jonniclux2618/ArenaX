# Rating Change Explanations

This document describes how player-facing rating change explanations are produced
from finalized match evidence (issue #196).

## Objective

Players and support need a reproducible explanation for every rating delta without
exposing confidential anti-abuse signals or private moderation fields.

## Canonical service boundary

Explanations are generated at the rating service boundary, from **persisted inputs
only**:

- the finalized match record (immutable once finalized),
- the rating version / model version that was active when the delta was applied,
- the persisted contributing factors captured at calculation time.

Explanations are **not** recomputed from the current model. Because they are derived
from persisted inputs, an explanation remains stable after future model changes and
can be reproduced by support at any later date.

## What an explanation contains

Each rating delta links to:

1. the finalized match (match id and finalization timestamp),
2. the rating version used for the calculation,
3. a concise set of **player-safe** contributing factors.

Player-safe factors are limited to information the player can already observe or
reason about (for example: match outcome, opponent rating at the time of the match,
and the rating version applied).

## What is excluded

Player-facing explanations must never include:

- confidential anti-abuse signals (risk scores, flags, heuristics, thresholds),
- private moderation fields (moderator notes, reports, sanctions, internal review
  state),
- any field not explicitly marked player-safe.

These fields may exist in internal records but are filtered out before an
explanation is returned to players or support.

## Immutability and corrections

Finalized match and tournament records are immutable. Rating deltas and their
explanations are only changed through an explicit, audited correction flow; the
original explanation is retained for audit.

## Validation

- Explanation consistency is tested against recalculation from the same persisted
  inputs.
- Tests verify that private moderation and anti-abuse fields are excluded from
  player-facing output.
- Focused automated coverage exists for authorization, failure handling, and
  concurrency / replay behavior.
