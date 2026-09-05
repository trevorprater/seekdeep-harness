//! Pure durable provider-usage and approximate context-pressure folds.

use seekdeep_core::session::SessionEvent;
use seekdeep_llm::TokenUsage;
use seekdeep_session_projection::{ProjectionDefinition, ProjectionTransition};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    projection::{ContextPressureProjection, TokenUsageProjection},
    surface_projection::{ShadowPriceClaim, fold_surface_projection},
};

/// Provider-usage projection key.
pub const TOKEN_USAGE_KEY: &str = "tokenUsage";
/// Context-pressure projection key.
pub const CONTEXT_PRESSURE_KEY: &str = "contextPressure";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UsageSample {
    turn: u64,
    step: u64,
    buckets: TokenUsageProjection,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TokenUsageState {
    totals: TokenUsageProjection,
    last: Option<UsageSample>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ContextPressureState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pressure_tokens: Option<u64>,
    surface_tokens: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sampled_surface_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claim: Option<ShadowPriceClaim>,
}

/// Builds the cumulative `tokenUsage` definition.
#[must_use]
pub fn token_usage_definition() -> ProjectionDefinition {
    ProjectionDefinition::new(
        TOKEN_USAGE_KEY,
        1,
        || Ok(serde_json::to_value(TokenUsageState::default())?),
        apply_usage,
        |state| {
            let state: TokenUsageState = serde_json::from_value(state.clone())?;
            Ok(serde_json::to_value(state.totals)?)
        },
    )
}

/// Builds the last-wins `contextPressure` definition.
#[must_use]
pub fn context_pressure_definition() -> ProjectionDefinition {
    ProjectionDefinition::new(
        CONTEXT_PRESSURE_KEY,
        4,
        || Ok(serde_json::to_value(ContextPressureState::default())?),
        apply_pressure,
        pressure_view,
    )
}

fn apply_usage(state: &Value, event: &SessionEvent) -> anyhow::Result<ProjectionTransition> {
    let state: TokenUsageState = serde_json::from_value(state.clone())?;
    let Some((turn, step, usage)) = usage_sample(event)? else {
        return Ok(ProjectionTransition::Unchanged);
    };
    let buckets = buckets_from(&usage);
    let previous = state
        .last
        .as_ref()
        .filter(|sample| sample.turn == turn && sample.step == step)
        .map(|sample| &sample.buckets);
    if previous == Some(&buckets) {
        return Ok(ProjectionTransition::Unchanged);
    }
    let next = TokenUsageState {
        totals: add_replacing(&state.totals, previous, &buckets)?,
        last: Some(UsageSample {
            turn,
            step,
            buckets,
        }),
    };
    ProjectionTransition::changed(next)
}

fn apply_pressure(state: &Value, event: &SessionEvent) -> anyhow::Result<ProjectionTransition> {
    let state: ContextPressureState = serde_json::from_value(state.clone())?;
    let fold = fold_surface_projection(state.claim.as_ref(), event)?;
    let mut next = state.clone();
    if event.event_type == "request/context" {
        next.context_window = optional_u64(&event.data, "contextWindow")?;
    }
    if let Some((_, _, usage)) = usage_sample(event)? {
        next.pressure_tokens = Some(pressure_from(&usage)?);
        next.sampled_surface_tokens = Some(next.surface_tokens);
    }
    next.surface_tokens = next
        .surface_tokens
        .checked_add(fold.delta_tokens)
        .ok_or_else(|| anyhow::anyhow!("contextPressure surface token total overflowed"))?;
    next.claim = fold.claim;
    if next == state {
        Ok(ProjectionTransition::Unchanged)
    } else {
        ProjectionTransition::changed(next)
    }
}

fn pressure_view(state: &Value) -> anyhow::Result<Value> {
    let state: ContextPressureState = serde_json::from_value(state.clone())?;
    anyhow::ensure!(
        state.context_window.is_none_or(|window| window > 0),
        "contextPressure contextWindow must be positive when present"
    );
    let projected_tokens = match (state.pressure_tokens, state.sampled_surface_tokens) {
        (Some(pressure), Some(sampled)) => {
            let projected =
                i128::from(pressure) + i128::from(state.surface_tokens) - i128::from(sampled);
            Some(if projected <= 0 {
                0
            } else {
                u64::try_from(projected)
                    .map_err(|_| anyhow::anyhow!("contextPressure projectedTokens overflowed"))?
            })
        }
        _ => None,
    };
    Ok(serde_json::to_value(ContextPressureProjection {
        pressure_tokens: state.pressure_tokens,
        projected_tokens,
        context_window: state.context_window,
    })?)
}

fn usage_sample(event: &SessionEvent) -> anyhow::Result<Option<(u64, u64, TokenUsage)>> {
    let usage = if event.event_type == "assistant/chunk"
        && event.data.pointer("/chunk/type").and_then(Value::as_str) == Some("usage")
    {
        event.data.pointer("/chunk/usage")
    } else if event.event_type == "assistant/message" {
        event.data.get("usage")
    } else {
        None
    };
    let Some(usage) = usage else {
        return Ok(None);
    };
    let turn = required_u64(&event.data, "turn")?;
    let step = required_u64(&event.data, "step")?;
    Ok(Some((turn, step, serde_json::from_value(usage.clone())?)))
}

fn buckets_from(usage: &TokenUsage) -> TokenUsageProjection {
    TokenUsageProjection {
        uncached_input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens.unwrap_or(0),
        cache_write_tokens: usage.cache_write_tokens.unwrap_or(0),
    }
}

fn add_replacing(
    totals: &TokenUsageProjection,
    previous: Option<&TokenUsageProjection>,
    next: &TokenUsageProjection,
) -> anyhow::Result<TokenUsageProjection> {
    fn replace(total: u64, previous: u64, next: u64) -> anyhow::Result<u64> {
        total
            .checked_sub(previous)
            .and_then(|value| value.checked_add(next))
            .ok_or_else(|| anyhow::anyhow!("tokenUsage bucket arithmetic overflowed"))
    }
    let previous = previous.cloned().unwrap_or_default();
    Ok(TokenUsageProjection {
        uncached_input_tokens: replace(
            totals.uncached_input_tokens,
            previous.uncached_input_tokens,
            next.uncached_input_tokens,
        )?,
        output_tokens: replace(
            totals.output_tokens,
            previous.output_tokens,
            next.output_tokens,
        )?,
        cache_read_tokens: replace(
            totals.cache_read_tokens,
            previous.cache_read_tokens,
            next.cache_read_tokens,
        )?,
        cache_write_tokens: replace(
            totals.cache_write_tokens,
            previous.cache_write_tokens,
            next.cache_write_tokens,
        )?,
    })
}

fn pressure_from(usage: &TokenUsage) -> anyhow::Result<u64> {
    usage
        .input_tokens
        .checked_add(usage.cache_read_tokens.unwrap_or(0))
        .and_then(|value| value.checked_add(usage.cache_write_tokens.unwrap_or(0)))
        .ok_or_else(|| anyhow::anyhow!("contextPressure prompt token sum overflowed"))
}

fn required_u64(value: &Value, field: &str) -> anyhow::Result<u64> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("token projection {field} must be a non-negative integer"))
}

fn optional_u64(value: &Value, field: &str) -> anyhow::Result<Option<u64>> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            anyhow::anyhow!("token projection {field} must be a non-negative integer")
        }),
    }
}
