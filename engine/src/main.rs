//! Command line runner for the engine, for development and tests.
//!
//!   engine run [--tier small|standard|large] [--budget-micros N] [--detach] "task"
//!   engine decide RUN_ID approve|reject
//!
//! Reads DATABASE_URL, GATEWAY_ISSUER_SECRET and ENGINE_GATEWAY_URL (default
//! http://127.0.0.1:8081). Refuses to start with provider keys or the vault
//! master key in the environment. Prints one JSON event per line.
//!
//! Exit codes: 0 completed, 2 budget exceeded, 3 other failure, 4 unsafe
//! environment or configuration, 10 waiting for approval, 64 usage error.

use std::{
    io::BufRead,
    process::ExitCode,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use engine::{
    agent::{Agent, AgentConfig, Decision, RunState, RunStatus},
    gateway::{GatewayClient, issue_task_token},
    runner::Runner,
    secrets_guard,
    store::{RunStore, StoredRun, connect_and_migrate},
    tools::{Tool, ToolRegistry, TypedTool},
};
use protocol::ModelTier;
use schemars::JsonSchema;
use secrecy::SecretString;
use serde::Deserialize;
use serde_json::json;

const USAGE: &str = "usage:\n  engine run [--tier small|standard|large] [--budget-micros N] \
[--detach] \"task\"\n  engine decide RUN_ID approve|reject";
const DEFAULT_GATEWAY_URL: &str = "http://127.0.0.1:8081";
const DEFAULT_BUDGET_MICROS: u64 = 50_000;
const AGENT_ID: &str = "demo-agent";

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ArchiveArgs {
    #[serde(default = "latest_report")]
    report_id: String,
}

fn latest_report() -> String {
    "latest".to_string()
}

fn demo_tools() -> ToolRegistry {
    let get_time = TypedTool::new(
        "get_time",
        "Returns the current Unix time.",
        |_: NoArgs| async {
            let seconds = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "clock error".to_string())?
                .as_secs();
            Ok(format!("unix time {seconds}"))
        },
    );
    let archive = TypedTool::new(
        "archive_report",
        "Archives a report. This cannot be undone.",
        |args: ArchiveArgs| async move {
            Ok(format!(
                "report {} archived (demo tool: nothing was changed)",
                args.report_id
            ))
        },
    )
    .mark_irreversible();
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(get_time), Arc::new(archive)];
    ToolRegistry::new(tools).unwrap_or_default()
}

struct Settings {
    gateway_url: String,
    issuer_secret: SecretString,
    database_url: SecretString,
}

fn settings() -> Result<Settings, String> {
    let required = |name: &str| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("{name} is not set"))
    };
    Ok(Settings {
        gateway_url: std::env::var("ENGINE_GATEWAY_URL")
            .unwrap_or_else(|_| DEFAULT_GATEWAY_URL.to_string()),
        issuer_secret: SecretString::from(required("GATEWAY_ISSUER_SECRET")?),
        database_url: SecretString::from(required("DATABASE_URL")?),
    })
}

fn exit_code(status: RunStatus) -> u8 {
    match status {
        RunStatus::Completed => 0,
        RunStatus::BudgetExceeded => 2,
        RunStatus::AwaitingApproval => 10,
        RunStatus::Running | RunStatus::MaxSteps | RunStatus::Failed => 3,
    }
}

fn fail(message: &str) -> u8 {
    eprintln!("{message}");
    3
}

async fn build_agent(settings: &Settings, run: &StoredRun) -> Result<Agent, String> {
    let token = issue_task_token(
        &settings.gateway_url,
        &settings.issuer_secret,
        &run.id,
        &run.agent_id,
        run.budget_micros,
    )
    .await
    .map_err(|error| format!("could not get a task token: {error}"))?;
    let client = GatewayClient::new(&settings.gateway_url, token).map_err(|e| e.to_string())?;
    let config = AgentConfig {
        tier: run.tier,
        ..AgentConfig::default()
    };
    Ok(Agent::new(client, demo_tools(), config))
}

fn emit_approval(agent: &Agent, run: &StoredRun) {
    println!(
        "{}",
        json!({ "event": "approval_required", "run_id": run.id, "actions": agent.approval_actions(&run.state) })
    );
}

