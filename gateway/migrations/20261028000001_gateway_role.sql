-- The gateway connects as ao_gateway, which may only use the gateway tables,
-- with exactly the operations the gateway code runs. It cannot create or
-- change tables: migrations run with the database owner (server migrate).
-- The role is created without login; server migrate sets its password.

DO $$
BEGIN
    CREATE ROLE ao_gateway NOLOGIN;
EXCEPTION WHEN duplicate_object OR unique_violation THEN
    NULL;
END
$$;

GRANT SELECT, INSERT, UPDATE ON task_tokens, task_budgets, agent_budgets, monthly_spend TO ao_gateway;
-- Reservations are created and finished (deleted), never changed.
GRANT SELECT, INSERT, DELETE ON budget_reservations TO ao_gateway;
-- Append-only, by privilege as well as by trigger.
GRANT SELECT, INSERT ON usage_events TO ao_gateway;
GRANT SELECT, INSERT, UPDATE, DELETE ON provider_credentials TO ao_gateway;
GRANT USAGE ON SEQUENCE budget_reservations_id_seq, usage_events_id_seq TO ao_gateway;
