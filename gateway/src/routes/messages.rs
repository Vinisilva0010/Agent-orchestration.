use axum::{Extension, Json, extract::State};
use serde::Serialize;

use crate::{
    AppState,
    budget::{CallOutcome, UsageRecord},
    provider::{CompletionRequest, CompletionResponse, Usage},
    routes::error::ApiError,
    tokens::TaskGrant,
};

#[derive(Debug, Serialize)]
pub struct MessageResponse {
    #[serde(flatten)]
    completion: CompletionResponse,
    cost_micros: u64,
    task_budget_remaining_micros: Option<u64>,
}

/// Sends a provider-neutral completion request on behalf of the task identified
/// by the caller's token.
///
/// The highest possible cost is reserved in every spending limit before the
/// provider is called. The input upper bound counts one token per byte of the
/// request, which byte-level tokenizers never exceed. Every call, successful or
/// failed, is recorded in the append-only usage log.
pub async fn create_message(
    State(state): State<AppState>,
    Extension(grant): Extension<TaskGrant>,
    Json(request): Json<CompletionRequest>,
) -> Result<Json<MessageResponse>, ApiError> {
    request.validate()?;

    let price = state.provider.price(request.tier);
    let max_input_tokens = serde_json::to_vec(&request).map_or(u64::MAX, |b| b.len() as u64);
    let reserved = price.max_cost_micros(max_input_tokens, u64::from(request.max_output_tokens));
    let reservation = state.ledger.reserve(&grant, reserved).await?;

    let completion = match state.provider.complete(&request).await {
        Ok(completion) => completion,
        Err(error) => {
            let failed = UsageRecord {
                provider: state.provider.name().to_string(),
                model: request.tier.as_str().to_string(),
                outcome: CallOutcome::Failed,
                usage: Usage::default(),
                cost_micros: 0,
            };
            if reservation.settle(failed).await.is_err() {
                tracing::error!(task_id = %grant.task_id, "could not record a failed call");
            }
            return Err(error.into());
        }
    };

    let cost = price.cost_micros(&completion.usage);
    let record = UsageRecord {
        provider: completion.provider.clone(),
        model: completion.model.clone(),
        outcome: CallOutcome::Completed,
        usage: completion.usage,
        cost_micros: cost,
    };
    // The provider has already done (and charged for) the work, so the result is
    // returned even if recording fails. The failure is logged with the cost so it
    // can be reconciled.
    let remaining = match reservation.settle(record).await {
        Ok(remaining) => remaining,
        Err(_) => {
            tracing::error!(
                task_id = %grant.task_id,
                agent_id = %grant.agent_id,
                cost_micros = cost,
                "could not record spending; reconcile manually"
            );
            None
        }
    };

    tracing::info!(
        task_id = %grant.task_id,
        agent_id = %grant.agent_id,
        provider = %completion.provider,
        model = %completion.model,
        input_tokens = completion.usage.input_tokens,
        output_tokens = completion.usage.output_tokens,
        cache_read_tokens = completion.usage.cache_read_tokens,
        cache_write_tokens = completion.usage.cache_write_tokens,
        reserved_micros = reserved,
        cost_micros = cost,
        "completion"
    );

    Ok(Json(MessageResponse {
        completion,
        cost_micros: cost,
        task_budget_remaining_micros: remaining,
    }))
}
