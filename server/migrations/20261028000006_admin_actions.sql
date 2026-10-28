-- Who asked the gateway to change a key or a limit. The gateway's own audit
-- triggers record the change itself with its database role; this table adds
-- the person. The server can only insert. Keys are never stored here.

CREATE TABLE admin_actions (
    id            BIGINT      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind          TEXT        NOT NULL CHECK (kind IN ('credential_store', 'credential_delete', 'agent_budget_set')),
    target        TEXT        NOT NULL CHECK (length(target) BETWEEN 1 AND 128),
    details       JSONB       NOT NULL DEFAULT '{}'::jsonb,
    outcome       TEXT        NOT NULL CHECK (outcome IN ('ok', 'failed')),
    requested_by  BIGINT      NOT NULL REFERENCES users (id),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

GRANT INSERT ON admin_actions TO ao_server;
GRANT USAGE ON SEQUENCE admin_actions_id_seq TO ao_server;

CREATE FUNCTION audit_admin_actions() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public, pg_temp AS $$
BEGIN
    PERFORM audit_append('admin.' || NEW.kind, NEW.target,
                         NEW.details || jsonb_build_object('requested_by', NEW.requested_by,
                                                           'outcome', NEW.outcome));
    RETURN NEW;
END;
$$;
CREATE TRIGGER admin_actions_audit
    AFTER INSERT ON admin_actions
    FOR EACH ROW EXECUTE FUNCTION audit_admin_actions();
