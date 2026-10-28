-- Audit log of security-relevant changes, written by the database itself.
--
-- Triggers on the audited tables append an event in the same transaction as
-- the change, so a change cannot happen without its event. Services cannot
-- skip or forge events: they have no privilege on audit_events or on
-- audit_append. Events are append-only (privileges and triggers) and chained:
-- each hash covers the previous hash and the event, so editing or removing an
-- event, even by an owner who disables the triggers, is found by
-- audit_verify(). Removing the newest events can only be detected against a
-- hash kept outside the database (Phase 9).
--
-- Events never contain secrets: no API keys, ciphertext, token hashes, tool
-- inputs or tool results.

CREATE TABLE audit_events (
    id           BIGINT      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    occurred_at  TIMESTAMPTZ NOT NULL,
    -- Database role that made the change. Services cannot choose it.
    db_role      TEXT        NOT NULL,
    -- Who the service says acted (setting ao.actor), or the database role.
    actor        TEXT        NOT NULL CHECK (length(actor) BETWEEN 1 AND 200),
    action       TEXT        NOT NULL CHECK (action ~ '^[a-z_]+[.][a-z_]+$'),
    target       TEXT        NOT NULL,
    details      JSONB       NOT NULL,
    prev_hash    BYTEA       CHECK (prev_hash IS NULL OR octet_length(prev_hash) = 32),
    hash         BYTEA       NOT NULL CHECK (octet_length(hash) = 32)
);

CREATE FUNCTION reject_audit_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'audit_events is append-only';
END;
$$;

CREATE TRIGGER audit_events_no_update_delete
    BEFORE UPDATE OR DELETE ON audit_events
    FOR EACH ROW EXECUTE FUNCTION reject_audit_change();

CREATE TRIGGER audit_events_no_truncate
    BEFORE TRUNCATE ON audit_events
    FOR EACH STATEMENT EXECUTE FUNCTION reject_audit_change();

