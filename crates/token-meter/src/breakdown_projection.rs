//! Pure fixed-heuristic context-composition projection.

use seekdeep_core::{
    request_header::{EpochHeader, canonical_header},
    session::SessionEvent,
};
use seekdeep_session_projection::{ProjectionDefinition, ProjectionTransition};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    estimate::{estimate_recorded_header, estimate_system_tokens, estimate_tools_tokens},
    projection::ContextBreakdownProjection,
    surface_projection::{ShadowPriceClaim, fold_surface_projection},
};

/// Context-composition projection key.
pub const CONTEXT_BREAKDOWN_KEY: &str = "contextBreakdown";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ContextBreakdownState {
    system_tokens: u64,
    tools_tokens: u64,
    message_tokens: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claim: Option<ShadowPriceClaim>,
}

/// Builds the bounded `contextBreakdown` definition.
#[must_use]
pub fn context_breakdown_definition() -> ProjectionDefinition {
    ProjectionDefinition::new(
        CONTEXT_BREAKDOWN_KEY,
        2,
        || Ok(serde_json::to_value(ContextBreakdownState::default())?),
        apply,
        view,
    )
}

fn apply(state: &Value, event: &SessionEvent) -> anyhow::Result<ProjectionTransition> {
    let state: ContextBreakdownState = serde_json::from_value(state.clone())?;
    let fold = fold_surface_projection(state.claim.as_ref(), event)?;
    let mut next = state.clone();
    if event.event_type == "request/header" {
        let recorded = event
            .data
            .get("header")
            .ok_or_else(|| anyhow::anyhow!("request/header lacks header"))?;
        (next.system_tokens, next.tools_tokens) =
            match serde_json::from_value::<EpochHeader>(recorded.clone()) {
                Ok(header) => {
                    let header = canonical_header(header);
                    (
                        estimate_system_tokens(Some(&header)),
                        estimate_tools_tokens(Some(&header)),
                    )
                }
                // Scrubbed recordings carry placeholder strings where the typed header expects
                // a system prompt and a schema list; the source prices them by length.
                Err(_) => estimate_recorded_header(recorded),
            };
    }
    next.message_tokens = next
        .message_tokens
        .checked_add(fold.delta_tokens)
        .ok_or_else(|| anyhow::anyhow!("contextBreakdown message token total overflowed"))?;
    next.claim = fold.claim;
    if next == state {
        Ok(ProjectionTransition::Unchanged)
    } else {
        ProjectionTransition::changed(next)
    }
}

fn view(state: &Value) -> anyhow::Result<Value> {
    let state: ContextBreakdownState = serde_json::from_value(state.clone())?;
    let message_tokens = u64::try_from(state.message_tokens)
        .map_err(|_| anyhow::anyhow!("contextBreakdown messageTokens must be non-negative"))?;
    Ok(serde_json::to_value(ContextBreakdownProjection {
        system_tokens: state.system_tokens,
        tools_tokens: state.tools_tokens,
        message_tokens,
    })?)
}
