//! The run_python tool sends the run's own task id with the sandbox secret,
//! and turns sandbox refusals into tool errors.

use std::sync::Arc;

use engine::{
    sandbox::{SandboxClient, run_python_tool},
    tools::Tool,
};
use secrecy::SecretString;
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path},
};

const SECRET: &str = "engine-test-sandbox-secret-0123456789";

fn tool(server: &MockServer, task_id: &str) -> Arc<dyn Tool> {
    let client = SandboxClient::new(&server.uri(), SecretString::from(SECRET.to_string())).unwrap();
    run_python_tool(Arc::new(client), task_id.to_string())
}

#[tokio::test]
async fn run_python_sends_the_runs_task_and_describes_the_result() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/run"))
        .and(header("authorization", format!("Bearer {SECRET}").as_str()))
        .and(body_json(json!({ "task_id": "run_1", "code": "print(6 * 7)" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "exit_code": 0, "output": "42\n", "timed_out": false, "truncated": false, "files": ["report.txt"]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let text = tool(&server, "run_1")
        .run(json!({ "code": "print(6 * 7)" }))
        .await
        .unwrap();
    assert!(
        text.contains("Exit code: 0") && text.contains("42") && text.contains("report.txt"),
        "{text}"
    );
}

#[tokio::test]
async fn sandbox_refusals_become_tool_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .with_priority(2)
        .mount(&server)
        .await;
    let tool = tool(&server, "run_1");
    assert!(
        tool.run(json!({ "code": "x" }))
            .await
            .unwrap_err()
            .contains("busy")
    );
    assert!(
        tool.run(json!({ "code": "x" }))
            .await
            .unwrap_err()
            .contains("refused")
    );
}

#[test]
fn the_sandbox_must_be_on_loopback_or_https() {
    let secret = || SecretString::from(SECRET.to_string());
    assert!(SandboxClient::new("http://10.0.0.5:8095", secret()).is_err());
    assert!(
        SandboxClient::new(
            "http://127.0.0.1:8095",
            SecretString::from("short".to_string())
        )
        .is_err()
    );
    assert!(SandboxClient::new("http://127.0.0.1:8095", secret()).is_ok());
}