-- The hashed form of an event. JSON encoding keeps fields unambiguous.
CREATE FUNCTION audit_digest(
    p_prev BYTEA, p_at TIMESTAMPTZ, p_role TEXT, p_actor TEXT,
    p_action TEXT, p_target TEXT, p_details JSONB
) RETURNS BYTEA
LANGUAGE sql STABLE SET search_path = pg_catalog, public, pg_temp AS $$
    SELECT sha256(convert_to(jsonb_build_array(
        encode(coalesce(p_prev, ''::bytea), 'hex'),
        to_char(p_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
        p_role, p_actor, p_action, p_target, p_details
    )::text, 'UTF8'))
$$;

-- Appends one chained event. Only the audit triggers call it.
CREATE FUNCTION audit_append(p_action TEXT, p_target TEXT, p_details JSONB) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public, pg_temp AS $$
DECLARE
    v_role  TEXT := CASE WHEN current_setting('role') = 'none'
                         THEN session_user ELSE current_setting('role') END;
    v_actor TEXT := left(coalesce(nullif(current_setting('ao.actor', true), ''), v_role), 200);
    v_at    TIMESTAMPTZ := clock_timestamp();
    v_prev  BYTEA;
BEGIN
    -- One writer at a time until commit, so the chain follows commit order.
    -- Audited transactions must use READ COMMITTED (the default).
    PERFORM pg_advisory_xact_lock(7291001);
    SELECT hash INTO v_prev FROM audit_events ORDER BY id DESC LIMIT 1;
    INSERT INTO audit_events (occurred_at, db_role, actor, action, target, details, prev_hash, hash)
    VALUES (v_at, v_role, v_actor, p_action, p_target, p_details, v_prev,
            audit_digest(v_prev, v_at, v_role, v_actor, p_action, p_target, p_details));
END;
$$;
REVOKE EXECUTE ON FUNCTION audit_append(TEXT, TEXT, JSONB) FROM PUBLIC;

-- The id of the first event that does not match the chain, or NULL.
CREATE FUNCTION audit_verify() RETURNS BIGINT
LANGUAGE plpgsql STABLE SET search_path = pg_catalog, public, pg_temp AS $$
DECLARE
    r      RECORD;
    v_prev BYTEA := NULL;
BEGIN
    FOR r IN SELECT * FROM audit_events ORDER BY id LOOP
        IF r.prev_hash IS DISTINCT FROM v_prev
           OR r.hash <> audit_digest(r.prev_hash, r.occurred_at, r.db_role, r.actor,
                                     r.action, r.target, r.details) THEN
            RETURN r.id;
        END IF;
        v_prev := r.hash;
    END LOOP;
    RETURN NULL;
END;
$$;

CREATE FUNCTION audit_provider_credentials() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public, pg_temp AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        PERFORM audit_append('credential.deleted', OLD.provider,
                             jsonb_build_object('key_version', OLD.key_version));
        RETURN OLD;
    END IF;
    PERFORM audit_append(CASE TG_OP WHEN 'INSERT' THEN 'credential.stored' ELSE 'credential.replaced' END,
                         NEW.provider, jsonb_build_object('key_version', NEW.key_version));
    RETURN NEW;
END;
$$;
CREATE TRIGGER provider_credentials_audit
    AFTER INSERT OR UPDATE OR DELETE ON provider_credentials
    FOR EACH ROW EXECUTE FUNCTION audit_provider_credentials();

CREATE FUNCTION audit_task_tokens() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public, pg_temp AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        PERFORM audit_append('task_token.issued', NEW.task_id,
                             jsonb_build_object('agent_id', NEW.agent_id, 'expires_at', NEW.expires_at));
    ELSIF OLD.revoked_at IS NULL AND NEW.revoked_at IS NOT NULL THEN
        PERFORM audit_append('task_token.revoked', NEW.task_id,
                             jsonb_build_object('agent_id', NEW.agent_id));
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER task_tokens_audit
    AFTER INSERT OR UPDATE OF revoked_at ON task_tokens
    FOR EACH ROW EXECUTE FUNCTION audit_task_tokens();

CREATE FUNCTION audit_agent_budgets() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public, pg_temp AS $$
BEGIN
    PERFORM audit_append('agent_budget.set', NEW.agent_id,
                         jsonb_build_object('monthly_limit_micros', NEW.monthly_limit_micros));
    RETURN NEW;
END;
$$;
CREATE TRIGGER agent_budgets_audit
    AFTER INSERT OR UPDATE OF monthly_limit_micros ON agent_budgets
    FOR EACH ROW EXECUTE FUNCTION audit_agent_budgets();

CREATE FUNCTION audit_task_budgets() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public, pg_temp AS $$
BEGIN
    PERFORM audit_append('task_budget.set', NEW.task_id,
                         jsonb_build_object('limit_micros', NEW.limit_micros));
    RETURN NEW;
END;
$$;
-- Reservations update spent and reserved amounts only, so they add no events.
CREATE TRIGGER task_budgets_audit
    AFTER INSERT OR UPDATE OF limit_micros ON task_budgets
    FOR EACH ROW EXECUTE FUNCTION audit_task_budgets();

CREATE FUNCTION audit_engine_runs() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public, pg_temp AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        PERFORM audit_append('run.created', NEW.id, jsonb_build_object(
            'agent_id', NEW.agent_id, 'tier', NEW.tier,
            'budget_micros', NEW.budget_micros, 'status', NEW.status));
        RETURN NEW;
    END IF;
    IF jsonb_typeof(NEW.state->'approved') = 'array'
       AND NEW.state->'approved' IS DISTINCT FROM OLD.state->'approved' THEN
        PERFORM audit_append('run.decision', NEW.id, jsonb_build_object(
            'approved', NEW.state->'approved',
            'pending', coalesce((SELECT jsonb_agg(call->'id')
                                 FROM jsonb_array_elements(coalesce(NEW.state->'pending', '[]'::jsonb)) AS call),
                                '[]'::jsonb)));
    END IF;
    IF NEW.state->>'tier' IS DISTINCT FROM OLD.state->>'tier' THEN
        PERFORM audit_append('run.tier_raised', NEW.id, jsonb_build_object(
            'from', coalesce(OLD.state->>'tier', OLD.tier), 'to', NEW.state->>'tier'));
    END IF;
    IF NEW.budget_micros <> OLD.budget_micros THEN
        PERFORM audit_append('run.budget_changed', NEW.id, jsonb_build_object(
            'from', OLD.budget_micros, 'to', NEW.budget_micros));
    END IF;
    IF NEW.status <> OLD.status THEN
        PERFORM audit_append('run.status', NEW.id, jsonb_build_object(
            'from', OLD.status, 'to', NEW.status));
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER engine_runs_audit
    AFTER INSERT OR UPDATE ON engine_runs
    FOR EACH ROW EXECUTE FUNCTION audit_engine_runs();

CREATE FUNCTION audit_engine_tool_executions() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public, pg_temp AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        PERFORM audit_append('tool.started', NEW.run_id,
                             jsonb_build_object('call_id', NEW.call_id, 'tool', NEW.tool));
    ELSIF OLD.state = 'started' AND NEW.state = 'finished' THEN
        PERFORM audit_append('tool.finished', NEW.run_id, jsonb_build_object(
            'call_id', NEW.call_id, 'tool', NEW.tool, 'is_error', NEW.is_error));
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER engine_tool_executions_audit
    AFTER INSERT OR UPDATE ON engine_tool_executions
    FOR EACH ROW EXECUTE FUNCTION audit_engine_tool_executions();
