-- Open budget reservations. A row exists while a model call is in flight.
-- Finishing a reservation deletes its row and adjusts balances in the same
-- transaction, so a reservation can never be finished twice. Rows left behind
-- by a crash are released by the stale reservation sweeper.
CREATE TABLE budget_reservations (
    id             BIGINT      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    task_id        TEXT        NOT NULL,
    agent_id       TEXT        NOT NULL,
    period         TEXT        NOT NULL CHECK (period ~ '^[0-9]{4}-(0[1-9]|1[0-2])$'),
    amount_micros  BIGINT      NOT NULL CHECK (amount_micros >= 0),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX budget_reservations_created_at_idx ON budget_reservations (created_at);
