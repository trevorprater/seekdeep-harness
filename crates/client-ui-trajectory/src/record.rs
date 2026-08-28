//! Shared trajectory record data and formatting contracts.

use serde_json::Value;

/// Closed set of trajectory record kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrajectoryCellKind {
    /// System-prompt or tool-catalog change.
    System,
    /// User input.
    User,
    /// Injected context.
    Context,
    /// Compaction summary.
    Compacted,
    /// Assistant response.
    Message,
    /// Top-level tool call.
    Tool,
    /// Nested tool call.
    Subtool,
}

impl TrajectoryCellKind {
    /// Source wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Context => "context",
            Self::Compacted => "compacted",
            Self::Message => "message",
            Self::Tool => "tool",
            Self::Subtool => "subtool",
        }
    }
}

/// Recorded inputs needed to derive assistant TTFT and decode throughput.
#[derive(Clone, Debug, PartialEq)]
pub struct AssistantMetricDetail {
    /// Whether timing fields were recorded.
    pub timing_recorded: bool,
    /// Recorded step start in Unix milliseconds.
    pub step_start_time: Option<f64>,
    /// Recorded first-token time in Unix milliseconds.
    pub first_token_time: Option<f64>,
    /// Recorded completion time in Unix milliseconds.
    pub completed_time: Option<f64>,
    /// Whether provider usage exists.
    pub usage_provided: bool,
    /// Output tokens, if reported.
    pub output_tokens: Option<u64>,
}

/// One source content block preserved in model order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrajectorySourceBlock {
    /// Extensible block tag.
    pub kind: String,
    /// Complete text or serialized content.
    pub content: String,
    /// Safe image source, when present.
    pub image_src: Option<String>,
    /// Accessible image alternative.
    pub image_alt: Option<String>,
    /// Tool-call correlation identity.
    pub call_id: Option<String>,
    /// Tool name.
    pub tool_name: Option<String>,
}

/// Data contract for one trajectory record.
#[derive(Clone, Debug, PartialEq)]
pub struct TrajectoryCell {
    /// One-based display index.
    pub index: usize,
    /// Projection-stable identity.
    pub record_id: Option<String>,
    /// Closed semantic kind.
    pub kind: TrajectoryCellKind,
    /// Non-Markdown summary.
    pub text: String,
    /// Raw Markdown summary source.
    pub preview_markdown: Option<String>,
    /// Whether this user record opens a turn.
    pub opens_turn: Option<bool>,
    /// Source event sequence.
    pub source_seq: Option<u64>,
    /// Producer role/name payload.
    pub message_source: Option<Value>,
    /// Separator-only request boundary.
    pub request_only: Option<bool>,
    /// Complete input detail.
    pub input_detail: Option<String>,
    /// Complete prompt state.
    pub prompt_detail: Option<Value>,
    /// Replaced prompt state.
    pub previous_prompt_detail: Option<Value>,
    /// Complete output detail.
    pub output_detail: Option<String>,
    /// Complete reasoning detail.
    pub thinking_detail: Option<String>,
    /// Original input blocks.
    pub source_blocks: Vec<TrajectorySourceBlock>,
    /// Original output blocks.
    pub output_blocks: Vec<TrajectorySourceBlock>,
    /// Call-time model-visible schema.
    pub schema_detail: Option<String>,
    /// Assistant timing and usage facts.
    pub assistant_metrics: Option<AssistantMetricDetail>,
    /// Tool result summary.
    pub result: Option<String>,
    /// Raw Markdown result preview.
    pub result_preview_markdown: Option<String>,
    /// Tool-call correlation identity.
    pub call_id: Option<String>,
    /// Tool failure state.
    pub is_error: Option<bool>,
    /// Own duration in seconds.
    pub time_seconds: Option<f64>,
    /// Actual start in Unix milliseconds.
    pub started_at: Option<f64>,
    /// Prompt input tokens.
    pub input: Option<u64>,
    /// Cache-read input tokens.
    pub cache_read: Option<u64>,
    /// Cache-write input tokens.
    pub cache_write: Option<u64>,
    /// Completion tokens.
    pub output: Option<u64>,
    /// Reasoning tokens.
    pub think: Option<u64>,
    /// Legacy standalone-cell selection state.
    pub selected: Option<bool>,
}

impl TrajectoryCell {
    /// Creates the minimal ordinary record shape used by projections.
    #[must_use]
    pub fn new(index: usize, kind: TrajectoryCellKind, text: impl Into<String>) -> Self {
        Self {
            index,
            record_id: None,
            kind,
            text: text.into(),
            preview_markdown: None,
            opens_turn: None,
            source_seq: None,
            message_source: None,
            request_only: None,
            input_detail: None,
            prompt_detail: None,
            previous_prompt_detail: None,
            output_detail: None,
            thinking_detail: None,
            source_blocks: Vec::new(),
            output_blocks: Vec::new(),
            schema_detail: None,
            assistant_metrics: None,
            result: None,
            result_preview_markdown: None,
            call_id: None,
            is_error: None,
            time_seconds: None,
            started_at: None,
            input: None,
            cache_read: None,
            cache_write: None,
            output: None,
            think: None,
            selected: None,
        }
    }
}

/// Resolves identity that survives prepending older projected records.
#[must_use]
pub fn trajectory_record_id(cell: &TrajectoryCell) -> String {
    if let Some(record_id) = &cell.record_id {
        return record_id.clone();
    }
    if let Some(call_id) = &cell.call_id {
        return format!("{}\0call\0{call_id}", cell.kind.as_str());
    }
    if let Some(source_seq) = cell.source_seq {
        return format!("{}\0seq\0{source_seq}", cell.kind.as_str());
    }
    format!("{}\0index\0{}", cell.kind.as_str(), cell.index)
}

/// Formats a finite millisecond duration with JavaScript rounding and separators.
#[must_use]
pub fn format_duration_millis(milliseconds: Option<f64>) -> String {
    let Some(milliseconds) = milliseconds.filter(|value| value.is_finite()) else {
        return "—".to_owned();
    };
    let rounded = (milliseconds + 0.5).floor();
    let integer = if rounded == 0.0 {
        "0".to_owned()
    } else {
        format!("{rounded:.0}")
    };
    format!("{} ms", group_decimal_integer(&integer))
}

/// Formats elapsed seconds as an integer-millisecond label.
#[must_use]
pub fn format_elapsed_seconds(seconds: Option<f64>) -> String {
    format_duration_millis(seconds.map(|value| value * 1_000.0))
}

fn group_decimal_integer(integer: &str) -> String {
    let (sign, digits) = integer
        .strip_prefix('-')
        .map_or(("", integer), |digits| ("-", digits));
    let mut grouped = String::with_capacity(integer.len() + integer.len() / 3);
    grouped.push_str(sign);
    let first = digits.len() % 3;
    if first != 0 {
        grouped.push_str(&digits[..first]);
        if first < digits.len() {
            grouped.push(',');
        }
    }
    for (position, chunk) in digits.as_bytes()[first..].chunks(3).enumerate() {
        if position > 0 {
            grouped.push(',');
        }
        grouped.push_str(std::str::from_utf8(chunk).expect("formatted decimal digits are UTF-8"));
    }
    grouped
}
