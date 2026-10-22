ALTER TABLE match_disputes
    ADD COLUMN IF NOT EXISTS evidence_legal_hold BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS evidence_retention_until TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS evidence_deleted_at TIMESTAMPTZ;

UPDATE match_disputes
SET evidence_retention_until = resolved_at + INTERVAL '90 days'
WHERE resolved_at IS NOT NULL
  AND evidence_s3_key IS NOT NULL
  AND evidence_retention_until IS NULL;