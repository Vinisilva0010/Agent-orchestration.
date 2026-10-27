-- Runs pause with status awaiting_budget when the gateway refuses to reserve
-- the next call, so a person can raise the limit instead of losing the run.

ALTER TABLE engine_runs DROP CONSTRAINT engine_runs_status_check;
ALTER TABLE engine_runs ADD CONSTRAINT engine_runs_status_check CHECK (status IN (
    'running', 'awaiting_approval', 'awaiting_budget', 'completed',
    'budget_exceeded', 'max_steps', 'failed'));
