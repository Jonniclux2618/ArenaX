CREATE TABLE account_recovery_codes (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    code_hash CHAR(64) NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL DEFAULT NOW() + INTERVAL '1 year',
    consumed_at TIMESTAMPTZ
);

CREATE INDEX idx_account_recovery_codes_user
    ON account_recovery_codes(user_id, expires_at)
    WHERE consumed_at IS NULL;

CREATE TABLE account_recovery_cases (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    recovery_code_id UUID NOT NULL REFERENCES account_recovery_codes(id) ON DELETE CASCADE,
    status VARCHAR(16) NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'completed', 'cancelled', 'expired', 'appealed')),
    requested_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    eligible_at TIMESTAMPTZ NOT NULL DEFAULT NOW() + INTERVAL '24 hours',
    expires_at TIMESTAMPTZ NOT NULL DEFAULT NOW() + INTERVAL '72 hours',
    completed_at TIMESTAMPTZ,
    notification_status VARCHAR(16) NOT NULL DEFAULT 'pending'
        CHECK (notification_status IN ('pending', 'sent', 'failed', 'unavailable'))
);

CREATE UNIQUE INDEX idx_account_recovery_one_pending_case
    ON account_recovery_cases(user_id)
    WHERE status = 'pending';

CREATE INDEX idx_account_recovery_case_expiry
    ON account_recovery_cases(status, expires_at);

CREATE TABLE account_recovery_case_audit (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    case_id UUID NOT NULL REFERENCES account_recovery_cases(id) ON DELETE CASCADE,
    action VARCHAR(32) NOT NULL,
    details JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE analytics_dataset_contracts (
    dataset_name VARCHAR(100) PRIMARY KEY,
    owner VARCHAR(200) NOT NULL,
    schema_json JSONB NOT NULL CHECK (jsonb_typeof(schema_json) = 'object'),
    approved_purpose TEXT NOT NULL,
    access_tier VARCHAR(16) NOT NULL
        CHECK (access_tier IN ('public', 'internal', 'confidential', 'restricted')),
    retention_days INTEGER NOT NULL CHECK (retention_days > 0),
    deidentification_rule TEXT NOT NULL,
    status VARCHAR(16) NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'suspended')),
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE analytics_dataset_access_grants (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    dataset_name VARCHAR(100) NOT NULL REFERENCES analytics_dataset_contracts(dataset_name),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    granted_by UUID NOT NULL REFERENCES users(id),
    granted_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    review_due_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    revoked_by UUID REFERENCES users(id),
    UNIQUE(dataset_name, user_id, granted_at)
);

CREATE INDEX idx_analytics_grants_active
    ON analytics_dataset_access_grants(dataset_name, user_id)
    WHERE revoked_at IS NULL;

CREATE TABLE analytics_quality_thresholds (
    dataset_name VARCHAR(100) NOT NULL REFERENCES analytics_dataset_contracts(dataset_name),
    event_type VARCHAR(100) NOT NULL,
    window_seconds INTEGER NOT NULL CHECK (window_seconds > 0),
    min_events INTEGER NOT NULL DEFAULT 0 CHECK (min_events >= 0),
    max_events INTEGER CHECK (max_events IS NULL OR max_events >= min_events),
    max_null_rate NUMERIC(5, 4) NOT NULL DEFAULT 1
        CHECK (max_null_rate >= 0 AND max_null_rate <= 1),
    max_age_seconds INTEGER NOT NULL CHECK (max_age_seconds > 0),
    range_constraints JSONB NOT NULL DEFAULT '{}'::jsonb
        CHECK (jsonb_typeof(range_constraints) = 'object'),
    severity VARCHAR(16) NOT NULL DEFAULT 'warning'
        CHECK (severity IN ('info', 'warning', 'critical')),
    PRIMARY KEY(dataset_name, event_type)
);

CREATE TABLE analytics_quality_events (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    source_event_id UUID NOT NULL,
    dataset_name VARCHAR(100) NOT NULL,
    event_type VARCHAR(100) NOT NULL,
    event_at TIMESTAMPTZ NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    payload JSONB NOT NULL,
    status VARCHAR(16) NOT NULL
        CHECK (status IN ('healthy', 'degraded', 'quarantined')),
    check_results JSONB NOT NULL DEFAULT '[]'::jsonb
);

CREATE INDEX idx_analytics_quality_events_window
    ON analytics_quality_events(dataset_name, event_type, received_at DESC)
    WHERE status <> 'quarantined';
CREATE INDEX idx_analytics_quality_events_retention
    ON analytics_quality_events(dataset_name, received_at, id);

ALTER TABLE analytics_game_metrics
    ADD COLUMN data_quality_status VARCHAR(16) NOT NULL DEFAULT 'unknown'
        CHECK (data_quality_status IN ('unknown', 'healthy', 'degraded'));

CREATE TABLE analytics_match_aggregation_receipts (
    match_id_hash CHAR(64) PRIMARY KEY,
    game_id INTEGER NOT NULL
);

