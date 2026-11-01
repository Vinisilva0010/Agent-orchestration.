//! Following tasks: an agent's latest runs and queued requests, and one run
//! in detail (conversation, tool activity, actions waiting for a person,
//! spend and result).
//!
//! Visibility follows the agent list: someone who may not see the agent gets
//! "not found" for its runs too. Everything comes from the run's saved state
//! and the execution log; the server never changes a run here. Text written
//! by models or tools is returned as plain strings, for the page to show as
//! text.

use std::collections::HashMap;

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use serde::Serialize;
use serde_json::Value;

use crate::{
    http::{ApiError, AppState, CurrentUser},
    tasks::{Access, agent_access},
};

/// Runs listed per agent, newest first.
const LIST_LIMIT: i64 = 50;
/// Characters of the task text in lists.
const LIST_TASK_CHARS: i32 = 300;
/// Characters of one tool result in the activity.
const RESULT_CHARS: usize = 2000;
/// Characters of the model's explanation next to an approval.
const REASON_CHARS: usize = 4000;

/// RFC 3339 in UTC, as text.
macro_rules! utc {
    ($column:literal) => {
        concat!(
            "to_char(",
            $column,
            " AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')"
        )
    };
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/agents/{agent_id}/runs", get(list_runs))
        .route("/api/runs/{id}", get(run_detail))
}

pub(crate) fn valid_run_id(id: &str) -> bool {
    id.len() == 36
        && id.starts_with("run_")
        && id[4..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn truncate(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_string(),
    }
}

/// One line of an agent's task list: a run, or a request still in the queue.
#[derive(Serialize)]
struct RunItem {
    run_id: Option<String>,
    /// Set for requests made from the workspace.
    task_start_id: Option<i64>,
    /// The run's status, or "queued" for a request not started yet.
    status: String,
    task: String,
    requested_by: Option<String>,
    budget_micros: i64,
    cost_micros: i64,
    created_at: String,
    updated_at: String,
}

type RunRow = (
    String,
    String,
    String,
    Option<String>,
    i64,
    i64,
    String,
    String,
    Option<i64>,
);

async fn list_runs(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(agent_id): Path<String>,
) -> Result<Json<Vec<RunItem>>, ApiError> {
    if agent_access(&state, &user, &agent_id).await?.access == Access::Hidden {
        return Err(ApiError::not_found());
    }
    let queued: Vec<(i64, String, String, i64, String)> = sqlx::query_as(concat!(
        "SELECT t.id, left(t.task, $2), u.display_name, t.budget_micros, ",
        utc!("t.created_at"),
        " FROM task_starts t JOIN users u ON u.id = t.requested_by \
         WHERE t.agent_id = $1 AND t.processed_at IS NULL ORDER BY t.id LIMIT $3"
    ))
    .bind(&agent_id)
    .bind(LIST_TASK_CHARS)
    .bind(LIST_LIMIT)
    .fetch_all(&state.pool)
    .await?;
    let runs: Vec<RunRow> = sqlx::query_as(concat!(
        "SELECT r.id, r.status, \
                left(coalesce(t.task, r.state #>> '{messages,0,content,0,text}', ''), $2), \
                u.display_name, r.budget_micros, \
                coalesce((r.state->>'cost_micros')::bigint, 0), ",
        utc!("r.created_at"),
        ", ",
        utc!("r.updated_at"),
        ", t.id \
         FROM engine_runs r \
         LEFT JOIN task_starts t ON t.run_id = r.id \
         LEFT JOIN users u ON u.id = t.requested_by \
         WHERE r.agent_id = $1 ORDER BY r.created_at DESC, r.id LIMIT $3"
    ))
    .bind(&agent_id)
    .bind(LIST_TASK_CHARS)
    .bind(LIST_LIMIT)
    .fetch_all(&state.pool)
    .await?;

    let mut items: Vec<RunItem> = queued
        .into_iter()
        .map(
            |(id, task, requested_by, budget_micros, created_at)| RunItem {
                run_id: None,
                task_start_id: Some(id),
                status: "queued".to_string(),
                task,
                requested_by: Some(requested_by),
                budget_micros,
                cost_micros: 0,
                updated_at: created_at.clone(),
                created_at,
            },
        )
        .collect();
    items.extend(runs.into_iter().map(
        |(
            id,
            status,
            task,
            requested_by,
            budget_micros,
            cost_micros,
            created_at,
            updated_at,
            task_start_id,
        )| RunItem {
            run_id: Some(id),
            task_start_id,
            status,
            task,
            requested_by,
            budget_micros,
            cost_micros,
            created_at,
            updated_at,
        },
    ));
    Ok(Json(items))
}

/// A part of the conversation, as people read it.
#[derive(Serialize, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    /// "person" for the task and other people's text, "agent" for the model.
    role: &'static str,
    text: String,
    /// Tools the agent called in this turn, by name.
    tools: Vec<String>,
}

