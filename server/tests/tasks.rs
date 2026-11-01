//! Starting tasks from the workspace: who may, with what budget, how many at
//! once, and what the audit log keeps.

mod common;

use axum::http::{Method, StatusCode};
use common::{as_test, call, create_agent, grant, setup};
use serde_json::json;
use sqlx::PgPool;

fn task_uri(agent_id: &str) -> String {
    format!("/api/agents/{agent_id}/tasks")
}

#[sqlx::test(migrations = false)]
async fn only_people_allowed_on_the_agent_start_tasks(pool: PgPool) {
    let setup = setup(&pool).await;
    create_agent(&setup, "support", 500_000).await;
    let body = || Some(json!({ "task": "Summarize the open tickets" }));

    // No permission: the agent does not exist for them.
    let (status, _) = call(
        &setup.app,
        Method::POST,
        &task_uri("support"),
        &setup.bob.cookie,
        body(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Viewers never act, even with a permission.
    grant(&setup, setup.viewer.id, "support", "use").await;
    let (status, _) = call(
        &setup.app,
        Method::POST,
        &task_uri("support"),
        &setup.viewer.cookie,
        body(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Unknown and malformed agent ids look the same as hidden ones.
    for agent in ["missing", "bad%20id"] {
        let (status, _) = call(
            &setup.app,
            Method::POST,
            &task_uri(agent),
            &setup.admin.cookie,
            body(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{agent}");
    }

    grant(&setup, setup.alice.id, "support", "use").await;
    let (status, created) = call(
        &setup.app,
        Method::POST,
        &task_uri("support"),
        &setup.alice.cookie,
        body(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(created["id"].as_i64().is_some());
    let (status, _) = call(
        &setup.app,
        Method::POST,
        &task_uri("support"),
        &setup.admin.cookie,
        body(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let rows: Vec<(String, i64, i64, Option<String>)> = sqlx::query_as(
        "SELECT task, budget_micros, requested_by, run_id FROM task_starts ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "Summarize the open tickets");
    // The agent's task budget is the default.
    assert_eq!(rows[0].1, 500_000);
    assert_eq!(rows[0].2, setup.alice.id);
    assert_eq!(rows[0].3, None);

    // An archived agent takes no new tasks, even from its permission holders.
    let (status, _) = call(
        &setup.app,
        Method::PATCH,
        "/api/agents/support",
        &setup.admin.cookie,
        Some(json!({ "archived": true })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body_admin) = call(
        &setup.app,
        Method::POST,
        &task_uri("support"),
        &setup.admin.cookie,
        body(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body_admin["error"]["code"], "agent_archived");
    let (status, _) = call(
        &setup.app,
        Method::POST,
        &task_uri("support"),
        &setup.alice.cookie,
        body(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = false)]
async fn task_text_and_budget_are_validated(pool: PgPool) {
    let setup = setup(&pool).await;
    create_agent(&setup, "support", 500_000).await;
    grant(&setup, setup.alice.id, "support", "manage").await;
    let bad = [
        json!({ "task": "" }),
        json!({ "task": "   \n " }),
        json!({ "task": "x".repeat(8001) }),
        json!({ "task": "a\u{0}b" }),
        json!({ "task": "ok", "budget_micros": 0 }),
        json!({ "task": "ok", "budget_micros": -1 }),
        json!({ "task": "ok", "budget_micros": 100_000_001 }),
    ];
    for body in bad {
        let (status, _) = call(
            &setup.app,
            Method::POST,
            &task_uri("support"),
            &setup.admin.cookie,
            Some(body.clone()),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            &body.to_string()[..40.min(body.to_string().len())]
        );
    }
    let (status, _) = call(
        &setup.app,
        Method::POST,
        &task_uri("support"),
        &setup.admin.cookie,
        Some(json!({ "task": "ok", "agent_id": "other" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // 8000 characters, counted as characters, are fine.
    let (status, _) = call(
        &setup.app,
        Method::POST,
        &task_uri("support"),
        &setup.alice.cookie,
        Some(json!({ "task": "é".repeat(8000) })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    // Money: only administrators give a task more than the agent's budget.
    let (status, _) = call(
        &setup.app,
        Method::POST,
        &task_uri("support"),
        &setup.alice.cookie,
        Some(json!({ "task": "ok", "budget_micros": 500_001 })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &setup.app,
        Method::POST,
        &task_uri("support"),
        &setup.alice.cookie,
        Some(json!({ "task": "ok", "budget_micros": 200_000 })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, _) = call(
        &setup.app,
        Method::POST,
        &task_uri("support"),
        &setup.admin.cookie,
        Some(json!({ "task": "ok", "budget_micros": 2_000_000 })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let budgets: Vec<i64> = sqlx::query_scalar("SELECT budget_micros FROM task_starts ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(budgets, vec![500_000, 200_000, 2_000_000]);
}

#[sqlx::test(migrations = false)]
async fn pending_task_requests_are_limited_per_person(pool: PgPool) {
    let setup = setup(&pool).await;
    create_agent(&setup, "support", 500_000).await;
    grant(&setup, setup.alice.id, "support", "use").await;
    let body = || Some(json!({ "task": "work" }));

    // Requests sent at the same moment cannot pass the limit together.
    let mut handles = Vec::new();
    for _ in 0..8 {
        let app = setup.app.clone();
        let cookie = setup.alice.cookie.clone();
        handles.push(tokio::spawn(async move {
            call(
                &app,
                Method::POST,
                &task_uri("support"),
                &cookie,
                Some(json!({ "task": "work" })),
            )
            .await
        }));
    }
    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await.unwrap());
    }
    let accepted = results
        .iter()
        .filter(|(status, _)| *status == StatusCode::ACCEPTED)
        .count();
    assert_eq!(accepted, 5);
    let limited = results
        .iter()
        .filter(|(status, body)| {
            *status == StatusCode::TOO_MANY_REQUESTS
                && body["error"]["code"] == "too_many_pending_tasks"
        })
        .count();
    assert_eq!(limited, 3);

    // The limit is per person.
    let (status, _) = call(
        &setup.app,
        Method::POST,
        &task_uri("support"),
        &setup.admin.cookie,
        body(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    // Once the engine takes a request, there is room again.
    as_test(
        &pool,
        "UPDATE task_starts SET processed_at = now(), outcome = 'failed: test' \
         WHERE id = (SELECT min(id) FROM task_starts)",
    )
    .await;
    let (status, _) = call(
        &setup.app,
        Method::POST,
        &task_uri("support"),
        &setup.alice.cookie,
        body(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[sqlx::test(migrations = false)]
async fn task_requests_are_audited_without_their_text(pool: PgPool) {
    let setup = setup(&pool).await;
    create_agent(&setup, "support", 500_000).await;
    grant(&setup, setup.alice.id, "support", "use").await;
    let secret_text = "Customer card 4111 1111 1111 1111 needs a refund";
    let (status, _) = call(
        &setup.app,
        Method::POST,
        &task_uri("support"),
        &setup.alice.cookie,
        Some(json!({ "task": secret_text })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let (actor, target, details): (String, String, String) = sqlx::query_as(
        "SELECT actor, target, details::text FROM audit_events WHERE action = 'task.requested'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(actor, format!("user:{}", setup.alice.id));
    assert_eq!(target, "support");
    assert!(!details.contains("4111"), "{details}");
    assert!(details.contains("task_sha256"), "{details}");
    assert!(details.contains("\"budget_micros\": 500000"), "{details}");

    // A processed request is final, even for the database owner.
    as_test(
        &pool,
        "UPDATE task_starts SET processed_at = now(), outcome = 'failed: test'",
    )
    .await;
    let mut tx = pool.begin().await.unwrap();
    let changed = sqlx::query("UPDATE task_starts SET processed_at = NULL, outcome = NULL")
        .execute(&mut *tx)
        .await;
    assert!(changed.is_err(), "a processed request was reopened");
    let (action,): (String,) =
        sqlx::query_as("SELECT action FROM audit_events ORDER BY id DESC LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(action, "task.failed");
}

#[sqlx::test(migrations = false)]
async fn starting_a_task_needs_the_workspace_origin(pool: PgPool) {
    let setup = setup(&pool).await;
    create_agent(&setup, "support", 500_000).await;
    let request = axum::http::Request::builder()
        .method(Method::POST)
        .uri(task_uri("support"))
        .header(axum::http::header::ORIGIN, "https://evil.example")
        .header(axum::http::header::COOKIE, &setup.admin.cookie)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(json!({ "task": "x" }).to_string()))
        .unwrap();
    let response = tower::ServiceExt::oneshot(setup.app.clone(), request)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM task_starts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
