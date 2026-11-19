CREATE OR REPLACE FUNCTION audit_row_change()
RETURNS TRIGGER AS $$
DECLARE
    actor UUID;
    old_json JSONB;
    new_json JSONB;
    resource UUID;
BEGIN
    old_json := CASE WHEN TG_OP = 'INSERT' THEN NULL ELSE to_jsonb(OLD) END;
    new_json := CASE WHEN TG_OP = 'DELETE' THEN NULL ELSE to_jsonb(NEW) END;

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
        old_values, new_values, source, created_at
    ) VALUES (
        actor,
        LOWER(TG_OP),
        TG_TABLE_NAME,
        resource,
        old_json,
        new_json,
        'trigger',
        NOW()
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
    WHERE created_at < cutoff
    ON CONFLICT (id) DO NOTHING;

    GET DIAGNOSTICS moved = ROW_COUNT;
    RETURN moved;
END;
$$ LANGUAGE plpgsql;

COMMENT ON FUNCTION archive_audit_logs_past_retention(TIMESTAMPTZ) IS
    'Copies audit entries older than the cutoff into audit_logs_archive. Idempotent; never deletes.';

DROP FUNCTION redact_audit_jsonb(JSONB, TEXT[]);
DROP TABLE audit_event_policies;
