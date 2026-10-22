-- Keep one authoritative score report per participant and match. The row lock
-- in MatchService serializes writes; this index is the final database guard.
ALTER TABLE match_scores
    ADD COLUMN IF NOT EXISTS request_hash VARCHAR(64);

CREATE UNIQUE INDEX IF NOT EXISTS idx_match_scores_match_player_unique
    ON match_scores(match_id, player_id);

-- Preserve the original request fingerprint and idempotency key alongside the
-- actor already recorded in player_id.
ALTER TABLE score_report_attempts
    ADD COLUMN IF NOT EXISTS request_hash VARCHAR(64),
    ADD COLUMN IF NOT EXISTS idempotency_key VARCHAR(255);

CREATE INDEX IF NOT EXISTS idx_score_report_attempts_idempotency
    ON score_report_attempts(match_id, player_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;