CREATE TABLE analytics_retention_policies (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    dataset_name VARCHAR(100) NOT NULL UNIQUE
        REFERENCES analytics_dataset_contracts(dataset_name),
    owner VARCHAR(200) NOT NULL,
    target_table VARCHAR(64) NOT NULL
        CHECK (target_table = 'analytics_quality_events'),
    retention_days INTEGER NOT NULL CHECK (retention_days > 0),
    eligibility_rule TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    last_success_at TIMESTAMPTZ,
    last_successful_watermark TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE analytics_retention_runs (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    policy_id UUID NOT NULL REFERENCES analytics_retention_policies(id),
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    dry_run BOOLEAN NOT NULL,
    status VARCHAR(16) NOT NULL
        CHECK (status IN ('running', 'succeeded', 'failed', 'skipped')),
    examined_count BIGINT NOT NULL DEFAULT 0 CHECK (examined_count >= 0),
    deleted_count BIGINT NOT NULL DEFAULT 0 CHECK (deleted_count >= 0),
    watermark TIMESTAMPTZ,
    retryable BOOLEAN NOT NULL DEFAULT FALSE,
    error_code VARCHAR(64)
);

CREATE INDEX idx_analytics_retention_runs_policy
    ON analytics_retention_runs(policy_id, started_at DESC);

CREATE TABLE analytics_retention_exceptions (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    policy_id UUID NOT NULL REFERENCES analytics_retention_policies(id),
    scope JSONB NOT NULL CHECK (jsonb_typeof(scope) = 'object'),
    reason TEXT NOT NULL,
    starts_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL,
    requested_by UUID NOT NULL REFERENCES users(id),
    reviewed_by UUID REFERENCES users(id),
    reviewed_at TIMESTAMPTZ,
    approved BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CHECK (expires_at > starts_at),
    CHECK ((reviewed_by IS NULL) = (reviewed_at IS NULL))
);

CREATE INDEX idx_analytics_retention_exceptions_active
    ON analytics_retention_exceptions(policy_id, starts_at, expires_at)
    WHERE reviewed_at IS NOT NULL AND approved = TRUE;

COMMENT ON TABLE account_recovery_codes IS
    'Hashed, one-use account recovery proofs; plaintext codes are shown only at issuance.';
COMMENT ON TABLE account_recovery_cases IS
    'Time-bounded recovery requests with a 24-hour cooling period and notification evidence.';
COMMENT ON TABLE account_recovery_case_audit IS
    'Recovery case audit events; never stores recovery codes or credentials.';
COMMENT ON TABLE analytics_dataset_contracts IS
    'Approved analytics purpose, owner, schema, tier, retention, and de-identification metadata.';
COMMENT ON TABLE analytics_dataset_access_grants IS
    'Reviewable, revocable per-user access to contracted analytics datasets.';
COMMENT ON TABLE analytics_quality_events IS
    'Validated analytical events with quality results; failed events are quarantined.';
COMMENT ON TABLE analytics_match_aggregation_receipts IS
    'Irreversible SHA-256 match-ID digests used only to prevent duplicate lifetime aggregation.';
COMMENT ON TABLE analytics_retention_runs IS
    'Per-dataset dry-run and purge evidence; contains counts and watermarks, not deleted payloads.';
COMMENT ON TABLE analytics_retention_exceptions IS
    'Reviewed, scoped, time-bounded holds on analytics retention purges.';

INSERT INTO analytics_dataset_contracts
    (dataset_name, owner, schema_json, approved_purpose, access_tier,
     retention_days, deidentification_rule)
VALUES (
    'match_completion',
    'Gameplay Analytics',
    '{
      "$schema":"http://json-schema.org/draft-07/schema#",
      "type":"object",
      "additionalProperties":false,
      "properties":{
        "game_id":{"type":"integer","minimum":1,"x-data-classification":"internal"},
        "duration_secs":{"type":"integer","minimum":1,"maximum":86400,"x-data-classification":"internal"},
        "player_count":{"type":"integer","minimum":1,"maximum":100,"x-data-classification":"internal"}
      },
      "required":["game_id","duration_secs","player_count"]
    }'::jsonb,
    'Quality-controlled per-game match aggregation',
    'internal',
    90,
    'Do not publish player IDs, match IDs, wagers, or rewards in the event payload.'
)
ON CONFLICT (dataset_name) DO NOTHING;

INSERT INTO analytics_retention_policies
    (dataset_name, owner, target_table, retention_days, eligibility_rule)
VALUES (
    'match_completion',
    'Gameplay Analytics',
    'analytics_quality_events',
    90,
    'received_at <= now() - 90 days; active reviewed exceptions are excluded'
)
ON CONFLICT (dataset_name) DO NOTHING;

INSERT INTO analytics_quality_thresholds
    (dataset_name, event_type, window_seconds, min_events, max_events,
     max_null_rate, max_age_seconds, range_constraints, severity)
VALUES (
    'match_completion',
    'match_completed',
    60,
    0,
    1000,
    0,
    300,
    '{"game_id":{"min":1},"duration_secs":{"min":1,"max":86400},"player_count":{"min":1,"max":100}}'::jsonb,
    'warning'
)
ON CONFLICT (dataset_name, event_type) DO NOTHING;
