//! Command line runner for the engine, for development and tests.
//!
//!   engine run [--tier small|standard|large] [--budget-micros N] [--detach] "task"
//!   engine decide RUN_ID approve|reject
//!   engine worker [--once]
//!     Carries out people's requests recorded by the server; --once stops
//!     when none is left.
//!
//! Reads DATABASE_URL, GATEWAY_ISSUER_SECRET and ENGINE_GATEWAY_URL (default
//! http://127.0.0.1:8081). Refuses to start with provider keys or the vault
//! master key in the environment. Prints one JSON event per line.
//!
//! Exit codes: 0 completed, 2 budget exceeded, 3 other failure, 4 unsafe
//! environment or configuration, 10 waiting for approval, 64 usage error.

use std::{
    collections::HashMap,
    io::BufRead,
    process::ExitCode,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use engine::{
    agent::{Agent, AgentConfig, Decision, RunState, RunStatus},
    gateway::{GatewayClient, issue_task_token},
    runner::Runner,
    secrets_guard,
    store::{RunRequest, RunStore, StoredRun, connect},
    tools::{Tool, ToolRegistry, TypedTool},
};
use protocol::ModelTier;
use schemars::JsonSchema;
use secrecy::SecretString;
use serde::Deserialize;
use serde_json::{Value, json};

const USAGE: &str = "usage:\n  engine run [--tier small|standard|large] [--budget-micros N] \
[--detach] \"task\"\n  engine decide RUN_ID approve|reject\n  engine worker [--once]";
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
        RunStatus::AwaitingBudget => 11,
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
    if !matches!(command, Some("run" | "worker")) && !(command == Some("decide") && args.len() == 3)
    {
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
    let store = match connect(&settings.database_url).await {
        Ok(pool) => RunStore::new(pool),
        Err(error) => {
            eprintln!("database: {error}");
            return 4;
        }
    };
    match command {
        Some("run") => command_run(&settings, &store, &args[1..]).await,
        Some("worker") => command_worker(&settings, &store, &args[1..]).await,
        _ => command_decide(&settings, &store, &args[1], &args[2]).await,
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ExitCode::from(run_cli(&args).await)
}

async fn command_worker(settings: &Settings, store: &RunStore, args: &[String]) -> u8 {
    let once = match args {
        [] => false,
        [flag] if flag == "--once" => true,
        _ => {
            eprintln!("{USAGE}");
            return 64;
        }
    };
    println!("{}", json!({ "event": "worker_started", "once": once }));
    loop {
        let claimed = tokio::select! {
            _ = tokio::signal::ctrl_c() => return 0,
            claimed = store.claim_request() => claimed,
        };
        match claimed {
            Ok(Some(request)) => {
                let outcome = match apply_request(settings, store, &request).await {
                    Ok(outcome) => outcome,
                    Err(error) => format!("failed: {error}"),
                };
                if let Err(error) = store.finish_request(request.id, &outcome).await {
                    eprintln!(
                        "could not record the outcome of request {}: {error}",
                        request.id
                    );
                }
                println!(
                    "{}",
                    json!({ "event": "request", "id": request.id, "run_id": request.run_id, "kind": request.kind, "outcome": outcome })
                );
            }
            Ok(None) if once => return 0,
            Ok(None) => tokio::time::sleep(Duration::from_secs(2)).await,
            Err(error) => {
                eprintln!("could not read requests: {error}");
                if once {
                    return 3;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

/// Carries out one request. Requests for runs that are no longer waiting are
/// skipped, never applied to a later state.
async fn apply_request(
    settings: &Settings,
    store: &RunStore,
    request: &RunRequest,
) -> Result<String, String> {
    let run = store
        .load(&request.run_id)
        .await
        .map_err(|error| error.to_string())?;
    match request.kind.as_str() {
        "decision" => {
            if run.state.status != RunStatus::AwaitingApproval {
                return Ok("skipped: the run is not waiting for approval".to_string());
            }
            let approved: HashMap<String, bool> = request
                .payload
                .get("approve")
                .and_then(Value::as_array)
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| id.as_str().map(|id| (id.to_string(), true)))
                        .collect()
                })
                .unwrap_or_default();
            let agent = build_agent(settings, &run).await?;
            let run = Runner::new(&agent, store)
                .decide(&run.id, &Decision::PerCall(approved))
                .await
                .map_err(|error| error.to_string())?;
            Ok(run.state.status.as_str().to_string())
        }
        "budget" => {
            if run.state.status != RunStatus::AwaitingBudget {
                return Ok("skipped: the run is not waiting for budget".to_string());
            }
            let budget = request
                .payload
                .get("budget_micros")
                .and_then(Value::as_u64)
                .ok_or("the request has no valid budget")?;
            if budget <= run.budget_micros {
                return Err("the new budget must be higher than the current one".to_string());
            }
            let run = store
                .set_budget(run, budget)
                .await
                .map_err(|error| error.to_string())?;
            // Issuing the token again raises the task limit in the gateway and
            // keeps what was already spent.
            let agent = build_agent(settings, &run).await?;
            let state = agent
                .record_budget_increase(run.state.clone())
                .map_err(|error| error.to_string())?;
            let run = store
                .save(run, state)
                .await
                .map_err(|error| error.to_string())?;
            let run = Runner::new(&agent, store)
                .drive(run)
                .await
                .map_err(|error| error.to_string())?;
            Ok(run.state.status.as_str().to_string())
        }
        "cancel" => {
            if !matches!(
                run.state.status,
                RunStatus::AwaitingApproval | RunStatus::AwaitingBudget
            ) {
                return Ok("skipped: only waiting runs can be cancelled".to_string());
            }
            let mut state = run.state.clone();
            state.status = RunStatus::Failed;
            state.error = Some(format!("cancelled by a person (request {})", request.id));
            state.pending.clear();
            let run = store
                .save(run, state)
                .await
                .map_err(|error| error.to_string())?;
            Ok(run.state.status.as_str().to_string())
        }
        other => Err(format!("unknown request kind: {other}")),
    }
}
