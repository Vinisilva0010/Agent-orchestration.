//! bench: measures billed cost per solved task against a running gateway.
//!
//!   bench [--tier small|standard|large] [--trials N] [--pace-ms N]
//!         [--budget-micros N] [--task ID]...
//!
//! Reads GATEWAY_ISSUER_SECRET and ENGINE_GATEWAY_URL (default
//! http://127.0.0.1:8081). Prints one JSON line per trial, then a summary.

use std::{process::ExitCode, time::Duration};

use bench::{
    harness::{BenchConfig, run_all},
    metrics::{Summary, TrialResult, summarize},
    tasks::{self, Scenario},
};
use engine::secrets_guard;
use protocol::ModelTier;
use secrecy::SecretString;

const USAGE: &str = "usage: bench [--tier small|standard|large] [--trials N] [--pace-ms N] \
[--budget-micros N] [--scenario base|knowledge] [--task ID]...";

fn usd(micros: u64) -> String {
    format!("{}.{:06}", micros / 1_000_000, micros % 1_000_000)
}

fn print_summary(label: &str, summary: &Summary, k: u32) {
    let pass = summary
        .pass_hat_k
        .map_or_else(|| "-".to_string(), |p| format!("{:.0}%", p * 100.0));
    let per_success = summary
        .cost_per_success_micros
        .map_or_else(|| "-".to_string(), usd);
    println!(
        "{label:<24} {:>3}/{:<3} {:>6.0}%  pass^{k} {pass:>5}  total ${}  per solved ${per_success}  cache hit {:.0}%",
        summary.successes,
        summary.trials,
        summary.success_rate * 100.0,
        usd(summary.total_cost_micros),
        summary.cache_hit_ratio * 100.0,
    );
}

#[tokio::main]
async fn main() -> ExitCode {
    let mut tier = ModelTier::Small;
    let mut trials = 3u32;
    let mut pace_ms = 0u64;
    let mut budget = 200_000u64;
    let mut only: Vec<String> = Vec::new();
    let mut scenario = Scenario::Base;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next();
        let parsed = match (arg.as_str(), value.as_deref()) {
            ("--tier", Some("small")) => {
                tier = ModelTier::Small;
                true
            }
            ("--tier", Some("standard")) => {
                tier = ModelTier::Standard;
                true
            }
            ("--tier", Some("large")) => {
                tier = ModelTier::Large;
                true
            }
            ("--trials", Some(v)) => v.parse().map(|n: u32| trials = n.max(1)).is_ok(),
            ("--pace-ms", Some(v)) => v.parse().map(|n| pace_ms = n).is_ok(),
            ("--budget-micros", Some(v)) => v.parse().map(|n: u64| budget = n.max(1)).is_ok(),
            ("--scenario", Some("base")) => {
                scenario = Scenario::Base;
                true
            }
            ("--scenario", Some("knowledge")) => {
                scenario = Scenario::Knowledge;
                true
            }
            ("--task", Some(v)) => {
                only.push(v.to_string());
                true
            }
            _ => false,
        };
        if !parsed {
            eprintln!("{USAGE}");
            return ExitCode::from(64);
        }
    }

    if let Err(error) = secrets_guard::check_environment() {
        eprintln!("{error}");
        return ExitCode::from(4);
    }
    let Some(issuer_secret) = std::env::var("GATEWAY_ISSUER_SECRET")
        .ok()
        .filter(|s| !s.is_empty())
    else {
        eprintln!("GATEWAY_ISSUER_SECRET is not set");
        return ExitCode::from(4);
    };
    let config = BenchConfig {
        gateway_url: std::env::var("ENGINE_GATEWAY_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8081".to_string()),
        issuer_secret: SecretString::from(issuer_secret),
        tier,
        budget_micros: budget,
        scenario,
    };

    let selected: Vec<_> = tasks::all(scenario)
        .into_iter()
        .filter(|task| only.is_empty() || only.iter().any(|id| id == task.id))
        .collect();
    if selected.is_empty() {
        eprintln!("no task matches --task");
        return ExitCode::from(64);
    }

    let run = run_all(
        &config,
        &selected,
        trials,
        Duration::from_millis(pace_ms),
        Duration::from_secs(65),
        |result| println!("{}", serde_json::to_string(result).unwrap_or_default()),
    )
    .await;
    let results = run.results;

    println!();
    println!("task                     solved     rate  reliability   cost");
    for task in &selected {
        let rows: Vec<TrialResult> = results
            .iter()
            .filter(|r| r.task == task.id)
            .cloned()
            .collect();
        print_summary(task.id, &summarize(&rows, trials), trials);
    }
    print_summary("ALL", &summarize(&results, trials), trials);
    if !run.discarded.is_empty() {
        let cost: u64 = run.discarded.iter().map(|r| r.cost_micros).sum();
        println!(
            "rate limited and repeated: {} trials, ${} not counted above",
            run.discarded.len(),
            usd(cost)
        );
    }
    ExitCode::SUCCESS
}
