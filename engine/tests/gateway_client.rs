use engine::gateway::{GatewayClient, GatewayError, validate_base_url};
use protocol::{CompletionRequest, ContentBlock, Message, ModelTier, Role};
use secrecy::SecretString;
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

const TOKEN: &str = "tsk_test-token";

fn token() -> SecretString {
    SecretString::from(TOKEN.to_string())
}

fn request() -> CompletionRequest {
    CompletionRequest {
        tier: ModelTier::Small,
        system: vec![],
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hi".to_string(),
                signature: None,
            }],
        }],
        tools: vec![],
        max_output_tokens: 64,
    }
}

fn ok_body() -> serde_json::Value {
    json!({
        "provider": "mock", "model": "mock-small", "content": [{ "type": "text", "text": "hello" }],
        "stop_reason": "end_turn",
        "usage": { "input_tokens": 1, "output_tokens": 1, "cache_read_tokens": 0, "cache_write_tokens": 0 },
        "cost_micros": 7, "task_budget_remaining_micros": 93
    })
}

#[test]
fn accepts_https_and_loopback_http_only() {
    for good in [
        "https://gateway.example.com",
        "http://127.0.0.1:8081",
        "http://localhost:8081",
    ] {
        assert!(validate_base_url(good).is_ok(), "{good}");
    }
    for bad in [
        "http://gateway.example.com",
        "ftp://127.0.0.1",
        "127.0.0.1:8081",
        "",
    ] {
        assert!(validate_base_url(bad).is_err(), "{bad}");
    }
}

#[test]
fn requires_a_task_token_and_never_shows_it() {
    let wrong = GatewayClient::new(
        "http://127.0.0.1:8081",
        SecretString::from("sk-key".to_string()),
    );
    assert!(matches!(wrong, Err(GatewayError::Config(_))));
    let client = GatewayClient::new("http://127.0.0.1:8081", token()).unwrap();
    assert!(!format!("{client:?}").contains(TOKEN));
}

#[tokio::test]
async fn sends_the_request_with_only_the_task_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(1)
        .mount(&server)
        .await;

    let client = GatewayClient::new(&server.uri(), token()).unwrap();
    let response = client.complete(&request()).await.unwrap();
    assert_eq!(response.cost_micros, 7);
    assert_eq!(response.task_budget_remaining_micros, Some(93));
    assert_eq!(response.completion.model, "mock-small");

    let sent: CompletionRequest =
        serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
    assert_eq!(sent, request());
}

#[tokio::test]
async fn maps_gateway_errors() {
    for (status, check) in [
        (
            402,
            (|e: &GatewayError| matches!(e, GatewayError::BudgetExceeded(m) if m == "spent"))
                as fn(&GatewayError) -> bool,
        ),
        (429, |e| {
            matches!(e, GatewayError::ProviderUnavailable { status: 429, .. })
        }),
        (503, |e| {
            matches!(e, GatewayError::ProviderUnavailable { status: 503, .. })
        }),
        (400, |e| {
            matches!(e, GatewayError::Rejected { status: 400, .. })
        }),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(status)
                    .set_body_json(json!({ "error": { "code": "x", "message": "spent" } })),
            )
            .mount(&server)
            .await;
        let error = GatewayClient::new(&server.uri(), token())
            .unwrap()
            .complete(&request())
            .await
            .unwrap_err();
        assert!(check(&error), "status {status}: {error:?}");
    }
}

#[tokio::test]
async fn reports_unreachable_gateways_and_bad_bodies() {
    // Port 9 (discard) has no listener on a development machine.
    let error = GatewayClient::new("http://127.0.0.1:9", token())
        .unwrap()
        .complete(&request())
        .await
        .unwrap_err();
    assert_eq!(error, GatewayError::Unreachable);

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;
    let error = GatewayClient::new(&server.uri(), token())
        .unwrap()
        .complete(&request())
        .await
        .unwrap_err();
    assert_eq!(error, GatewayError::InvalidResponse);
}
