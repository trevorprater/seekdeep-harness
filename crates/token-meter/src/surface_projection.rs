//! O(1) surface repricing through adjacent compaction shadow claims.

use seekdeep_core::session::{SessionEvent, SurfaceOp, derive_event_message, is_surface_event};
use serde::{Deserialize, Serialize};

use crate::{estimate::estimate_message, surface_fold::signed_difference};

/// Price of the exact range that the immediately following event replaces.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShadowPriceClaim {
    /// Inclusive first current surface sequence.
    pub start: u64,
    /// Inclusive last current surface sequence.
    pub end: u64,
    /// Fixed-heuristic price of that range.
    pub tokens: u64,
}

/// One event's bounded-state surface effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceTokensFold {
    /// Signed total movement, or zero off-surface.
    pub delta_tokens: i64,
    /// Claim surviving into exactly the next event.
    pub claim: Option<ShadowPriceClaim>,
}

/// Applies one committed event to the bounded shadow-price fold.
///
/// # Errors
///
/// Returns when an adjacent claim names a different replacement range.
pub fn fold_surface_projection(
    claim: Option<&ShadowPriceClaim>,
    event: &SessionEvent,
) -> anyhow::Result<SurfaceTokensFold> {
    if matches!(
        event.event_type.as_str(),
        "compaction/summary" | "compaction/prune"
    ) {
        let start = required_u64(&event.data, "/shadowedRange/start", "shadowedRange.start")?;
        let end = required_u64(&event.data, "/shadowedRange/end", "shadowedRange.end")?;
        let tokens = required_u64(&event.data, "/shadowedTokenCount", "shadowedTokenCount")?;
        return Ok(SurfaceTokensFold {
            delta_tokens: 0,
            claim: Some(ShadowPriceClaim { start, end, tokens }),
        });
    }
    if !is_surface_event(event) {
        return Ok(SurfaceTokensFold {
            delta_tokens: 0,
            claim: None,
        });
    }
    let tokens = derive_event_message(event)
        .as_ref()
        .map_or(0, estimate_message);
    match event.surface_op.as_ref() {
        Some(SurfaceOp::Marker(marker)) if marker == "append" => Ok(SurfaceTokensFold {
            delta_tokens: i64::try_from(tokens)
                .map_err(|_| anyhow::anyhow!("token surface price exceeds i64"))?,
            claim: None,
        }),
        Some(SurfaceOp::Replace(replacement)) => {
            let Some(claim) = claim else {
                return Ok(SurfaceTokensFold {
                    delta_tokens: 0,
                    claim: None,
                });
            };
            anyhow::ensure!(
                claim.start == replacement.start && claim.end == replacement.end,
                "token surface: replace at seq {} over range {}-{} has no adjacent shadow price (armed claim covers {}-{})",
                event.seq,
                replacement.start,
                replacement.end,
                claim.start,
                claim.end
            );
            Ok(SurfaceTokensFold {
                delta_tokens: signed_difference(tokens, claim.tokens)?,
                claim: None,
            })
        }
        _ => Ok(SurfaceTokensFold {
            delta_tokens: 0,
            claim: None,
        }),
    }
}

fn required_u64(value: &serde_json::Value, pointer: &str, field: &str) -> anyhow::Result<u64> {
    value
        .pointer(pointer)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("token surface: {field} must be a non-negative integer"))
}
