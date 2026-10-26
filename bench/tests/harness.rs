use bench::{
    harness::{BenchConfig, run_trial},
    tasks,
};
use engine::agent::RunStatus;
use protocol::ModelTier;
use secrecy::SecretString;
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

fn reply(content: Value, stop_reason: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "provider": "mock", "model": "mock-small", "content": content, "stop_reason": stop_reason,
        "usage": { "input_tokens": 10, "output_tokens": 5, "cache_read_tokens": 1500, "cache_write_tokens": 0 },
        "cost_micros": 5, "task_budget_remaining_micros": 100
    }))
}

async fn gateway(close_id: u32) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/internal/tasks/tokens"))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(json!({ "token": "tsk_bench", "expires_at": 0 })),
        )
        .mount(&server)
        .await;
    let call = json!([{ "type": "tool_use", "id": "call_1", "name": "close_ticket",
        "input": { "id": close_id, "resolution": "A new password reset link was sent." } }]);
    let replies = [
        reply(call, "tool_use"),
        reply(json!([{ "type": "text", "text": "Closed." }]), "end_turn"),
    ];
    for (index, reply) in replies.into_iter().enumerate() {
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(reply)
            .up_to_n_times(1)
            .with_priority(u8::try_from(index + 1).unwrap())
            .mount(&server)
            .await;
    }
    server
}

fn config(server: &MockServer) -> BenchConfig {
    BenchConfig {
        gateway_url: server.uri(),
        issuer_secret: SecretString::from("test-issuer-secret".to_string()),
        tier: ModelTier::Small,
        budget_micros: 200_000,
    }
}

#[tokio::test]
async fn a_correct_run_is_solved_and_costed() {
    let server = gateway(4).await;
    let all = tasks::all();
    let task = all.iter().find(|t| t.id == "close-reset-email").unwrap();
    let result = run_trial(&config(&server), task, 1).await;
    assert_eq!(result.status, RunStatus::Completed);
    assert!(result.success, "{result:?}");
    assert_eq!(result.cost_micros, 10);
    assert_eq!(result.usage.cache_read_tokens, 3000);
}

#[tokio::test]
async fn closing_the_wrong_ticket_is_not_solved() {
    let server = gateway(2).await;
    let all = tasks::all();
    let task = all.iter().find(|t| t.id == "close-reset-email").unwrap();
    let result = run_trial(&config(&server), task, 1).await;
    assert_eq!(result.status, RunStatus::Completed);
    assert!(!result.success);
}
