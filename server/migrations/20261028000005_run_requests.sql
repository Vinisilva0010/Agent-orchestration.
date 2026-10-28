-- People's requests about runs: approval decisions, budget raises and
-- cancellations. The server records them after checking permissions; the
-- engine worker carries them out. The server never drives a run.

CREATE TABLE run_requests (
    id            BIGINT      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    run_id        TEXT        NOT NULL REFERENCES engine_runs (id),
    kind          TEXT        NOT NULL CHECK (kind IN ('decision', 'budget', 'cancel')),
    -- decision: {"approve": ["call id", ...]} (calls not listed are rejected)
    -- budget:   {"budget_micros": N}
    -- cancel:   {}
    payload       JSONB       NOT NULL,
    requested_by  BIGINT      NOT NULL REFERENCES users (id),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    processed_at  TIMESTAMPTZ,
    outcome       TEXT        CHECK (outcome IS NULL OR length(outcome) <= 500),
    CHECK ((processed_at IS NULL) = (outcome IS NULL))
);
-- At most one request per run waits for the engine.
CREATE UNIQUE INDEX run_requests_one_pending ON run_requests (run_id) WHERE processed_at IS NULL;
CREATE INDEX run_requests_pending ON run_requests (id) WHERE processed_at IS NULL;

GRANT SELECT, INSERT ON run_requests TO ao_server;
GRANT USAGE ON SEQUENCE run_requests_id_seq TO ao_server;
-- The server lists waiting runs; it cannot change them.
GRANT SELECT ON engine_runs TO ao_server;
-- The worker claims requests and records outcomes, nothing else.
GRANT SELECT ON run_requests TO ao_engine;
GRANT UPDATE (processed_at, outcome) ON run_requests TO ao_engine;

CREATE FUNCTION audit_run_requests() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public, pg_temp AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        PERFORM audit_append('run_request.created', NEW.run_id, jsonb_build_object(
            'request_id', NEW.id, 'kind', NEW.kind, 'payload', NEW.payload,
            'requested_by', NEW.requested_by));
    ELSE
        PERFORM audit_append('run_request.processed', NEW.run_id, jsonb_build_object(
            'request_id', NEW.id, 'kind', NEW.kind, 'outcome', NEW.outcome));
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER run_requests_audit
    AFTER INSERT OR UPDATE OF outcome ON run_requests
    FOR EACH ROW EXECUTE FUNCTION audit_run_requests();
