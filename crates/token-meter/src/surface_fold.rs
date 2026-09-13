//! Positional priced-surface fold used by measurements and compaction plans.

use seekdeep_core::session::{SessionEvent, SurfaceOp, derive_event_message};

use crate::{estimate::estimate_message, types::TokenSurfaceNode};

/// One surface event's atomic placement and price.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceTokenFold {
    /// Price of the event's own projected message.
    pub tokens: u64,
    /// Detached next surface.
    pub nodes: Vec<TokenSurfaceNode>,
    /// Signed total movement.
    pub delta_tokens: i64,
}

/// Folds one surface event onto a priced positional surface.
///
/// # Errors
///
/// Returns a source-compatible diagnostic for an invalid replacement range or
/// a non-canonical surface marker.
pub fn fold_surface_tokens(
    nodes: &[TokenSurfaceNode],
    event: &SessionEvent,
) -> anyhow::Result<SurfaceTokenFold> {
    let tokens = derive_event_message(event)
        .as_ref()
        .map_or(0, estimate_message);
    match event.surface_op.as_ref() {
        Some(SurfaceOp::Marker(marker)) if marker == "append" => {
            let mut next = nodes.to_vec();
            next.push(TokenSurfaceNode {
                seq: event.seq,
                tokens,
            });
            Ok(SurfaceTokenFold {
                tokens,
                nodes: next,
                delta_tokens: to_i64(tokens)?,
            })
        }
        Some(SurfaceOp::Replace(replacement)) => {
            let start = nodes.iter().position(|node| node.seq == replacement.start);
            let end = nodes.iter().position(|node| node.seq == replacement.end);
            let (Some(start), Some(end)) = (start, end) else {
                anyhow::bail!(
                    "token surface: replace at seq {} has invalid current range {}-{}",
                    event.seq,
                    replacement.start,
                    replacement.end
                );
            };
            anyhow::ensure!(
                start <= end,
                "token surface: replace at seq {} has invalid current range {}-{}",
                event.seq,
                replacement.start,
                replacement.end
            );
            let removed = nodes[start..=end]
                .iter()
                .try_fold(0_u64, |total, node| total.checked_add(node.tokens))
                .ok_or_else(|| anyhow::anyhow!("token surface price exceeds u64"))?;
            let mut next = nodes.to_vec();
            next.splice(
                start..=end,
                [TokenSurfaceNode {
                    seq: event.seq,
                    tokens,
                }],
            );
            Ok(SurfaceTokenFold {
                tokens,
                nodes: next,
                delta_tokens: signed_difference(tokens, removed)?,
            })
        }
        _ => anyhow::bail!(
            "token surface: event at seq {} has no valid surface operation",
            event.seq
        ),
    }
}

fn to_i64(value: u64) -> anyhow::Result<i64> {
    i64::try_from(value).map_err(|_| anyhow::anyhow!("token surface price exceeds i64"))
}

pub(crate) fn signed_difference(next: u64, previous: u64) -> anyhow::Result<i64> {
    let difference = i128::from(next) - i128::from(previous);
    i64::try_from(difference).map_err(|_| anyhow::anyhow!("token surface delta exceeds i64"))
}
