-- Agent runs and the at-most-once log of irreversible tool executions.

CREATE TABLE engine_runs (
    id            TEXT PRIMARY KEY CHECK (id ~ '^run_[0-9a-f]{32}$'),
    agent_id      TEXT NOT NULL CHECK (agent_id ~ '^[A-Za-z0-9_-]{1,128}$'),
    tier          TEXT NOT NULL CHECK (tier IN ('small', 'standard', 'large')),
    budget_micros BIGINT NOT NULL CHECK (budget_micros > 0),
    status        TEXT NOT NULL CHECK (status IN (
                      'running', 'awaiting_approval', 'completed',
                      'budget_exceeded', 'max_steps', 'failed')),
    state         JSONB NOT NULL,
    -- Incremented on every save; a save with a stale version is rejected.
    version       BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX engine_runs_awaiting_approval ON engine_runs (updated_at)
    WHERE status = 'awaiting_approval';

CREATE TABLE engine_tool_executions (
    run_id      TEXT NOT NULL REFERENCES engine_runs (id),
    call_id     TEXT NOT NULL CHECK (length(call_id) BETWEEN 1 AND 256),
    tool        TEXT NOT NULL,
    state       TEXT NOT NULL CHECK (state IN ('started', 'finished')),
    result      TEXT,
    is_error    BOOLEAN,
    started_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ,
    PRIMARY KEY (run_id, call_id),
    CHECK (
        (state = 'started' AND result IS NULL AND is_error IS NULL AND finished_at IS NULL)
        OR (state = 'finished' AND result IS NOT NULL AND is_error IS NOT NULL
            AND finished_at IS NOT NULL)
    )
);
