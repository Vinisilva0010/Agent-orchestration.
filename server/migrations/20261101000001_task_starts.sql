-- Requests to start a task, recorded by the server after checking that the
-- person may use the agent. The engine worker claims each request once,
-- creates the run and notes its id here, in one transaction, and only then
-- runs the agent. A processed request never changes again.

CREATE TABLE task_starts (
    id             BIGINT      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    agent_id       TEXT        NOT NULL REFERENCES agents (id),
    task           TEXT        NOT NULL CHECK (length(task) BETWEEN 1 AND 8000),
    -- In millionths of a dollar (at most US$ 100, as for agents).
    budget_micros  BIGINT      NOT NULL CHECK (budget_micros BETWEEN 1 AND 100000000),
    requested_by   BIGINT      NOT NULL REFERENCES users (id),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    processed_at   TIMESTAMPTZ,
    run_id         TEXT        REFERENCES engine_runs (id),
    outcome        TEXT        CHECK (outcome IS NULL OR length(outcome) <= 500),
    CHECK ((processed_at IS NULL) = (outcome IS NULL)),
    CHECK (run_id IS NULL OR processed_at IS NOT NULL)
);
CREATE INDEX task_starts_pending ON task_starts (id) WHERE processed_at IS NULL;
CREATE INDEX task_starts_pending_by_person ON task_starts (requested_by) WHERE processed_at IS NULL;
CREATE INDEX task_starts_by_agent ON task_starts (agent_id, id DESC);
-- One request per run.
CREATE UNIQUE INDEX task_starts_run ON task_starts (run_id) WHERE run_id IS NOT NULL;
-- Lists an agent's latest runs.
CREATE INDEX engine_runs_by_agent ON engine_runs (agent_id, created_at DESC);

-- The server records requests; it can never mark one processed or point it
-- at a run.
GRANT SELECT ON task_starts TO ao_server;
GRANT INSERT (agent_id, task, budget_micros, requested_by) ON task_starts TO ao_server;
GRANT USAGE ON SEQUENCE task_starts_id_seq TO ao_server;
-- The worker claims requests and records what happened, nothing else.
GRANT SELECT ON task_starts TO ao_engine;
GRANT UPDATE (processed_at, run_id, outcome) ON task_starts TO ao_engine;

-- A processed request is final: it cannot be claimed again or moved to
-- another run.
CREATE FUNCTION task_starts_final() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, public, pg_temp AS $$
BEGIN
    IF OLD.processed_at IS NOT NULL THEN
        RAISE EXCEPTION 'task start % was already processed', OLD.id
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.agent_id <> OLD.agent_id OR NEW.task <> OLD.task
       OR NEW.budget_micros <> OLD.budget_micros OR NEW.requested_by <> OLD.requested_by
       OR NEW.created_at <> OLD.created_at THEN
        RAISE EXCEPTION 'task start % cannot be changed', OLD.id
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER task_starts_final
    BEFORE UPDATE ON task_starts
    FOR EACH ROW EXECUTE FUNCTION task_starts_final();

-- The task text is not copied into the audit log, which can never be edited:
-- people may paste something they later need removed. Its SHA-256 and length
-- identify it.
CREATE FUNCTION audit_task_starts() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public, pg_temp AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        PERFORM audit_append('task.requested', NEW.agent_id, jsonb_build_object(
            'task_start_id', NEW.id, 'requested_by', NEW.requested_by,
            'budget_micros', NEW.budget_micros, 'task_chars', length(NEW.task),
            'task_sha256', encode(sha256(convert_to(NEW.task, 'UTF8')), 'hex')));
    ELSIF NEW.run_id IS NOT NULL THEN
        PERFORM audit_append('task.started', NEW.agent_id, jsonb_build_object(
            'task_start_id', NEW.id, 'run_id', NEW.run_id));
    ELSE
        PERFORM audit_append('task.failed', NEW.agent_id, jsonb_build_object(
            'task_start_id', NEW.id, 'outcome', NEW.outcome));
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER task_starts_audit
    AFTER INSERT OR UPDATE OF outcome ON task_starts
    FOR EACH ROW EXECUTE FUNCTION audit_task_starts();
