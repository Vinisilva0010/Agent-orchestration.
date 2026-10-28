//! Each service role gets exactly the table operations its code uses, and
//! nothing else. Every check runs in a transaction that is rolled back.

use sqlx::PgPool;

const INSUFFICIENT_PRIVILEGE: &str = "42501";

const ALLOWED: &[(&str, &str)] = &[
    (
        "ao_gateway",
        "INSERT INTO task_tokens (token_hash, task_id, agent_id, expires_at) VALUES (decode(repeat('00', 32), 'hex'), 't', 'a', now())",
    ),
    (
        "ao_gateway",
        "UPDATE task_tokens SET revoked_at = now() WHERE false",
    ),
    (
        "ao_gateway",
        "INSERT INTO task_budgets (task_id, limit_micros) VALUES ('t', 1) ON CONFLICT (task_id) DO UPDATE SET limit_micros = EXCLUDED.limit_micros",
    ),
    (
        "ao_gateway",
        "INSERT INTO agent_budgets (agent_id, monthly_limit_micros) VALUES ('a', 1) ON CONFLICT (agent_id) DO UPDATE SET monthly_limit_micros = EXCLUDED.monthly_limit_micros",
    ),
    (
        "ao_gateway",
        "INSERT INTO monthly_spend (scope, scope_id, period) VALUES ('workspace', '', '2026-10') ON CONFLICT DO NOTHING",
    ),
    (
        "ao_gateway",
        "UPDATE monthly_spend SET reserved_micros = 0 WHERE false",
    ),
    (
        "ao_gateway",
        "INSERT INTO budget_reservations (task_id, agent_id, period, amount_micros) VALUES ('t', 'a', '2026-10', 0)",
    ),
    ("ao_gateway", "DELETE FROM budget_reservations WHERE false"),
    (
        "ao_gateway",
        "INSERT INTO usage_events (task_id, agent_id, provider, model, outcome, reserved_micros) VALUES ('t', 'a', 'mock', 'm', 'completed', 0)",
    ),
    ("ao_gateway", "SELECT count(*) FROM usage_events"),
    (
        "ao_gateway",
        "INSERT INTO provider_credentials (provider, ciphertext, nonce, key_version) VALUES ('gemini', decode(repeat('00', 17), 'hex'), decode(repeat('00', 12), 'hex'), 1) ON CONFLICT (provider) DO UPDATE SET ciphertext = EXCLUDED.ciphertext",
    ),
    ("ao_gateway", "SELECT provider FROM provider_credentials"),
    ("ao_gateway", "DELETE FROM provider_credentials WHERE false"),
    (
        "ao_engine",
        "WITH run AS (INSERT INTO engine_runs (id, agent_id, tier, budget_micros, status, state) VALUES ('run_' || repeat('0', 32), 'a', 'small', 1, 'running', '{}') RETURNING id) INSERT INTO engine_tool_executions (run_id, call_id, tool, state) SELECT id, 'c', 'x', 'started' FROM run",
    ),
    (
        "ao_engine",
        "UPDATE engine_runs SET version = version + 1 WHERE false",
    ),
    (
        "ao_engine",
        "UPDATE engine_tool_executions SET state = 'finished' WHERE false",
    ),
    ("ao_engine", "SELECT count(*) FROM engine_tool_executions"),
];

const DENIED: &[(&str, &str)] = &[
    ("ao_engine", "SELECT count(*) FROM provider_credentials"),
    ("ao_engine", "SELECT count(*) FROM task_tokens"),
    ("ao_engine", "SELECT count(*) FROM task_budgets"),
    ("ao_engine", "SELECT count(*) FROM usage_events"),
    ("ao_engine", "DELETE FROM engine_runs WHERE false"),
    ("ao_engine", "ALTER TABLE engine_runs ADD COLUMN x int"),
    ("ao_engine", "CREATE TABLE stolen (x int)"),
    ("ao_gateway", "SELECT count(*) FROM engine_runs"),
    (
        "ao_gateway",
        "UPDATE usage_events SET cost_micros = 0 WHERE false",
    ),
    ("ao_gateway", "DELETE FROM usage_events WHERE false"),
    ("ao_gateway", "TRUNCATE usage_events"),
    (
        "ao_gateway",
        "UPDATE budget_reservations SET amount_micros = 0 WHERE false",
    ),
    ("ao_gateway", "SELECT count(*) FROM _sqlx_migrations"),
    ("ao_gateway", "CREATE TABLE stolen (x int)"),
];

async fn try_as(pool: &PgPool, role: &str, sql: &'static str) -> Result<(), String> {
    let set_role = match role {
        "ao_gateway" => "SET LOCAL ROLE ao_gateway",
        "ao_engine" => "SET LOCAL ROLE ao_engine",
        other => panic!("unknown role {other}"),
    };
    let mut tx = pool.begin().await.unwrap();
    sqlx::query(set_role).execute(&mut *tx).await.unwrap();
    match sqlx::query(sql).execute(&mut *tx).await {
        Ok(_) => Ok(()),
        Err(sqlx::Error::Database(error)) => Err(error
            .code()
            .map(|code| code.to_string())
            .unwrap_or_default()),
        Err(error) => panic!("{sql}: {error}"),
    }
}

#[sqlx::test(migrations = false)]
async fn each_service_role_gets_only_what_its_code_uses(pool: PgPool) {
    server::migrate_all(&pool).await.unwrap();
    for (role, sql) in ALLOWED {
        assert_eq!(
            try_as(&pool, role, sql).await,
            Ok(()),
            "{role} must be allowed: {sql}"
        );
    }
    for (role, sql) in DENIED {
        assert_eq!(
            try_as(&pool, role, sql).await,
            Err(INSUFFICIENT_PRIVILEGE.to_string()),
            "{role} must be denied: {sql}"
        );
    }
}
