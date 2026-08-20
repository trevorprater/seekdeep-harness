//! Replay-safe, model-free tool-result pruning service.

use std::sync::Arc;

use seekdeep_cordis::{Context, Plugin, ServiceKey};
use seekdeep_core::session::{AppendOptions, Session, SurfaceOp};
use seekdeep_llm::{ContentBlock, Message};
use seekdeep_schemastery::Schema;
use seekdeep_token_meter::TOKEN_METER;
use serde_json::{Value, json};

use crate::config::{DEFAULTS, PRUNE_MARKER, code_point_length, resolve_config};
use crate::types::{PruneResult, PrunedEntry, ResolvedConfig, ToolResultPruneConfig};

/// Cordis plugin name.
pub const NAME: &str = "compaction-tool-result-pruner";

/// Services required by the tool-result pruner.
pub const INJECT: &[&str] = &["tokenMeter"];

/// Typed Cordis slot corresponding to `ctx.toolResultPruner`.
pub const TOOL_RESULT_PRUNER: ServiceKey<ToolResultPruner> = ServiceKey::new("toolResultPruner");

/// The source-compatible admission schema for [`ToolResultPruneConfig`].
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn config_schema() -> Schema {
    Schema::object([
        (
            "thresholdChars",
            Schema::number()
                .step(1.0)
                .min(1.0)
                .with_default(DEFAULTS.threshold_chars),
        ),
        (
            "headChars",
            Schema::number()
                .step(1.0)
                .min(0.0)
                .with_default(DEFAULTS.head_chars),
        ),
        (
            "tailChars",
            Schema::number()
                .step(1.0)
                .min(0.0)
                .with_default(DEFAULTS.tail_chars),
        ),
    ])
}

/// Deterministic head/middle/tail pruning for current tool-result surface nodes.
#[derive(Clone, Debug)]
pub struct ToolResultPruner {
    /// Resolved and immutable character budgets.
    pub config: ResolvedConfig,
    context: Context,
}

impl ToolResultPruner {
    /// Builds, resolves, and publishes the pruner service.
    ///
    /// # Errors
    ///
    /// Returns invalid-configuration, duplicate-service, or inactive-owner
    /// failures.
    pub fn new(context: &Context, config: &ToolResultPruneConfig) -> anyhow::Result<Arc<Self>> {
        let pruner = Arc::new(Self {
            config: resolve_config(config)?,
            context: context.clone(),
        });
        context.provide(TOOL_RESULT_PRUNER, pruner.clone())?;
        Ok(pruner)
    }

    /// Measures text content in Unicode code points; non-text blocks cost zero.
    #[must_use]
    pub fn measure_content(&self, blocks: &[ContentBlock]) -> usize {
        blocks.iter().fold(0, |chars, block| match block {
            ContentBlock::Text { text } => chars + code_point_length(text),
            _ => chars,
        })
    }

    /// Replaces an over-budget text middle while retaining rich-block order.
    ///
    /// # Panics
    ///
    /// Panics on the two internal invariants the validated configuration
    /// guarantees cannot be violated by a well-formed over-budget input.
    #[must_use]
    pub fn prune_content(&self, blocks: &[ContentBlock]) -> Option<Vec<ContentBlock>> {
        let total_chars = self.measure_content(blocks);
        if total_chars <= self.config.threshold_chars {
            return None;
        }

        let removed_start = self.config.head_chars;
        let removed_end = total_chars - self.config.tail_chars;
        let mut pruned: Vec<ContentBlock> = Vec::new();
        let mut consumed = 0;
        let mut marker_inserted = false;

        for block in blocks {
            let ContentBlock::Text { text } = block else {
                pruned.push(block.clone());
                continue;
            };
            let points: Vec<char> = text.chars().collect();
            let block_start = consumed;
            let block_end = block_start + points.len();
            let head_end = points.len().min(removed_start.saturating_sub(block_start));
            let tail_start = points.len().min(removed_end.saturating_sub(block_start));
            let intersects_removed = block_start < removed_end && block_end > removed_start;
            let marker = if intersects_removed && !marker_inserted {
                PRUNE_MARKER
            } else {
                ""
            };
            if !marker.is_empty() {
                marker_inserted = true;
            }
            let text = format!(
                "{}{}{}",
                points[..head_end].iter().collect::<String>(),
                marker,
                points[tail_start..].iter().collect::<String>()
            );
            if !text.is_empty() {
                pruned.push(ContentBlock::Text { text });
            }
            consumed = block_end;
        }

        assert!(
            marker_inserted,
            "tool-result prune: failed to locate the removed text span"
        );
        let chars_after = self.measure_content(&pruned);
        assert!(
            chars_after <= self.config.threshold_chars && chars_after < total_chars,
            "tool-result prune: replacement must be smaller and within threshold"
        );
        Some(pruned)
    }

