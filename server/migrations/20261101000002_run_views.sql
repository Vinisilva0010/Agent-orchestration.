-- The server shows each run's tool activity: it reads the execution log of
-- irreversible calls, and never changes it.
GRANT SELECT ON engine_tool_executions TO ao_server;