/// One tool call and what became of it.
#[derive(Serialize, Debug, PartialEq, Eq)]
pub(crate) struct Activity {
    call_id: String,
    tool: String,
    /// done, error, rejected, waiting, started (outcome unknown) or not_run.
    outcome: &'static str,
    result: Option<String>,
    irreversible: bool,
    started_at: Option<String>,
    finished_at: Option<String>,
}

/// An action waiting for a person's decision.
#[derive(Serialize, Debug, PartialEq)]
pub(crate) struct Approval {
    call_id: String,
    tool: String,
    /// The tool's description, written by its developers.
    description: String,
    input: Value,
    irreversible: bool,
    /// What the model wrote in the turn that asked for the action. Untrusted.
    reason: String,
}

fn str_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn blocks(message: &Value) -> &[Value] {
    message
        .get("content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn text_of(message: &Value) -> String {
    blocks(message)
        .iter()
        .filter(|block| str_field(block, "type") == "text")
        .map(|block| str_field(block, "text"))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn messages(state: &Value) -> &[Value] {
    state
        .get("messages")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// The conversation: message text, with tool calls summarized by name.
/// Reasoning blocks and tool results are left out (results are in the
/// activity).
pub(crate) fn conversation(state: &Value) -> Vec<Entry> {
    messages(state)
        .iter()
        .filter_map(|message| {
            let role = match str_field(message, "role") {
                "assistant" => "agent",
                _ => "person",
            };
            let text = text_of(message);
            let tools: Vec<String> = blocks(message)
                .iter()
                .filter(|block| str_field(block, "type") == "tool_use")
                .map(|block| str_field(block, "name").to_string())
                .collect();
            (!text.is_empty() || !tools.is_empty()).then_some(Entry { role, text, tools })
        })
        .collect()
}

/// Approval records the engine keeps in the state: call id -> (tool's
/// description, irreversible, decision).
/// Description, irreversible, decision.
type Record = (String, bool, Option<bool>);

fn approval_records(state: &Value) -> Option<HashMap<String, Record>> {
    let records = state.get("approvals")?.as_array()?;
    Some(
        records
            .iter()
            .map(|record| {
                (
                    str_field(record, "call_id").to_string(),
                    (
                        str_field(record, "description").to_string(),
                        record
                            .get("irreversible")
                            .and_then(Value::as_bool)
                            .unwrap_or(true),
                        record.get("approved").and_then(Value::as_bool),
                    ),
                )
            })
            .collect(),
    )
}

/// Actions the run waits on. Runs saved before the engine kept approval
/// records treat every waiting call as irreversible, the safe reading.
pub(crate) fn waiting_approvals(status: &str, state: &Value) -> Vec<Approval> {
    if status != "awaiting_approval" {
        return Vec::new();
    }
    let reason = messages(state)
        .iter()
        .rev()
        .find(|message| str_field(message, "role") == "assistant")
        .map(text_of)
        .map(|text| truncate(&text, REASON_CHARS))
        .unwrap_or_default();
    let records = approval_records(state);
    let pending = state
        .get("pending")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    pending
        .iter()
        .filter_map(|call| {
            let call_id = str_field(call, "id").to_string();
            let (description, irreversible) = match &records {
                None => (String::new(), true),
                Some(records) => match records.get(&call_id) {
                    Some((description, irreversible, None)) => (description.clone(), *irreversible),
                    // Decided already, or a call that needs no approval.
                    _ => return None,
                },
            };
            Some(Approval {
                tool: str_field(call, "name").to_string(),
                input: call.get("input").cloned().unwrap_or(Value::Null),
                call_id,
                description,
                irreversible,
                reason: reason.clone(),
            })
        })
        .collect()
}

struct Execution {
    state: String,
    started_at: String,
    finished_at: Option<String>,
}

/// Every tool call of the run, in order, with its outcome.
fn activity(status: &str, state: &Value, executions: &HashMap<String, Execution>) -> Vec<Activity> {
    let records = approval_records(state).unwrap_or_default();
    let mut results: HashMap<&str, (&str, bool)> = HashMap::new();
    for message in messages(state) {
        for block in blocks(message) {
            if str_field(block, "type") == "tool_result" {
                results.insert(
                    str_field(block, "tool_use_id"),
                    (
                        str_field(block, "content"),
                        block
                            .get("is_error")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    ),
                );
            }
        }
    }
    let waiting: Vec<&str> = if status == "awaiting_approval" {
        state
            .get("pending")
            .and_then(Value::as_array)
            .map(|calls| calls.iter().map(|call| str_field(call, "id")).collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut items = Vec::new();
    for message in messages(state) {
        if str_field(message, "role") != "assistant" {
            continue;
        }
        for block in blocks(message) {
            if str_field(block, "type") != "tool_use" {
                continue;
            }
            let call_id = str_field(block, "id");
            let record = records.get(call_id);
            let execution = executions.get(call_id);
            let result = results.get(call_id);
            let outcome = match (record, result, execution) {
                (Some((_, _, Some(false))), _, _) => "rejected",
                (_, Some((_, true)), _) => "error",
                (_, Some((_, false)), _) => "done",
                _ if waiting.contains(&call_id) => "waiting",
                (_, None, Some(execution)) if execution.state == "started" => "started",
                _ => "not_run",
            };
            items.push(Activity {
                call_id: call_id.to_string(),
                tool: str_field(block, "name").to_string(),
                outcome,
                result: match outcome {
                    "done" | "error" => result.map(|(content, _)| truncate(content, RESULT_CHARS)),
                    _ => None,
                },
                irreversible: record.is_some_and(|(_, irreversible, _)| *irreversible)
                    || execution.is_some(),
                started_at: execution.map(|execution| execution.started_at.clone()),
                finished_at: execution.and_then(|execution| execution.finished_at.clone()),
            });
        }
    }
    items
}

#[derive(Serialize)]
struct RunDetail {
    id: String,
    agent_id: String,
    agent_name: Option<String>,
    status: String,
    task: String,
    requested_by: Option<String>,
    budget_micros: i64,
    cost_micros: i64,
    created_at: String,
    updated_at: String,
    final_text: String,
    error: Option<String>,
    conversation: Vec<Entry>,
    activity: Vec<Activity>,
    approvals: Vec<Approval>,
    can_decide: bool,
    can_cancel: bool,
    can_raise_budget: bool,
}

type DetailRow = (
    String,
    Option<String>,
    String,
    i64,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
);

async fn run_detail(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(run_id): Path<String>,
) -> Result<Json<RunDetail>, ApiError> {
    if !valid_run_id(&run_id) {
        return Err(ApiError::not_found());
    }
    let row: Option<DetailRow> = sqlx::query_as(concat!(
        "SELECT r.agent_id, a.display_name, r.status, r.budget_micros, r.state::text, ",
        utc!("r.created_at"),
        ", ",
        utc!("r.updated_at"),
        ", t.task, u.display_name \
         FROM engine_runs r \
         LEFT JOIN agents a ON a.id = r.agent_id \
         LEFT JOIN task_starts t ON t.run_id = r.id \
         LEFT JOIN users u ON u.id = t.requested_by \
         WHERE r.id = $1"
    ))
    .bind(&run_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some((
        agent_id,
        agent_name,
        status,
        budget_micros,
        run_state,
        created_at,
        updated_at,
        task,
        requested_by,
    )) = row
    else {
        return Err(ApiError::not_found());
    };
    // Administrators also see runs of agents that were never registered
    // (started from the engine's command line).
    let access = if user.role == "admin" {
        Access::Manage
    } else {
        agent_access(&state, &user, &agent_id).await?.access
    };
    if access == Access::Hidden {
        return Err(ApiError::not_found());
    }
    let run_state: Value = serde_json::from_str(&run_state).unwrap_or(Value::Null);
    let executions: Vec<(String, String, String, Option<String>)> = sqlx::query_as(concat!(
        "SELECT call_id, state, ",
        utc!("started_at"),
        ", CASE WHEN finished_at IS NULL THEN NULL ELSE ",
        utc!("finished_at"),
        " END FROM engine_tool_executions WHERE run_id = $1"
    ))
    .bind(&run_id)
    .fetch_all(&state.pool)
    .await?;
    let executions: HashMap<String, Execution> = executions
        .into_iter()
        .map(|(call_id, state, started_at, finished_at)| {
            (
                call_id,
                Execution {
                    state,
                    started_at,
                    finished_at,
                },
            )
        })
        .collect();

    let waiting = matches!(status.as_str(), "awaiting_approval" | "awaiting_budget");
    let task = task.unwrap_or_else(|| {
        messages(&run_state)
            .first()
            .map(text_of)
            .unwrap_or_default()
    });
    Ok(Json(RunDetail {
        conversation: conversation(&run_state),
        activity: activity(&status, &run_state, &executions),
        approvals: waiting_approvals(&status, &run_state),
        can_decide: status == "awaiting_approval" && matches!(access, Access::Use | Access::Manage),
        can_cancel: waiting && access == Access::Manage,
        can_raise_budget: status == "awaiting_budget" && user.role == "admin",
        cost_micros: run_state
            .get("cost_micros")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        final_text: str_field(&run_state, "final_text").to_string(),
        error: run_state
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_string),
        id: run_id,
        agent_id,
        agent_name,
        status,
        task,
        requested_by,
        budget_micros,
        created_at,
        updated_at,
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn run_ids_are_checked_before_any_query() {
        assert!(valid_run_id("run_0123456789abcdef0123456789abcdef"));
        for bad in [
            "run_0123456789ABCDEF0123456789abcdef",
            "run_0123",
            "../etc/passwd",
            "run_0123456789abcdef0123456789abcdeg",
            "",
        ] {
            assert!(!valid_run_id(bad), "{bad}");
        }
    }

    #[test]
    fn truncation_counts_characters() {
        assert_eq!(truncate("ação", 10), "ação");
        assert_eq!(truncate("ação", 2), "aç…");
    }

    #[test]
    fn old_runs_treat_every_waiting_call_as_irreversible() {
        let state = json!({
            "messages": [{ "role": "assistant", "content": [{ "type": "text", "text": "why" }] }],
            "pending": [{ "id": "c1", "name": "archive_report", "input": { "report_id": "q3" } }],
        });
        let approvals = waiting_approvals("awaiting_approval", &state);
        assert_eq!(approvals.len(), 1);
        assert!(approvals[0].irreversible);
        assert_eq!(approvals[0].reason, "why");
        assert!(waiting_approvals("completed", &state).is_empty());
    }
}