    /// Prunes every over-budget tool result from one stable current-surface
    /// snapshot.
    ///
    /// # Errors
    ///
    /// Returns a session rejection; replacements committed earlier in the pass
    /// remain durable.
    pub fn prune_session(&self, session: &Session) -> anyhow::Result<PruneResult> {
        let events = session.events();
        let mut candidates = Vec::new();
        for seq in session.surface_nodes() {
            let index = usize::try_from(seq).unwrap_or(usize::MAX);
            if let Some(event) = events.get(index)
                && event.event_type == "tool/result"
            {
                candidates.push((seq, event.clone()));
            }
        }

        let mut pruned = Vec::new();
        let mut chars_removed = 0;
        for (seq, event) in candidates {
            let message: Message = serde_json::from_value(
                event
                    .data
                    .get("message")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("tool/result lacks its message"))?,
            )?;
            let Some(ContentBlock::ToolResult {
                tool_call_id,
                content: result_content,
                is_error,
            }) = message.content().first()
            else {
                continue;
            };
            let Some(new_content) = self.prune_content(result_content) else {
                continue;
            };
            let chars_before = self.measure_content(result_content);
            let chars_after = self.measure_content(&new_content);
            let call_id = tool_call_id.clone();

            let new_message = Message::from_existing(
                message.id().clone(),
                message.role(),
                vec![ContentBlock::ToolResult {
                    tool_call_id: tool_call_id.clone(),
                    content: new_content,
                    is_error: *is_error,
                }],
                message.source().clone(),
                message.fields().clone(),
            );

            let token_meter = self
                .context
                .get(TOKEN_METER)
                .ok_or_else(|| anyhow::anyhow!("tool-result-pruner requires tokenMeter"))?;
            let shadowed_tokens = token_meter.estimate_message(&message);
            session.append(
                "compaction/prune",
                json!({
                    "shadowedRange": {"start": seq, "end": seq},
                    "shadowedSeqs": [seq],
                    "shadowedTokenCount": shadowed_tokens,
                }),
                AppendOptions::default(),
            )?;

            let mut data = event.data.clone();
            data["message"] = serde_json::to_value(new_message)?;
            let replacement = session.append(
                "tool/result",
                data,
                AppendOptions {
                    surface_op: Some(SurfaceOp::replace(seq, seq)),
                    source_event_seqs: Some(vec![seq]),
                    ..AppendOptions::default()
                },
            )?;
            pruned.push(PrunedEntry {
                original_seq: seq,
                replacement_seq: replacement.seq,
                call_id,
                chars_before,
                chars_after,
            });
            chars_removed += chars_before - chars_after;
        }
        Ok(PruneResult {
            pruned,
            chars_removed,
        })
    }
}

/// Builds the loader-compatible tool-result pruner plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: ToolResultPruneConfig = serde_json::from_value(config)?;
            ToolResultPruner::new(&context, &config)?;
            Ok(())
        })
    })
    .with_config_validator(|value: &Value| {
        config_schema()
            .resolve(value)
            .map_err(|error| anyhow::anyhow!("{error}"))
    })
}
