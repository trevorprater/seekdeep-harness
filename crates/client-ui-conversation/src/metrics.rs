//! Latency and decode-throughput folds shared by turn chrome.

use std::collections::BTreeMap;

/// One assistant step's recorded timing boundaries.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AssistantTiming {
    /// step/start timestamp.
    pub step_start_time: Option<f64>,
    /// First token-delta timestamp.
    pub first_token_time: Option<f64>,
    /// Final assistant-message timestamp.
    pub completed_time: f64,
}

/// Minimal assistant node consumed by the metric fold.
#[derive(Clone, Debug, PartialEq)]
pub struct AssistantMetricNode {
    /// Turn coordinate.
    pub turn: u64,
    /// Step coordinate.
    pub step: u64,
    /// Optional timing.
    pub timing: Option<AssistantTiming>,
    /// Provider-reported output tokens.
    pub output_tokens: Option<f64>,
}

/// Per-step derivable readings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StepReading {
    /// step/start to first token.
    pub ttft_ms: Option<f64>,
    /// First token to completion.
    pub decode_ms: Option<f64>,
    /// Valid nonnegative finite output-token count.
    pub output_tokens: Option<f64>,
}

/// Available footer figures for one turn.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TurnMetrics {
    /// Lowest-step TTFT.
    pub ttft_ms: Option<f64>,
    /// Summed output tokens divided by summed decode seconds.
    pub tokens_per_second: Option<f64>,
}

/// Derives one assistant step's latency and usage readings.
#[must_use]
pub fn assistant_step_reading(node: &AssistantMetricNode) -> StepReading {
    let timing = node.timing;
    let ttft_ms = timing
        .and_then(|timing| Some((timing.first_token_time? - timing.step_start_time?).max(0.0)));
    let decode_ms =
        timing.and_then(|timing| Some((timing.completed_time - timing.first_token_time?).max(0.0)));
    let output_tokens = node
        .output_tokens
        .filter(|tokens| tokens.is_finite() && *tokens >= 0.0);
    StepReading {
        ttft_ms,
        decode_ms,
        output_tokens,
    }
}

#[derive(Clone, Copy, Debug)]
struct Fold {
    first_step: u64,
    first_ttft: Option<f64>,
    decode_ms: f64,
    output_tokens: f64,
    sampled: bool,
}

/// Folds assistant nodes into per-turn footer metrics.
#[must_use]
pub fn derive_turn_metrics(nodes: &[AssistantMetricNode]) -> BTreeMap<u64, TurnMetrics> {
    let mut folds = BTreeMap::<u64, Fold>::new();
    for node in nodes {
        let reading = assistant_step_reading(node);
        let fold = folds.entry(node.turn).or_insert(Fold {
            first_step: node.step,
            first_ttft: reading.ttft_ms,
            decode_ms: 0.0,
            output_tokens: 0.0,
            sampled: false,
        });
        if node.step < fold.first_step {
            fold.first_step = node.step;
            fold.first_ttft = reading.ttft_ms;
        }
        if let (Some(decode_ms), Some(tokens)) = (reading.decode_ms, reading.output_tokens) {
            fold.decode_ms += decode_ms;
            fold.output_tokens += tokens;
            fold.sampled = true;
        }
    }
    folds
        .into_iter()
        .filter_map(|(turn, fold)| {
            let tokens_per_second = (fold.sampled && fold.decode_ms > 0.0)
                .then(|| fold.output_tokens / (fold.decode_ms / 1000.0));
            (fold.first_ttft.is_some() || tokens_per_second.is_some()).then_some((
                turn,
                TurnMetrics {
                    ttft_ms: fold.first_ttft,
                    tokens_per_second,
                },
            ))
        })
        .collect()
}

/// Formats latency seconds without a unit.
#[must_use]
pub fn format_latency_seconds(milliseconds: f64) -> String {
    let seconds = milliseconds.max(0.0) / 1000.0;
    if seconds < 10.0 {
        ((seconds * 10.0).round() / 10.0).to_string()
    } else {
        seconds.round().to_string()
    }
}

/// Formats decode throughput without a unit.
#[must_use]
pub fn format_tokens_per_second(tokens_per_second: f64) -> String {
    let value = tokens_per_second.max(0.0);
    if value >= 10.0 {
        value.round().to_string()
    } else {
        ((value * 10.0).round() / 10.0).to_string()
    }
}

/// A settled conversation node that contributes to the composer stats strip.
#[derive(Clone, Debug, PartialEq)]
pub enum WindowMetricNode {
    /// One settled assistant step.
    Assistant(AssistantMetricNode),
    /// One settled tool result and its optional call timestamp.
    ToolResult {
        /// Result timestamp.
        time: f64,
        /// Matching tool-call timestamp.
        call_time: Option<f64>,
    },
    /// A node kind that contributes no stats.
    Other,
}

