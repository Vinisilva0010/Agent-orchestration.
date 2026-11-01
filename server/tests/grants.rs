//! Reading a person's permissions on agents: administrators only.

mod common;

use axum::http::{Method, StatusCode};
use common::{call, create_agent, grant, setup};
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test(migrations = false)]
async fn only_administrators_read_permissions(pool: PgPool) {
    let setup = setup(&pool).await;
    create_agent(&setup, "support", 500_000).await;
    create_agent(&setup, "finance", 500_000).await;
    grant(&setup, setup.alice.id, "support", "use").await;
    grant(&setup, setup.alice.id, "finance", "manage").await;
    let uri = format!("/api/users/{}/agents", setup.alice.id);

    for person in [&setup.alice, &setup.bob, &setup.viewer] {
        let (status, _) = call(&setup.app, Method::GET, &uri, &person.cookie, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    let (status, body) = call(&setup.app, Method::GET, &uri, &setup.admin.cookie, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!([
            { "agent_id": "finance", "level": "manage" },
            { "agent_id": "support", "level": "use" }
        ])
    );
    let (status, _) = call(
        &setup.app,
        Method::GET,
        "/api/users/999999/agents",
        &setup.admin.cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
