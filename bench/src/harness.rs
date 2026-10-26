//! Runs benchmark trials against a gateway.
//!
//! Each trial gets a fresh desk, its own task token and its own budget. The
//! benchmark approves every irreversible action automatically: it measures
//! cost and correctness, while the approval pause has its own tests.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use engine::{
    agent::{Agent, AgentConfig, DEFAULT_SYSTEM, Decision, RunStatus},
    gateway::{GatewayClient, issue_task_token},
};
use protocol::{ModelTier, Usage};
use secrecy::SecretString;

use crate::{
    desk::Desk,
    metrics::TrialResult,
    tasks::{HANDBOOK, Task},
};

pub const AGENT_ID: &str = "bench";
const MAX_STEPS: u32 = 12;
const MAX_OUTPUT_TOKENS: u32 = 1024;

pub struct BenchConfig {
    pub gateway_url: String,
    pub issuer_secret: SecretString,
    pub tier: ModelTier,
    pub budget_micros: u64,
}

pub fn system_prompt() -> String {
    format!("{DEFAULT_SYSTEM}\n\n{HANDBOOK}")
}

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{nanos:x}")
}

pub async fn run_trial(config: &BenchConfig, task: &Task, trial: u32) -> TrialResult {
    let failed = |error: String| TrialResult {
        task: task.id.to_string(),
        trial,
        success: false,
        status: RunStatus::Failed,
        steps: 0,
        cost_micros: 0,
        usage: Usage::default(),
        error: Some(error),
    };

    let desk = Desk::new();
    let task_id = format!("bench-{}-{trial}-{}", task.id, unique_suffix());
    let token = match issue_task_token(
        &config.gateway_url,
        &config.issuer_secret,
        &task_id,
        AGENT_ID,
        config.budget_micros,
    )
    .await
    {
        Ok(token) => token,
        Err(error) => return failed(format!("could not get a task token: {error}")),
    };
    let client = match GatewayClient::new(&config.gateway_url, token) {
        Ok(client) => client,
        Err(error) => return failed(error.to_string()),
    };
    let agent = Agent::new(
        client,
        desk.tools(),
        AgentConfig {
            tier: config.tier,
            system: system_prompt(),
            max_steps: MAX_STEPS,
            max_output_tokens: MAX_OUTPUT_TOKENS,
        },
    );

    let mut state = agent.drive(agent.start(task.prompt)).await;
    while state.status == RunStatus::AwaitingApproval {
        match agent.decide(state.clone(), &Decision::ApproveAll).await {
            Ok(next) => state = next,
            Err(_) => break,
        }
    }

    TrialResult {
        task: task.id.to_string(),
        trial,
        success: state.status == RunStatus::Completed && task.check(&desk.snapshot()),
        status: state.status,
        steps: state.steps,
        cost_micros: state.cost_micros,
        usage: state.usage,
        error: state.error,
    }
}

/// Runs every task `trials` times, waiting `pace` between trials.
pub async fn run_all(
    config: &BenchConfig,
    tasks: &[Task],
    trials: u32,
    pace: Duration,
    mut on_result: impl FnMut(&TrialResult),
) -> Vec<TrialResult> {
    let mut results = Vec::new();
    for trial in 1..=trials {
        for task in tasks {
            if !results.is_empty() && !pace.is_zero() {
                tokio::time::sleep(pace).await;
            }
            let result = run_trial(config, task, trial).await;
            on_result(&result);
            results.push(result);
        }
    }
    results
}