fn emit_finished(run: &StoredRun) {
    println!(
        "{}",
        json!({
            "event": "finished", "run_id": run.id, "status": run.state.status,
            "steps": run.state.steps, "cost_micros": run.state.cost_micros,
            "final_text": run.state.final_text, "error": run.state.error,
        })
    );
}

async fn command_run(settings: &Settings, store: &RunStore, args: &[String]) -> u8 {
    let mut tier = ModelTier::Standard;
    let mut budget = DEFAULT_BUDGET_MICROS;
    let mut detach = false;
    let mut task = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--detach" => detach = true,
            "--tier" => {
                tier = match rest.next().map(String::as_str) {
                    Some("small") => ModelTier::Small,
                    Some("standard") => ModelTier::Standard,
                    Some("large") => ModelTier::Large,
                    _ => return fail("--tier must be small, standard or large"),
                };
            }
            "--budget-micros" => match rest.next().and_then(|value| value.parse().ok()) {
                Some(value) if value > 0 => budget = value,
                _ => return fail("--budget-micros must be a positive integer"),
            },
            other if task.is_none() && !other.starts_with("--") => task = Some(other.to_string()),
            _ => {
                eprintln!("{USAGE}");
                return 64;
            }
        }
    }
    let Some(task) = task else {
        eprintln!("{USAGE}");
        return 64;
    };

    let run = match store
        .create(AGENT_ID, tier, budget, RunState::new(&task))
        .await
    {
        Ok(run) => run,
        Err(error) => return fail(&error.to_string()),
    };
    let agent = match build_agent(settings, &run).await {
        Ok(agent) => agent,
        Err(error) => return fail(&error),
    };
    let runner = Runner::new(&agent, store);
    let mut run = match runner.drive(run).await {
        Ok(run) => run,
        Err(error) => return fail(&error.to_string()),
    };
    while run.state.status == RunStatus::AwaitingApproval {
        emit_approval(&agent, &run);
        if detach {
            return 10;
        }
        let mut line = String::new();
        let _ = std::io::stdin().lock().read_line(&mut line);
        let decision = if line.trim() == "approve" {
            Decision::ApproveAll
        } else {
            Decision::RejectAll
        };
        println!(
            "{}",
            json!({ "event": "decision", "approved": decision == Decision::ApproveAll })
        );
        run = match runner.decide(&run.id, &decision).await {
            Ok(run) => run,
            Err(error) => return fail(&error.to_string()),
        };
    }
    emit_finished(&run);
    exit_code(run.state.status)
}

async fn command_decide(settings: &Settings, store: &RunStore, run_id: &str, answer: &str) -> u8 {
    let decision = match answer {
        "approve" => Decision::ApproveAll,
        "reject" => Decision::RejectAll,
        _ => {
            eprintln!("{USAGE}");
            return 64;
        }
    };
    let run = match store.load(run_id).await {
        Ok(run) => run,
        Err(error) => return fail(&error.to_string()),
    };
    let agent = match build_agent(settings, &run).await {
        Ok(agent) => agent,
        Err(error) => return fail(&error),
    };
    let run = match Runner::new(&agent, store).decide(run_id, &decision).await {
        Ok(run) => run,
        Err(error) => return fail(&error.to_string()),
    };
    if run.state.status == RunStatus::AwaitingApproval {
        emit_approval(&agent, &run);
    } else {
        emit_finished(&run);
    }
    exit_code(run.state.status)
}

async fn run_cli(args: &[String]) -> u8 {
    let command = args.first().map(String::as_str);
    if !matches!(command, Some("run")) && !(command == Some("decide") && args.len() == 3) {
        eprintln!("{USAGE}");
        return 64;
    }
    if let Err(error) = secrets_guard::check_environment() {
        eprintln!("{error}");
        return 4;
    }
    let settings = match settings() {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("{error}");
            return 4;
        }
    };
    let store = match connect_and_migrate(&settings.database_url).await {
        Ok(pool) => RunStore::new(pool),
        Err(error) => {
            eprintln!("database: {error}");
            return 4;
        }
    };
    match command {
        Some("run") => command_run(&settings, &store, &args[1..]).await,
        _ => command_decide(&settings, &store, &args[1], &args[2]).await,
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ExitCode::from(run_cli(&args).await)
}
