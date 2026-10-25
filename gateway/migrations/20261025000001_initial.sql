-- Initial gateway schema.
-- Money is stored in integer micro-dollars (1 USD = 1_000_000).
-- Constraints repeat the application rules so invalid data is rejected even if
-- application code has a bug.

CREATE TABLE task_tokens (
    token_hash  BYTEA       PRIMARY KEY CHECK (octet_length(token_hash) = 32),
    task_id     TEXT        NOT NULL CHECK (task_id ~ '^[A-Za-z0-9_-]{1,128}$'),
    agent_id    TEXT        NOT NULL CHECK (agent_id ~ '^[A-Za-z0-9_-]{1,128}$'),
    expires_at  TIMESTAMPTZ NOT NULL,
    revoked_at  TIMESTAMPTZ,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX task_tokens_task_id_idx ON task_tokens (task_id);
CREATE INDEX task_tokens_expires_at_idx ON task_tokens (expires_at);

CREATE TABLE task_budgets (
    task_id          TEXT        PRIMARY KEY CHECK (task_id ~ '^[A-Za-z0-9_-]{1,128}$'),
    limit_micros     BIGINT      NOT NULL CHECK (limit_micros > 0),
    spent_micros     BIGINT      NOT NULL DEFAULT 0 CHECK (spent_micros >= 0),
    reserved_micros  BIGINT      NOT NULL DEFAULT 0 CHECK (reserved_micros >= 0),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE agent_budgets (
    agent_id              TEXT        PRIMARY KEY CHECK (agent_id ~ '^[A-Za-z0-9_-]{1,128}$'),
    monthly_limit_micros  BIGINT      NOT NULL CHECK (monthly_limit_micros > 0),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Spending per calendar month (UTC) for the workspace and for each agent.
-- The workspace row uses scope_id = ''.
CREATE TABLE monthly_spend (
    scope            TEXT   NOT NULL CHECK (scope IN ('workspace', 'agent')),
    scope_id         TEXT   NOT NULL,
    period           TEXT   NOT NULL CHECK (period ~ '^[0-9]{4}-(0[1-9]|1[0-2])$'),
    spent_micros     BIGINT NOT NULL DEFAULT 0 CHECK (spent_micros >= 0),
    reserved_micros  BIGINT NOT NULL DEFAULT 0 CHECK (reserved_micros >= 0),
    PRIMARY KEY (scope, scope_id, period),
    CHECK ((scope = 'workspace' AND scope_id = '') OR (scope = 'agent' AND scope_id <> ''))
);

-- One row per model call. Append-only: updates, deletes and truncation are
-- rejected by triggers.
CREATE TABLE usage_events (
    id                  BIGINT      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    occurred_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    task_id             TEXT        NOT NULL,
    agent_id            TEXT        NOT NULL,
    provider            TEXT        NOT NULL,
    model               TEXT        NOT NULL,
    outcome             TEXT        NOT NULL CHECK (outcome IN ('completed', 'failed')),
    input_tokens        BIGINT      NOT NULL DEFAULT 0 CHECK (input_tokens >= 0),
    output_tokens       BIGINT      NOT NULL DEFAULT 0 CHECK (output_tokens >= 0),
    cache_read_tokens   BIGINT      NOT NULL DEFAULT 0 CHECK (cache_read_tokens >= 0),
    cache_write_tokens  BIGINT      NOT NULL DEFAULT 0 CHECK (cache_write_tokens >= 0),
    reserved_micros     BIGINT      NOT NULL CHECK (reserved_micros >= 0),
    cost_micros         BIGINT      NOT NULL DEFAULT 0 CHECK (cost_micros >= 0)
);
CREATE INDEX usage_events_task_id_idx ON usage_events (task_id);
CREATE INDEX usage_events_occurred_at_idx ON usage_events (occurred_at);

CREATE FUNCTION reject_usage_event_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'usage_events is append-only';
END;
$$;

CREATE TRIGGER usage_events_no_update_delete
    BEFORE UPDATE OR DELETE ON usage_events
    FOR EACH ROW EXECUTE FUNCTION reject_usage_event_change();

CREATE TRIGGER usage_events_no_truncate
    BEFORE TRUNCATE ON usage_events
    FOR EACH STATEMENT EXECUTE FUNCTION reject_usage_event_change();

-- Provider API keys encrypted with AES-256-GCM. The master key is never stored
-- in the database.
CREATE TABLE provider_credentials (
    provider     TEXT        PRIMARY KEY CHECK (provider IN ('anthropic', 'openai', 'gemini')),
    ciphertext   BYTEA       NOT NULL CHECK (octet_length(ciphertext) > 16),
    nonce        BYTEA       NOT NULL CHECK (octet_length(nonce) = 12),
    key_version  INTEGER     NOT NULL CHECK (key_version > 0),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
