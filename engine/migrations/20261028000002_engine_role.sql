-- The engine connects as ao_engine, which may only use the run tables and
-- never delete from them. It has no access to provider credentials, task
-- tokens or budgets. Migrations run with the database owner (server migrate).

DO $$
BEGIN
    CREATE ROLE ao_engine NOLOGIN;
EXCEPTION WHEN duplicate_object OR unique_violation THEN
    NULL;
END
$$;

GRANT SELECT, INSERT, UPDATE ON engine_runs, engine_tool_executions TO ao_engine;
