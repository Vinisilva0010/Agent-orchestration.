-- People who use the workspace, their sessions and their access to agents.
-- The server connects as ao_server. Accounts are disabled, never deleted, so
-- the audit log keeps pointing at real accounts.

DO $$
BEGIN
    CREATE ROLE ao_server NOLOGIN;
EXCEPTION WHEN duplicate_object OR unique_violation THEN
    NULL;
END
$$;

CREATE TABLE users (
    id                   BIGINT      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    email                TEXT        NOT NULL CHECK (
                             email = lower(email) AND length(email) <= 254
                             AND email ~ '^[^@[:space:]]+@[^@[:space:]]+[.][^@[:space:]]+$'),
    display_name         TEXT        NOT NULL CHECK (length(display_name) BETWEEN 1 AND 100),
    role                 TEXT        NOT NULL CHECK (role IN ('admin', 'member', 'viewer')),
    password_hash        TEXT        NOT NULL CHECK (password_hash LIKE '$argon2id$%'),
    failed_logins        INTEGER     NOT NULL DEFAULT 0 CHECK (failed_logins >= 0),
    locked_until         TIMESTAMPTZ,
    disabled_at          TIMESTAMPTZ,
    password_changed_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX users_email_idx ON users (email);

-- Only the SHA-256 of a session token is stored.
CREATE TABLE sessions (
    token_hash    BYTEA       PRIMARY KEY CHECK (octet_length(token_hash) = 32),
    user_id       BIGINT      NOT NULL REFERENCES users (id),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at    TIMESTAMPTZ NOT NULL,
    revoked_at    TIMESTAMPTZ
);
CREATE INDEX sessions_user_id_idx ON sessions (user_id);
CREATE INDEX sessions_expires_at_idx ON sessions (expires_at);

-- 'use' runs an agent and answers its approvals; 'manage' also changes it.
CREATE TABLE agent_permissions (
    user_id     BIGINT      NOT NULL REFERENCES users (id),
    agent_id    TEXT        NOT NULL CHECK (agent_id ~ '^[A-Za-z0-9_-]{1,128}$'),
    level       TEXT        NOT NULL CHECK (level IN ('use', 'manage')),
    granted_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, agent_id)
);

GRANT SELECT, INSERT, UPDATE ON users, sessions TO ao_server;
GRANT SELECT, INSERT, UPDATE, DELETE ON agent_permissions TO ao_server;
GRANT USAGE ON SEQUENCE users_id_seq TO ao_server;
GRANT SELECT ON audit_events TO ao_server;

CREATE FUNCTION audit_users() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public, pg_temp AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        PERFORM audit_append('user.created', NEW.id::text,
                             jsonb_build_object('email', NEW.email, 'role', NEW.role));
        RETURN NEW;
    END IF;
    IF NEW.email <> OLD.email THEN
        PERFORM audit_append('user.email_changed', NEW.id::text,
                             jsonb_build_object('from', OLD.email, 'to', NEW.email));
    END IF;
    IF NEW.role <> OLD.role THEN
        PERFORM audit_append('user.role_changed', NEW.id::text,
                             jsonb_build_object('from', OLD.role, 'to', NEW.role));
    END IF;
    IF OLD.disabled_at IS NULL AND NEW.disabled_at IS NOT NULL THEN
        PERFORM audit_append('user.disabled', NEW.id::text, '{}'::jsonb);
    ELSIF OLD.disabled_at IS NOT NULL AND NEW.disabled_at IS NULL THEN
        PERFORM audit_append('user.enabled', NEW.id::text, '{}'::jsonb);
    END IF;
    IF NEW.password_hash <> OLD.password_hash THEN
        PERFORM audit_append('user.password_changed', NEW.id::text, '{}'::jsonb);
    END IF;
    IF NEW.failed_logins > OLD.failed_logins THEN
        PERFORM audit_append('user.login_failed', NEW.id::text,
                             jsonb_build_object('failed_logins', NEW.failed_logins));
    END IF;
    IF NEW.locked_until IS NOT NULL AND NEW.locked_until IS DISTINCT FROM OLD.locked_until THEN
        PERFORM audit_append('user.locked', NEW.id::text,
                             jsonb_build_object('until', NEW.locked_until));
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER users_audit
    AFTER INSERT OR UPDATE OF email, role, disabled_at, password_hash, failed_logins, locked_until
    ON users FOR EACH ROW EXECUTE FUNCTION audit_users();

CREATE FUNCTION audit_sessions() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public, pg_temp AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        PERFORM audit_append('session.created', NEW.user_id::text,
                             jsonb_build_object('expires_at', NEW.expires_at));
    ELSIF OLD.revoked_at IS NULL AND NEW.revoked_at IS NOT NULL THEN
        PERFORM audit_append('session.revoked', NEW.user_id::text, '{}'::jsonb);
    END IF;
    RETURN NEW;
END;
$$;
-- last_seen_at updates add no events.
CREATE TRIGGER sessions_audit
    AFTER INSERT OR UPDATE OF revoked_at ON sessions
    FOR EACH ROW EXECUTE FUNCTION audit_sessions();

CREATE FUNCTION audit_agent_permissions() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public, pg_temp AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        PERFORM audit_append('agent_permission.revoked', OLD.agent_id,
                             jsonb_build_object('user_id', OLD.user_id));
        RETURN OLD;
    END IF;
    PERFORM audit_append('agent_permission.granted', NEW.agent_id,
                         jsonb_build_object('user_id', NEW.user_id, 'level', NEW.level));
    RETURN NEW;
END;
$$;
CREATE TRIGGER agent_permissions_audit
    AFTER INSERT OR UPDATE OR DELETE ON agent_permissions
    FOR EACH ROW EXECUTE FUNCTION audit_agent_permissions();
