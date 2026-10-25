//! Schema tests against a real PostgreSQL. Each test gets its own temporary
//! database with all migrations applied, created and dropped by `sqlx::test`.
//! Requires DATABASE_URL pointing to a server where the user can create databases.

use sqlx::PgPool;

#[sqlx::test(migrations = "./migrations")]
async fn creates_all_tables(pool: PgPool) {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.tables \
         WHERE table_schema = 'public' ORDER BY table_name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    for expected in [
        "agent_budgets",
        "monthly_spend",
        "provider_credentials",
        "task_budgets",
        "task_tokens",
        "usage_events",
    ] {
        assert!(
            tables.iter().any(|t| t == expected),
            "missing table {expected}"
        );
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn usage_events_are_append_only(pool: PgPool) {
    sqlx::query(
        "INSERT INTO usage_events (task_id, agent_id, provider, model, outcome, reserved_micros, cost_micros) \
         VALUES ('task-1', 'agent-1', 'mock', 'mock-small', 'completed', 100, 21)",
    )
    .execute(&pool)
    .await
    .unwrap();

    assert!(
        sqlx::query("UPDATE usage_events SET cost_micros = 0")
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM usage_events")
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("TRUNCATE usage_events")
            .execute(&pool)
            .await
            .is_err()
    );

    let (count, cost): (i64, i64) =
        sqlx::query_as("SELECT count(*), max(cost_micros) FROM usage_events")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
    assert_eq!(cost, 21);
}

#[sqlx::test(migrations = "./migrations")]
async fn rejects_invalid_values(pool: PgPool) {
    let invalid = [
        "INSERT INTO task_budgets (task_id, limit_micros) VALUES ('task-1', 0)",
        "INSERT INTO task_budgets (task_id, limit_micros, spent_micros) VALUES ('task-1', 10, -1)",
        "INSERT INTO task_budgets (task_id, limit_micros) VALUES ('has space', 10)",
        "INSERT INTO agent_budgets (agent_id, monthly_limit_micros) VALUES ('agent-1', 0)",
        "INSERT INTO task_tokens (token_hash, task_id, agent_id, expires_at) \
         VALUES ('\\x00'::bytea, 'task-1', 'agent-1', now())",
        "INSERT INTO monthly_spend (scope, scope_id, period) VALUES ('workspace', '', '2026-13')",
        "INSERT INTO monthly_spend (scope, scope_id, period) VALUES ('agent', '', '2026-10')",
        "INSERT INTO monthly_spend (scope, scope_id, period) VALUES ('workspace', 'x', '2026-10')",
        "INSERT INTO provider_credentials (provider, ciphertext, nonce, key_version) \
         VALUES ('unknown', decode(repeat('00', 32), 'hex'), decode(repeat('00', 12), 'hex'), 1)",
        "INSERT INTO provider_credentials (provider, ciphertext, nonce, key_version) \
         VALUES ('anthropic', decode(repeat('00', 32), 'hex'), decode(repeat('00', 8), 'hex'), 1)",
        "INSERT INTO usage_events (task_id, agent_id, provider, model, outcome, reserved_micros) \
         VALUES ('t', 'a', 'mock', 'm', 'unknown', 1)",
    ];
    for statement in invalid {
        assert!(
            sqlx::query(statement).execute(&pool).await.is_err(),
            "should be rejected: {statement}"
        );
    }

    let valid = [
        "INSERT INTO task_budgets (task_id, limit_micros) VALUES ('task-1', 10)",
        "INSERT INTO monthly_spend (scope, scope_id, period) VALUES ('workspace', '', '2026-10')",
        "INSERT INTO monthly_spend (scope, scope_id, period) VALUES ('agent', 'agent-1', '2026-10')",
    ];
    for statement in valid {
        sqlx::query(statement).execute(&pool).await.unwrap();
    }
}
