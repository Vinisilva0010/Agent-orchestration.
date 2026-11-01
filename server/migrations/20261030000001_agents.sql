-- The agents people work with. The id is the same text the engine runs, the
-- gateway budgets and agent_permissions already use. Agents are archived,
-- never deleted, so runs and the audit log keep pointing at real agents.
CREATE TABLE agents (
    id                  TEXT        PRIMARY KEY CHECK (id ~ '^[A-Za-z0-9_-]{1,128}$'),
    display_name        TEXT        NOT NULL CHECK (length(display_name) BETWEEN 1 AND 100),
    description         TEXT        NOT NULL DEFAULT '' CHECK (length(description) <= 500),
    tier                TEXT        NOT NULL DEFAULT 'standard'
                                    CHECK (tier IN ('small', 'standard', 'large')),
    -- Default budget of one task, in millionths of a dollar (at most US$ 100).
    task_budget_micros  BIGINT      NOT NULL CHECK (task_budget_micros BETWEEN 1 AND 100000000),
    created_by          BIGINT      NOT NULL REFERENCES users (id),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    archived_at         TIMESTAMPTZ
);

-- The id and the creator never change, so the server may update only these.
GRANT SELECT, INSERT ON agents TO ao_server;
GRANT UPDATE (display_name, description, tier, task_budget_micros, updated_at, archived_at)
    ON agents TO ao_server;
GRANT SELECT ON agents TO ao_engine;
-- The server shows spend and limits; only the gateway writes them.
GRANT SELECT ON monthly_spend, agent_budgets TO ao_server;

CREATE FUNCTION audit_agents() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public, pg_temp AS $$
DECLARE
    changes jsonb := '{}'::jsonb;
BEGIN
    IF TG_OP = 'INSERT' THEN
        PERFORM audit_append('agent.created', NEW.id,
                             jsonb_build_object('display_name', NEW.display_name,
                                                'tier', NEW.tier,
                                                'task_budget_micros', NEW.task_budget_micros));
        RETURN NEW;
    END IF;
    IF NEW.display_name <> OLD.display_name THEN
        changes := changes || jsonb_build_object('display_name',
                   jsonb_build_object('from', OLD.display_name, 'to', NEW.display_name));
    END IF;
    IF NEW.description <> OLD.description THEN
        changes := changes || jsonb_build_object('description',
                   jsonb_build_object('from', OLD.description, 'to', NEW.description));
    END IF;
    IF NEW.tier <> OLD.tier THEN
        changes := changes || jsonb_build_object('tier',
                   jsonb_build_object('from', OLD.tier, 'to', NEW.tier));
    END IF;
    IF NEW.task_budget_micros <> OLD.task_budget_micros THEN
        changes := changes || jsonb_build_object('task_budget_micros',
                   jsonb_build_object('from', OLD.task_budget_micros, 'to', NEW.task_budget_micros));
    END IF;
    IF changes <> '{}'::jsonb THEN
        PERFORM audit_append('agent.changed', NEW.id, changes);
    END IF;
    IF OLD.archived_at IS NULL AND NEW.archived_at IS NOT NULL THEN
        PERFORM audit_append('agent.archived', NEW.id, '{}'::jsonb);
    ELSIF OLD.archived_at IS NOT NULL AND NEW.archived_at IS NULL THEN
        PERFORM audit_append('agent.restored', NEW.id, '{}'::jsonb);
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER agents_audit
    AFTER INSERT OR UPDATE ON agents
    FOR EACH ROW EXECUTE FUNCTION audit_agents();
