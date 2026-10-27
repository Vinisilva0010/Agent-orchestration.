//! Benchmark metrics: success rate, pass^k and billed cost per solved task.

use std::collections::BTreeMap;

use engine::agent::RunStatus;
use protocol::Usage;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TrialResult {
    pub task: String,
    pub trial: u32,
    pub success: bool,
    pub status: RunStatus,
    pub steps: u32,
    pub cost_micros: u64,
    pub usage: Usage,
    pub error: Option<String>,
    /// How the final desk differs from the expected desk, when it does.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mismatches: Vec<String>,
}

fn choose(n: u32, k: u32) -> f64 {
    (0..k).fold(1.0, |acc, i| acc * f64::from(n - i) / f64::from(i + 1))
}

/// Probability that k trials drawn from n, of which c succeeded, all succeed
/// (the tau-bench pass^k estimator). None when k is 0 or larger than n.
pub fn pass_hat_k(n: u32, c: u32, k: u32) -> Option<f64> {
    if k == 0 || k > n || c > n {
        return None;
    }
    if c < k {
        return Some(0.0);
    }
    Some(choose(c, k) / choose(n, k))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    pub trials: u32,
    pub successes: u32,
    pub success_rate: f64,
    /// Mean pass^k over tasks with at least k trials.
    pub pass_hat_k: Option<f64>,
    pub total_cost_micros: u64,
    /// Total billed cost divided by solved trials, so failed attempts count.
    pub cost_per_success_micros: Option<u64>,
    pub usage: Usage,
    /// Share of prompt tokens served from the provider cache.
    pub cache_hit_ratio: f64,
}

pub fn summarize(results: &[TrialResult], k: u32) -> Summary {
    let trials = u32::try_from(results.len()).unwrap_or(u32::MAX);
    let successes = u32::try_from(results.iter().filter(|r| r.success).count()).unwrap_or(u32::MAX);
    let total_cost_micros = results.iter().map(|r| r.cost_micros).sum::<u64>();
    let mut usage = Usage::default();
    for result in results {
        usage.input_tokens += result.usage.input_tokens;
        usage.output_tokens += result.usage.output_tokens;
        usage.cache_read_tokens += result.usage.cache_read_tokens;
        usage.cache_write_tokens += result.usage.cache_write_tokens;
    }
    let prompt = usage.input_tokens + usage.cache_read_tokens + usage.cache_write_tokens;

    let mut by_task: BTreeMap<&str, (u32, u32)> = BTreeMap::new();
    for result in results {
        let entry = by_task.entry(result.task.as_str()).or_default();
        entry.0 += 1;
        entry.1 += u32::from(result.success);
    }
    let per_task: Vec<f64> = by_task
        .values()
        .filter_map(|(n, c)| pass_hat_k(*n, *c, k))
        .collect();

    Summary {
        trials,
        successes,
        success_rate: if trials == 0 {
            0.0
        } else {
            f64::from(successes) / f64::from(trials)
        },
        pass_hat_k: if per_task.is_empty() {
            None
        } else {
            Some(per_task.iter().sum::<f64>() / per_task.len() as f64)
        },
        total_cost_micros,
        cost_per_success_micros: (successes > 0)
            .then(|| total_cost_micros.div_ceil(u64::from(successes))),
        usage,
        cache_hit_ratio: if prompt == 0 {
            0.0
        } else {
            usage.cache_read_tokens as f64 / prompt as f64
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(task: &str, success: bool, cost: u64) -> TrialResult {
        TrialResult {
            task: task.to_string(),
            trial: 0,
            success,
            status: RunStatus::Completed,
            steps: 1,
            cost_micros: cost,
            usage: Usage {
                input_tokens: 100,
                output_tokens: 10,
                cache_read_tokens: 300,
                cache_write_tokens: 100,
            },
            error: None,
            mismatches: Vec::new(),
        }
    }

    #[test]
    fn pass_hat_k_matches_the_estimator() {
        assert_eq!(pass_hat_k(3, 3, 3), Some(1.0));
        assert_eq!(pass_hat_k(3, 2, 3), Some(0.0));
        assert!((pass_hat_k(4, 2, 2).unwrap() - 1.0 / 6.0).abs() < 1e-12);
        assert!((pass_hat_k(4, 3, 1).unwrap() - 0.75).abs() < 1e-12);
        assert_eq!(pass_hat_k(2, 2, 3), None);
        assert_eq!(pass_hat_k(2, 2, 0), None);
    }

    #[test]
    fn failed_attempts_count_in_cost_per_success() {
        let results = [
            result("a", true, 100),
            result("a", false, 50),
            result("b", true, 51),
        ];
        let summary = summarize(&results, 1);
        assert_eq!(summary.successes, 2);
        assert_eq!(summary.total_cost_micros, 201);
        assert_eq!(summary.cost_per_success_micros, Some(101));
        assert!((summary.cache_hit_ratio - 0.6).abs() < 1e-12);
        assert!((summary.pass_hat_k.unwrap() - 0.75).abs() < 1e-12);
    }

    #[test]
    fn no_success_means_no_cost_per_success() {
        let summary = summarize(&[result("a", false, 10)], 1);
        assert_eq!(summary.cost_per_success_micros, None);
    }
}