/// Window-scoped fallback totals for assemblies without the durable projection.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WindowStats {
    /// Distinct assistant turns.
    pub turns: u64,
    /// Settled assistant steps.
    pub steps: u64,
    /// Summed assistant request wall time.
    pub llm_ms: f64,
    /// Summed paired tool wall time.
    pub tool_ms: f64,
    /// Summed recorded first-token latency.
    pub ttft_ms: f64,
    /// Steps carrying first-token latency.
    pub ttft_steps: u64,
    /// Summed decode wall time for usage-carrying steps.
    pub decode_ms: f64,
    /// Summed output tokens over those decode-timed steps.
    pub decode_tokens: f64,
}

/// Folds the loaded settled window into the stats-strip fallback.
#[must_use]
pub fn derive_window_stats(nodes: &[WindowMetricNode]) -> WindowStats {
    use std::collections::BTreeSet;

    let mut turns = BTreeSet::new();
    let mut stats = WindowStats::default();
    for node in nodes {
        match node {
            WindowMetricNode::ToolResult {
                time,
                call_time: Some(call_time),
            } => stats.tool_ms += (time - call_time).max(0.0),
            WindowMetricNode::ToolResult {
                call_time: None, ..
            }
            | WindowMetricNode::Other => {}
            WindowMetricNode::Assistant(node) => {
                turns.insert(node.turn);
                stats.steps += 1;
                if let Some(AssistantTiming {
                    step_start_time: Some(step_start_time),
                    completed_time,
                    ..
                }) = node.timing
                {
                    stats.llm_ms += (completed_time - step_start_time).max(0.0);
                }
                let reading = assistant_step_reading(node);
                if let Some(ttft_ms) = reading.ttft_ms {
                    stats.ttft_ms += ttft_ms;
                    stats.ttft_steps += 1;
                }
                if let (Some(decode_ms), Some(output_tokens)) =
                    (reading.decode_ms, reading.output_tokens)
                {
                    stats.decode_ms += decode_ms;
                    stats.decode_tokens += output_tokens;
                }
            }
        }
    }
    stats.turns = turns.len() as u64;
    stats
}

/// Compact token count used by the stats strip.
#[must_use]
pub fn format_tokens(tokens: f64) -> String {
    fn scaled(value: f64) -> String {
        if value >= 100.0 {
            value.round().to_string()
        } else {
            ((value * 10.0).round() / 10.0).to_string()
        }
    }

    if tokens < 1_000.0 {
        tokens.to_string()
    } else if tokens < 1_000_000.0 {
        format!("{}K", scaled(tokens / 1_000.0))
    } else {
        format!("{}M", scaled(tokens / 1_000_000.0))
    }
}

/// Compact duration used by the stats strip.
#[must_use]
pub fn format_duration(milliseconds: f64) -> String {
    let seconds = milliseconds / 1_000.0;
    if seconds < 60.0 {
        format!("{}s", (seconds * 10.0).round() / 10.0)
    } else {
        let whole = seconds.round();
        format!("{}m{}s", (whole / 60.0).floor(), whole % 60.0)
    }
}

/// Durable token-usage projection fields consumed by the stats strip.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TokenUsageStats {
    /// Uncached input tokens.
    pub uncached_input_tokens: f64,
    /// Cache-read tokens.
    pub cache_read_tokens: f64,
    /// Cache-write tokens.
    pub cache_write_tokens: f64,
    /// Output tokens.
    pub output_tokens: f64,
}

/// Sums the three disjoint prompt-side billing buckets.
#[must_use]
pub const fn billed_input_tokens(usage: TokenUsageStats) -> f64 {
    usage.uncached_input_tokens + usage.cache_read_tokens + usage.cache_write_tokens
}

/// Returns the rounded cache-read share of billed prompt input.
#[must_use]
pub fn cache_hit_percent(usage: TokenUsageStats) -> Option<f64> {
    let denominator = billed_input_tokens(usage);
    (denominator != 0.0).then(|| (usage.cache_read_tokens / denominator * 100.0).round())
}

/// Context-pressure projection fields used for the exported occupancy helper.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ContextPressureStats {
    /// Last provider-reported prompt size.
    pub pressure_tokens: Option<f64>,
    /// Provider sample carried forward over subsequent surface movement.
    pub projected_tokens: Option<f64>,
    /// Last known model context capacity.
    pub context_window: Option<f64>,
}

/// Resolved context occupancy.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContextOccupancy {
    /// Rounded occupancy, clamped only at the upper bound.
    pub percent: f64,
    /// Selected numerator.
    pub used_tokens: f64,
    /// Selected capacity.
    pub context_window: f64,
}

/// Resolves context occupancy only after both numerator and capacity are known.
#[must_use]
pub fn context_occupancy(pressure: Option<ContextPressureStats>) -> Option<ContextOccupancy> {
    let pressure = pressure?;
    let used_tokens = pressure.projected_tokens.or(pressure.pressure_tokens)?;
    let context_window = pressure.context_window?;
    let percent = (used_tokens / context_window * 100.0).round().min(100.0);
    Some(ContextOccupancy {
        percent,
        used_tokens,
        context_window,
    })
}
