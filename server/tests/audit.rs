//! Audit events are written by the database itself, in the same transaction
//! as the change, chained by hash, and cannot be changed or forged.

use sqlx::{PgPool, Row};

/// Runs statements as a service role and commits them.
async fn commit_as(
    pool: &PgPool,
    set_role: &'static str,
    actor: Option<&str>,
    statements: &[&'static str],
) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query(set_role).execute(&mut *tx).await.unwrap();
    if let Some(actor) = actor {
        sqlx::query("SELECT set_config('ao.actor', $1, true)")
            .bind(actor)
            .execute(&mut *tx)
            .await
            .unwrap();
    }
    for sql in statements {
        sqlx::query(*sql).execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
}

async fn verify(pool: &PgPool) -> Option<i64> {
    sqlx::query_scalar("SELECT audit_verify()")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = false)]
async fn changes_are_recorded_in_order_without_secrets_and_chained(pool: PgPool) {
    server::migrate_all(&pool).await.unwrap();
    commit_as(&pool, "SET LOCAL ROLE ao_gateway", None, &[
        "INSERT INTO provider_credentials (provider, ciphertext, nonce, key_version) VALUES ('gemini', decode(repeat('ab', 17), 'hex'), decode(repeat('00', 12), 'hex'), 1)",
        "DELETE FROM provider_credentials WHERE provider = 'gemini'",
    ])
    .await;
    commit_as(&pool, "SET LOCAL ROLE ao_engine", Some("user:alice"), &[
        r#"INSERT INTO engine_runs (id, agent_id, tier, budget_micros, status, state) VALUES ('run_' || repeat('1', 32), 'support', 'small', 100, 'awaiting_approval', '{"pending": [{"id": "c1"}], "approved": null}')"#,
        r#"UPDATE engine_runs SET status = 'running', state = '{"pending": [{"id": "c1"}], "approved": ["c1"]}' WHERE id = 'run_' || repeat('1', 32)"#,
    ])
    .await;

    let rows = sqlx::query(
        "SELECT action, actor, db_role, details::text AS details FROM audit_events ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let actions: Vec<String> = rows.iter().map(|row| row.get("action")).collect();
    assert_eq!(
        actions,
        [
            "credential.stored",
            "credential.deleted",
            "run.created",
            "run.decision",
            "run.status"
        ]
    );

    let stored = &rows[0];
    assert_eq!(stored.get::<String, _>("db_role"), "ao_gateway");
    assert_eq!(stored.get::<String, _>("actor"), "ao_gateway");
    let details: String = stored.get("details");
    assert!(details.contains("key_version"), "{details}");
    assert!(
        !details.contains("abab") && !details.contains("cipher"),
        "secret in the audit log: {details}"
    );

    let decision = &rows[3];
    assert_eq!(decision.get::<String, _>("db_role"), "ao_engine");
    assert_eq!(decision.get::<String, _>("actor"), "user:alice");
    assert!(decision.get::<String, _>("details").contains("c1"));

    assert_eq!(verify(&pool).await, None);
}

#[sqlx::test(migrations = false)]
async fn the_log_cannot_be_changed_and_tampering_is_found(pool: PgPool) {
    server::migrate_all(&pool).await.unwrap();
    commit_as(
        &pool,
        "SET LOCAL ROLE ao_gateway",
        None,
        &[
            "INSERT INTO agent_budgets (agent_id, monthly_limit_micros) VALUES ('a', 1)",
            "UPDATE agent_budgets SET monthly_limit_micros = 2 WHERE agent_id = 'a'",
            "UPDATE agent_budgets SET monthly_limit_micros = 3 WHERE agent_id = 'a'",
        ],
    )
    .await;
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM audit_events ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(ids.len(), 3);
    assert_eq!(verify(&pool).await, None);

    // Even the database owner cannot change or delete events while the
    // triggers are on.
    for sql in [
        "UPDATE audit_events SET actor = 'someone else'",
        "DELETE FROM audit_events",
        "TRUNCATE audit_events",
    ] {
        assert!(
            sqlx::query(sql).execute(&pool).await.is_err(),
            "{sql} was allowed"
        );
    }

    // An owner who disables the triggers can edit a row, but the chain shows
    // exactly which one.
    sqlx::query("ALTER TABLE audit_events DISABLE TRIGGER USER")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE audit_events SET details = jsonb_set(details, '{monthly_limit_micros}', '999') WHERE id = $1")
        .bind(ids[1])
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE audit_events ENABLE TRIGGER USER")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(verify(&pool).await, Some(ids[1]));
}
