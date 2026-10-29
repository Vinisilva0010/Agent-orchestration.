//! The engine's client for the sandbox service, and the run_python tool.
//! The engine never uses Docker: it only holds the sandbox URL and secret.

use std::{sync::Arc, time::Duration};

use reqwest::{Client, StatusCode, Url, header, redirect::Policy};
use schemars::JsonSchema;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::json;

use crate::tools::{Tool, TypedTool};

pub struct SandboxClient {
    run_url: Url,
    secret: SecretString,
    http: Client,
}

impl SandboxClient {
    pub fn new(base_url: &str, secret: SecretString) -> Result<Self, String> {
        let base = Url::parse(base_url).map_err(|_| "SANDBOX_URL is not a valid URL")?;
        let loopback = matches!(base.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
        let allowed = (base.scheme() == "https" && base.host_str().is_some())
            || (base.scheme() == "http" && loopback);
        if !allowed {
            return Err("SANDBOX_URL must use https, or http on loopback only".to_string());
        }
        if secret.expose_secret().chars().count() < 32 {
            return Err("SANDBOX_SECRET must have at least 32 characters".to_string());
        }
        let run_url = base
            .join("/v1/run")
            .map_err(|_| "SANDBOX_URL is not a valid URL")?;
        // reqwest is built without a bundled crypto backend; use ring, as the
        // gateway does. Ignored if one is already installed.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = Client::builder()
            // A run is limited to 30 seconds inside the sandbox.
            .timeout(Duration::from_secs(90))
            .connect_timeout(Duration::from_secs(5))
            .redirect(Policy::none())
            .no_proxy()
            .https_only(base.scheme() == "https")
            .build()
            .map_err(|_| "could not build the HTTP client")?;
        Ok(Self {
            run_url,
            secret,
            http,
        })
    }

    /// Runs code for a task and describes the result for the model.
    pub async fn run(&self, task_id: &str, code: &str) -> Result<String, String> {
        let body = serde_json::to_vec(&json!({ "task_id": task_id, "code": code }))
            .map_err(|_| "could not encode the request".to_string())?;
        let response = self
            .http
            .post(self.run_url.clone())
            .bearer_auth(self.secret.expose_secret())
            .header(header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| "the sandbox is not reachable".to_string())?;
        let status = response.status();
        if status == StatusCode::SERVICE_UNAVAILABLE {
            return Err("the sandbox is busy; try again shortly".to_string());
        }
        if !status.is_success() {
            return Err(format!(
                "the sandbox refused the code ({})",
                status.as_u16()
            ));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|_| "the sandbox response could not be read".to_string())?;
        let result: RunResult = serde_json::from_slice(&bytes)
            .map_err(|_| "unexpected sandbox response".to_string())?;
        Ok(result.describe())
    }
}

#[derive(Deserialize)]
struct RunResult {
    exit_code: Option<i32>,
    output: String,
    timed_out: bool,
    truncated: bool,
    files: Vec<String>,
}

impl RunResult {
    fn describe(&self) -> String {
        let mut text = if self.timed_out {
            "The code was stopped after 30 seconds.".to_string()
        } else {
            format!(
                "Exit code: {}",
                self.exit_code
                    .map_or_else(|| "unknown".to_string(), |code| code.to_string())
            )
        };
        if !self.output.is_empty() {
            text.push_str("\nOutput:\n");
            text.push_str(&self.output);
            if self.truncated {
                text.push_str("\n[output cut at 64 KB]");
            }
        }
        if !self.files.is_empty() {
            text.push_str("\nFiles saved in /data/out: ");
            text.push_str(&self.files.join(", "));
        }
        text
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunPythonArgs {
    /// Python 3.14 code to run.
    code: String,
}

/// The run_python tool for one task: every call runs in that task's folder.
pub fn run_python_tool(client: Arc<SandboxClient>, task_id: String) -> Arc<dyn Tool> {
    Arc::new(TypedTool::new(
        "run_python",
        "Runs Python 3.14 code in an isolated sandbox with no network. This task's files are read-only \
         in /data (results of earlier runs in /data/out). Write result files to /tmp/out. Print what you \
         need to see: output is limited to 64 KB and time to 30 seconds.",
        move |args: RunPythonArgs| {
            let client = Arc::clone(&client);
            let task_id = task_id.clone();
            async move { client.run(&task_id, &args.code).await }
        },
    ))
}
