-- Declare the audit event policy at the storage boundary. Policy changes
-- affect new rows only; existing rows are not reinterpreted.
CREATE TABLE audit_event_policies (
    resource_type TEXT PRIMARY KEY,
    required_fields TEXT[] NOT NULL DEFAULT ARRAY[]::TEXT[],
    pseudonymized_fields TEXT[] NOT NULL DEFAULT ARRAY[]::TEXT[],
    prohibited_fields TEXT[] NOT NULL,
    retention_class TEXT NOT NULL,
    retention_days INTEGER NOT NULL CHECK (retention_days > 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

INSERT INTO audit_event_policies (
    resource_type, required_fields, pseudonymized_fields, prohibited_fields,
    retention_class, retention_days
) VALUES
    ('users', ARRAY['id']::TEXT[], ARRAY[]::TEXT[],
     ARRAY['password', 'password_hash', 'secret', 'secret_key', 'private_key',
           'encrypted_secret_key', 'token', 'access_token', 'refresh_token',
           'authorization', 'cookie', 'request_body', 'raw_body', 'body',
           'email', 'email_address', 'phone', 'phone_number', 'ip_address',
           'user_agent']::TEXT[],
     'account-security-7y', 2555),
    ('api_keys', ARRAY['id']::TEXT[], ARRAY[]::TEXT[],
     ARRAY['key', 'api_key', 'api_key_hash', 'secret', 'client_secret', 'token',
           'authorization', 'cookie', 'request_body',
           'raw_body', 'body', 'ip_address', 'user_agent']::TEXT[],
     'credential-security-7y', 2555),
    ('transactions', ARRAY['id']::TEXT[], ARRAY[]::TEXT[],
     ARRAY['password', 'secret', 'secret_key', 'private_key', 'token',
           'authorization', 'cookie', 'request_body', 'raw_body', 'body',
           'email', 'email_address', 'phone', 'phone_number', 'ip_address',
           'user_agent']::TEXT[],
     'financial-7y', 2555),
    ('payments', ARRAY['id']::TEXT[], ARRAY[]::TEXT[],
     ARRAY['password', 'secret', 'secret_key', 'private_key', 'token',
           'authorization', 'cookie', 'request_body', 'raw_body', 'body',
           'email', 'email_address', 'phone', 'phone_number', 'ip_address',
           'user_agent']::TEXT[],
     'financial-7y', 2555),
    ('prize_distributions', ARRAY['id']::TEXT[], ARRAY[]::TEXT[],
     ARRAY['password', 'secret', 'secret_key', 'private_key', 'token',
           'authorization', 'cookie', 'request_body', 'raw_body', 'body',
           'email', 'email_address', 'phone', 'phone_number', 'ip_address',
           'user_agent']::TEXT[],
     'financial-7y', 2555),
    ('wallet_transactions', ARRAY['id']::TEXT[], ARRAY[]::TEXT[],
     ARRAY['password', 'secret', 'secret_key', 'private_key', 'token',
           'authorization', 'cookie', 'request_body', 'raw_body', 'body',
           'email', 'email_address', 'phone', 'phone_number', 'ip_address',
           'user_agent']::TEXT[],
     'financial-7y', 2555),
    ('reputation_scores', ARRAY['id']::TEXT[], ARRAY[]::TEXT[],
     ARRAY['password', 'secret', 'secret_key', 'private_key', 'token',
           'authorization', 'cookie', 'request_body', 'raw_body', 'body',
           'email', 'email_address', 'phone', 'phone_number', 'ip_address',
           'user_agent']::TEXT[],
     'gameplay-2y', 730),
    ('reputation_events', ARRAY['id']::TEXT[], ARRAY[]::TEXT[],
     ARRAY['password', 'secret', 'secret_key', 'private_key', 'token',
           'authorization', 'cookie', 'request_body', 'raw_body', 'body',
           'email', 'email_address', 'phone', 'phone_number', 'ip_address',
           'user_agent']::TEXT[],
     'gameplay-2y', 730),
    ('user_reputation', ARRAY['id']::TEXT[], ARRAY[]::TEXT[],
     ARRAY['password', 'secret', 'secret_key', 'private_key', 'token',
           'authorization', 'cookie', 'request_body', 'raw_body', 'body',
           'email', 'email_address', 'phone', 'phone_number', 'ip_address',
           'user_agent']::TEXT[],
     'gameplay-2y', 730),
    ('matches', ARRAY['id']::TEXT[], ARRAY[]::TEXT[],
     ARRAY['password', 'secret', 'secret_key', 'private_key', 'token',
           'authorization', 'cookie', 'request_body', 'raw_body', 'body',
           'email', 'email_address', 'phone', 'phone_number', 'ip_address',
           'user_agent']::TEXT[],
     'gameplay-2y', 730)
ON CONFLICT (resource_type) DO UPDATE SET
    required_fields = EXCLUDED.required_fields,
    pseudonymized_fields = EXCLUDED.pseudonymized_fields,
    prohibited_fields = EXCLUDED.prohibited_fields,
    retention_class = EXCLUDED.retention_class,
    retention_days = EXCLUDED.retention_days,
    updated_at = NOW();

CREATE OR REPLACE FUNCTION redact_audit_jsonb(payload JSONB, prohibited TEXT[])
RETURNS JSONB AS $$
DECLARE
    result JSONB;
BEGIN
    IF payload IS NULL THEN
        RETURN NULL;
    ELSIF jsonb_typeof(payload) = 'object' THEN
        SELECT COALESCE(
            jsonb_object_agg(entry.key, redact_audit_jsonb(entry.value, prohibited)),
            '{}'::JSONB
        )
        INTO result
        FROM jsonb_each(payload) AS entry
        WHERE NOT EXISTS (
            SELECT 1
            FROM unnest(prohibited) AS blocked(field)
            WHERE regexp_replace(LOWER(entry.key), '[-_]', '', 'g')
                = regexp_replace(LOWER(blocked.field), '[-_]', '', 'g')
        );
        RETURN result;
    ELSIF jsonb_typeof(payload) = 'array' THEN
        SELECT COALESCE(
            jsonb_agg(redact_audit_jsonb(entry.value, prohibited) ORDER BY entry.ordinality),
            '[]'::JSONB
        )
        INTO result
        FROM jsonb_array_elements(payload) WITH ORDINALITY AS entry(value, ordinality);
        RETURN result;
    END IF;
    RETURN payload;
END;
$$ LANGUAGE plpgsql IMMUTABLE;

CREATE OR REPLACE FUNCTION audit_row_change()
RETURNS TRIGGER AS $$
DECLARE
    actor UUID;
    old_json JSONB;
    new_json JSONB;
    resource UUID;
    required_fields TEXT[] := ARRAY['id']::TEXT[];
    prohibited_fields TEXT[] := ARRAY[
        'password', 'password_hash', 'secret', 'secret_key', 'private_key',
        'encrypted_secret_key', 'token', 'access_token', 'refresh_token',
        'authorization', 'cookie', 'request_body', 'raw_body', 'body',
        'email', 'phone_number', 'ip_address', 'user_agent'
    ]::TEXT[];
    retention_days INTEGER := 2555;
BEGIN
    old_json := CASE WHEN TG_OP = 'INSERT' THEN NULL ELSE to_jsonb(OLD) END;
    new_json := CASE WHEN TG_OP = 'DELETE' THEN NULL ELSE to_jsonb(NEW) END;

    SELECT policy.required_fields, policy.prohibited_fields, policy.retention_days
    INTO required_fields, prohibited_fields, retention_days
    FROM audit_event_policies AS policy
    WHERE policy.resource_type = TG_TABLE_NAME;

    IF NOT FOUND THEN
        required_fields := ARRAY['id']::TEXT[];
        prohibited_fields := ARRAY[
            'password', 'password_hash', 'secret', 'secret_key', 'private_key',
            'encrypted_secret_key', 'token', 'access_token', 'refresh_token',
            'authorization', 'cookie', 'request_body', 'raw_body', 'body',
            'email', 'email_address', 'phone', 'phone_number', 'ip_address',
            'user_agent', 'key', 'api_key', 'api_key_hash', 'client_secret'
        ]::TEXT[];
        retention_days := 2555;
    END IF;

    IF old_json IS NOT NULL THEN
        old_json := redact_audit_jsonb(old_json, prohibited_fields);
        IF NOT (old_json ?& required_fields) THEN
            RAISE EXCEPTION 'Audit policy for % requires fields %', TG_TABLE_NAME, required_fields;
        END IF;
    END IF;
    IF new_json IS NOT NULL THEN
        new_json := redact_audit_jsonb(new_json, prohibited_fields);
        IF NOT (new_json ?& required_fields) THEN
            RAISE EXCEPTION 'Audit policy for % requires fields %', TG_TABLE_NAME, required_fields;
        END IF;
    END IF;

    BEGIN
        actor := NULLIF(current_setting('audit.actor_id', TRUE), '')::UUID;
    EXCEPTION WHEN OTHERS THEN
        actor := NULL;
    END;

    IF actor IS NULL THEN
        actor := COALESCE(
            (new_json ->> 'user_id')::UUID,
            (old_json ->> 'user_id')::UUID
        );
    END IF;

    resource := COALESCE((new_json ->> 'id')::UUID, (old_json ->> 'id')::UUID);

    INSERT INTO audit_logs (
        user_id, action, resource_type, resource_id,
        old_values, new_values, source, created_at, retained_until
    ) VALUES (
        actor,
        LOWER(TG_OP),
        TG_TABLE_NAME,
        resource,
        old_json,
        new_json,
        'trigger',
        NOW(),
        NOW() + make_interval(days => retention_days)
    );

    RETURN COALESCE(NEW, OLD);
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION archive_audit_logs_past_retention(cutoff TIMESTAMPTZ)
RETURNS BIGINT AS $$
DECLARE
    moved BIGINT;
BEGIN
    INSERT INTO audit_logs_archive (
        id, sequence_number, user_id, action, resource_type, resource_id,
        details, old_values, new_values, ip_address, user_agent, source,
        entry_hash, previous_hash, created_at, retained_until
    )
    SELECT
        id, sequence_number, user_id, action, resource_type, resource_id,
        details, old_values, new_values, ip_address, user_agent, source,
        entry_hash, previous_hash, created_at, retained_until
    FROM audit_logs
    WHERE retained_until <= cutoff
    ON CONFLICT (id) DO NOTHING;

    GET DIAGNOSTICS moved = ROW_COUNT;
    RETURN moved;
END;
$$ LANGUAGE plpgsql;

COMMENT ON FUNCTION archive_audit_logs_past_retention(TIMESTAMPTZ) IS
    'Copies audit entries whose per-row retention deadline has elapsed into audit_logs_archive.';
