ALTER TABLE match_disputes
    DROP COLUMN IF EXISTS evidence_deleted_at,
    DROP COLUMN IF EXISTS evidence_retention_until,
    DROP COLUMN IF EXISTS evidence_legal_hold;