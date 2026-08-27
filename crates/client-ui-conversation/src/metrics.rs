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
