DROP INDEX IF EXISTS idx_score_report_attempts_idempotency;
ALTER TABLE score_report_attempts
    DROP COLUMN IF EXISTS idempotency_key,
    DROP COLUMN IF EXISTS request_hash;

DROP INDEX IF EXISTS idx_match_scores_match_player_unique;
ALTER TABLE match_scores DROP COLUMN IF EXISTS request_hash;