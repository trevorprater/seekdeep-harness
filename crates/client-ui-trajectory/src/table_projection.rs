//! Deterministic trajectory-ledger projection and inspector semantics.

use std::collections::{BTreeMap, BTreeSet};

use indexmap::IndexSet;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AssistantMetricDetail, CollapsedSummaryKind, TrajectoryCell, TrajectoryCellKind,
    TrajectoryTurnModel, trajectory_preview_text, trajectory_record_id,
};

/// One flattened ledger record with its structural placement.
#[derive(Clone, Debug, PartialEq)]
pub struct TrajectoryTableRecord {
    /// Model turn, absent between turns.
    pub turn: Option<u64>,
    /// Zero-based source-section position.
    pub section: usize,
    /// Source group title.
    pub group: String,
    /// Whether this is the first retained row in its group.
    pub group_start: bool,
    /// Whether this row opens visible turn content.
    pub turn_start: bool,
    /// Source record.
    pub cell: TrajectoryCell,
    /// Whether this is the final row in its source section.
    pub turn_end: bool,
    /// Synthetic folded-row description.
    pub collapsed_summary: Option<String>,
    /// Synthetic folded-row class.
    pub collapsed_summary_kind: Option<CollapsedSummaryKind>,
}

/// Request lifecycle state rendered by the ledger inspector.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrajectoryRecordState {
    /// Request or record completed.
    Complete,
    /// Request or record remains in flight.
    Running,
    /// Request or record failed.
    Error,
}

/// One provider token-usage snapshot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrajectoryUsage {
    /// Uncached input tokens.
    pub input: Option<u64>,
    /// Cache-read input tokens.
    pub cache_read: Option<u64>,
    /// Cache-write input tokens.
    pub cache_write: Option<u64>,
    /// Output tokens.
    pub output: Option<u64>,
    /// Reasoning-token subset.
    pub reasoning: Option<u64>,
}

/// Request purpose carried by the session-global request index.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrajectoryRequestPurpose {
    /// Ordinary assistant generation.
    #[default]
    Assistant,
    /// Context compaction.
    Compaction,
}

/// Session-global request metadata consumed by the table inspector.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrajectoryRequestNumber {
    /// Request anchor event sequence, when finalized.
    pub seq: Option<u64>,
    /// Source group title.
    pub group: String,
    /// Session-global one-based request number.
    pub number: u64,
    /// Explicit lifecycle state.
    pub status: Option<TrajectoryRecordState>,
    /// Recorded start time in Unix milliseconds.
    pub started_at: Option<f64>,
    /// Recorded completion time in Unix milliseconds.
    pub completed_at: Option<f64>,
    /// Display-safe failure.
    pub error: Option<String>,
    /// Scheduled retry ordinal.
    pub retry: Option<u64>,
    /// Maximum retry count.
    pub max_retries: Option<u64>,
    /// Retry delay in milliseconds.
    pub retry_delay_ms: Option<f64>,
    /// Result event sequence.
    pub result_seq: Option<u64>,
    /// Provider name.
    pub provider: Option<String>,
    /// Model name.
    pub model: Option<String>,
    /// Exact provider request configuration.
    pub request_config: Option<Value>,
    /// Per-request usage.
    pub usage: Option<TrajectoryUsage>,
    /// Session-prefix cumulative usage.
    pub cumulative_usage: Option<TrajectoryUsage>,
    /// Request purpose; omitted source values mean assistant generation.
    #[serde(default)]
    pub purpose: TrajectoryRequestPurpose,
    /// Model turn, absent for between-turn compaction.
    pub turn: Option<u64>,
    /// Assistant step, or zero for compaction.
    pub step: u64,
}

/// Inspector tab identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrajectoryDetailTab {
    /// Complete system prompt.
    SystemPrompt,
    /// Tool catalog.
    Tools,
    /// Summary.
    Overview,
    /// Rendered Markdown.
    Rendered,
    /// Raw Markdown or source blocks.
    Raw,
    /// Message provenance.
    Source,
    /// Tool payload.
    Input,
    /// Tool result.
    Output,
    /// Model-visible tool schema.
    Schema,
    /// Provider options.
    Options,
    /// Token usage.
    Usage,
    /// Recorded timing.
    Timing,
    /// Prompt update diff.
    Diff,
}

impl TrajectoryDetailTab {
    /// Source tab identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SystemPrompt => "system-prompt",
            Self::Tools => "tools",
            Self::Overview => "overview",
            Self::Rendered => "rendered",
            Self::Raw => "raw",
            Self::Source => "source",
            Self::Input => "input",
            Self::Output => "output",
            Self::Schema => "schema",
            Self::Options => "options",
            Self::Usage => "usage",
            Self::Timing => "timing",
            Self::Diff => "diff",
        }
    }
}

/// One context-sensitive inspector tab and its user-facing label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrajectoryDetailTabItem {
    /// Stable tab identity.
    pub id: TrajectoryDetailTab,
    /// Source user-facing label.
    pub label: &'static str,
}

/// Parsed split of a tool-call row label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrajectoryToolCallTextParts {
    /// Tool name.
    pub name: String,
    /// Serialized arguments, when separated from the name.
    pub arguments: Option<String>,
}

/// Parent record indexes for a Tool or Subtool record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrajectoryParentRecords {
    /// Parent assistant-message display index.
    pub message: Option<usize>,
    /// Parent top-level Tool display index.
    pub tool: Option<usize>,
}

/// Valid parsed model-visible tool schema.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedTrajectoryToolSchema {
    /// Tool name.
    pub name: String,
    /// Tool description.
    pub description: String,
    /// Object-valued parameters schema.
    pub parameters: Value,
}

/// Flattens grouped trajectory sections into source-equivalent ledger records.
#[must_use]
pub fn flatten_trajectory_table_records(
    turns: &[TrajectoryTurnModel],
) -> Vec<TrajectoryTableRecord> {
    let mut output = Vec::new();
    for (section, turn) in turns.iter().enumerate() {
        let mut first_in_section = true;
        let section_start = output.len();
        for group in &turn.groups {
            for (index, cell) in group.cells.iter().enumerate() {
                let turn_start = first_in_section
                    && cell.request_only != Some(true)
                    && cell.kind != TrajectoryCellKind::System
                    && (cell.kind != TrajectoryCellKind::Compacted || turn.turn.is_none());
                if turn_start {
                    first_in_section = false;
                }
                output.push(TrajectoryTableRecord {
                    turn: turn.turn,
                    section,
                    group: group.title.clone(),
                    group_start: index == 0,
                    turn_start,
                    cell: cell.clone(),
                    turn_end: false,
                    collapsed_summary: None,
                    collapsed_summary_kind: None,
                });
            }
        }
        if let Some(last) = output.get_mut(section_start..).and_then(<[_]>::last_mut) {
            last.turn_end = true;
        }
    }
    output
}

/// Retains matching content records and recomputes structural boundaries.
#[must_use]
pub fn filter_trajectory_table_records(
    records: &[TrajectoryTableRecord],
    matches: &BTreeSet<usize>,
) -> Vec<TrajectoryTableRecord> {
    let mut filtered = records
        .iter()
        .filter(|record| {
            record.cell.request_only != Some(true) && matches.contains(&record.cell.index)
        })
        .cloned()
        .map(|mut record| {
            record.group_start = false;
            record.turn_start = false;
            record.turn_end = false;
            record
        })
        .collect::<Vec<_>>();
    let mut started_sections = BTreeSet::new();
    for index in 0..filtered.len() {
        let previous = index.checked_sub(1).and_then(|at| filtered.get(at));
        let next = filtered.get(index + 1);
        let record = &filtered[index];
        let group_start = previous.is_none_or(|previous| {
            previous.section != record.section || previous.group != record.group
        });
        let turn_start = !started_sections.contains(&record.section)
            && record.cell.kind != TrajectoryCellKind::System
            && (record.cell.kind != TrajectoryCellKind::Compacted || record.turn.is_none());
        let turn_end = next.is_none_or(|next| next.section != record.section);
        let section = record.section;
        let record = &mut filtered[index];
        record.group_start = group_start;
        record.turn_start = turn_start;
        record.turn_end = turn_end;
        if turn_start {
            started_sections.insert(section);
        }
    }
    filtered
}

/// Parses a positive integral `Step N` group number with JavaScript-number semantics.
#[must_use]
pub fn trajectory_request_step(group: &str) -> Option<u64> {
    let value = group.strip_prefix("Step ")?.trim().parse::<f64>().ok()?;
    #[allow(clippy::cast_precision_loss)]
    if !value.is_finite() || value <= 0.0 || value.fract() != 0.0 || value > u64::MAX as f64 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(value as u64)
}

/// Builds the exact NUL-delimited source request-group key.
#[must_use]
pub fn trajectory_request_key(turn: Option<u64>, group: &str) -> String {
    format!(
        "{}\0{group}",
        turn.map_or_else(|| "null".to_owned(), |turn| turn.to_string())
    )
}

/// Locates the row that owns each visible request marker.
#[must_use]
pub fn index_trajectory_request_boundaries(
    records: &[TrajectoryTableRecord],
) -> BTreeMap<String, usize> {
    let mut boundaries = BTreeMap::new();
    for record in records {
        let key = trajectory_request_key(record.turn, &record.group);
        if boundaries.contains_key(&key) {
            continue;
        }
        if trajectory_request_step(&record.group).is_none() {
            if record.group_start {
                boundaries.insert(key, record.cell.index);
            }
            continue;
        }
        if matches!(
            record.cell.kind,
            TrajectoryCellKind::User | TrajectoryCellKind::Context
        ) {
            continue;
        }
        boundaries.insert(key, record.cell.index);
    }
    boundaries
}

/// Assigns session-global or deterministic fallback request numbers.
#[must_use]
pub fn index_trajectory_request_numbers(
    records: &[TrajectoryTableRecord],
    session_numbers: &[TrajectoryRequestNumber],
    boundaries: &BTreeMap<String, usize>,
) -> BTreeMap<String, u64> {
    let mut numbers = session_numbers
        .iter()
        .map(|request| {
            (
                trajectory_request_key(request.turn, &request.group),
                request.number,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut next = numbers.values().copied().max().unwrap_or(0) + 1;
    let mut boundary_records = records
        .iter()
        .filter(|record| {
            boundaries.get(&trajectory_request_key(record.turn, &record.group))
                == Some(&record.cell.index)
                && trajectory_request_step(&record.group).is_some()
        })
        .collect::<Vec<_>>();
    boundary_records.sort_by_key(|record| record.cell.index);
    for record in boundary_records {
        let key = trajectory_request_key(record.turn, &record.group);
        if let std::collections::btree_map::Entry::Vacant(entry) = numbers.entry(key) {
            entry.insert(next);
            next += 1;
        }
    }
    numbers
}

/// Indexes coincident request markers from left to right.
#[must_use]
pub fn index_trajectory_request_boundary_runs(
    records: &[TrajectoryTableRecord],
) -> BTreeMap<usize, usize> {
    let mut indexes = BTreeMap::new();
    let mut run_length = 0;
    for record in records {
        if record.cell.request_only == Some(true) {
            indexes.insert(record.cell.index, run_length);
            run_length += 1;
            continue;
        }
        if run_length > 0 && record.group_start && trajectory_request_step(&record.group).is_some()
        {
            indexes.insert(record.cell.index, run_length);
        }
        run_length = 0;
    }
    indexes
}

/// Builds the compact source summary for hidden turn content.
#[must_use]
pub fn summarize_trajectory_turn(records: &[TrajectoryTableRecord]) -> String {
    let steps = records
        .iter()
        .filter_map(|record| {
            record
                .group
                .starts_with("Step ")
                .then_some(record.group.as_str())
        })
        .collect::<BTreeSet<_>>()
        .len();
    let tool_calls = records
        .iter()
        .filter(|record| {
            matches!(
                record.cell.kind,
                TrajectoryCellKind::Tool | TrajectoryCellKind::Subtool
            )
        })
        .count();
    format!(
        "{steps} {} · {tool_calls} tool {}",
        if steps == 1 { "step" } else { "steps" },
        if tool_calls == 1 { "call" } else { "calls" }
    )
}

/// Folds each selected turn to its first content row plus a synthetic summary.
#[must_use]
pub fn collapse_trajectory_turn_records(
    records: &[TrajectoryTableRecord],
    collapsed_turns: &BTreeSet<u64>,
) -> Vec<TrajectoryTableRecord> {
    let mut by_turn = BTreeMap::<u64, Vec<&TrajectoryTableRecord>>::new();
    for record in records {
        if let Some(turn) = record.turn {
            by_turn.entry(turn).or_default().push(record);
        }
    }
    let mut output = Vec::new();
    for record in records {
        let Some(turn) = record.turn else {
            output.push(record.clone());
            continue;
        };
        if !collapsed_turns.contains(&turn) {
            output.push(record.clone());
            continue;
        }
        let turn_records = by_turn
            .get(&turn)
            .map_or_else(|| std::slice::from_ref(&record), Vec::as_slice);
        if record.cell.request_only == Some(true) || record.cell.kind == TrajectoryCellKind::System
        {
            output.push(record.clone());
            continue;
        }
        let content = turn_records
            .iter()
            .copied()
            .filter(|candidate| {
                candidate.cell.request_only != Some(true)
                    && candidate.cell.kind != TrajectoryCellKind::System
            })
            .collect::<Vec<_>>();
        if content.len() <= 1 {
            output.push(record.clone());
            continue;
        }
        if content.first().map(|first| first.cell.index) != Some(record.cell.index) {
            continue;
        }
        let mut first = record.clone();
        first.turn_end = false;
        output.push(first);
        let mut summary = record.clone();
        summary.group_start = false;
        summary.turn_start = false;
        summary.turn_end = true;
        summary.collapsed_summary = Some(summarize_trajectory_turn(
            &content
                .iter()
                .skip(1)
                .map(|record| (*record).clone())
                .collect::<Vec<_>>(),
        ));
        summary.collapsed_summary_kind = Some(CollapsedSummaryKind::Turn);
        output.push(summary);
    }
    output
}

/// Returns consecutive Tool/Subtool records owned by an assistant row.
#[must_use]
pub fn trajectory_assistant_tool_calls(
    records: &[TrajectoryTableRecord],
    assistant_index: usize,
) -> Vec<&TrajectoryTableRecord> {
    let Some(at) = records
        .iter()
        .position(|record| record.cell.index == assistant_index)
    else {
        return Vec::new();
    };
    if records[at].cell.kind != TrajectoryCellKind::Message {
        return Vec::new();
    }
    records[at + 1..]
        .iter()
        .take_while(|record| {
            matches!(
                record.cell.kind,
                TrajectoryCellKind::Tool | TrajectoryCellKind::Subtool
            )
        })
        .collect()
}

/// Builds the compact source summary for hidden assistant tool calls.
#[must_use]
pub fn summarize_trajectory_assistant_tools(records: &[TrajectoryTableRecord]) -> String {
    let names = records
        .iter()
        .filter_map(|record| {
            let name = record
                .cell
                .text
                .split_once(" · ")
                .map_or(record.cell.text.as_str(), |(name, _)| name);
            (!name.is_empty()).then_some(name)
        })
        .collect::<IndexSet<_>>();
    let count = records.len();
    let summary = format!("{count} tool {}", if count == 1 { "call" } else { "calls" });
    if names.is_empty() {
        summary
    } else {
        format!(
            "{summary} · {}",
            names.into_iter().collect::<Vec<_>>().join(", ")
        )
    }
}

/// Folds selected assistant call groups into synthetic summary rows.
#[must_use]
pub fn collapse_trajectory_assistant_records(
    records: &[TrajectoryTableRecord],
    collapsed_assistants: &BTreeSet<String>,
) -> Vec<TrajectoryTableRecord> {
    let mut output = Vec::new();
    let mut index = 0;
    while index < records.len() {
        let record = &records[index];
        output.push(record.clone());
        if record.cell.kind != TrajectoryCellKind::Message
            || !collapsed_assistants.contains(&trajectory_record_id(&record.cell))
        {
            index += 1;
            continue;
        }
        let calls = records[index + 1..]
            .iter()
            .take_while(|candidate| {
                candidate.collapsed_summary.is_none()
                    && matches!(
                        candidate.cell.kind,
                        TrajectoryCellKind::Tool | TrajectoryCellKind::Subtool
                    )
            })
            .cloned()
            .collect::<Vec<_>>();
        if calls.is_empty() {
            index += 1;
            continue;
        }
        if let Some(last) = output.last_mut() {
            last.turn_end = false;
        }
        let mut summary = record.clone();
        summary.group_start = false;
        summary.turn_start = false;
        summary.turn_end = calls.last().is_some_and(|last| last.turn_end);
        summary.collapsed_summary = Some(summarize_trajectory_assistant_tools(&calls));
        summary.collapsed_summary_kind = Some(CollapsedSummaryKind::Assistant);
        output.push(summary);
        index += calls.len() + 1;
    }
    output
}

/// Derives running, failed, or complete state from one record.
#[must_use]
pub fn trajectory_record_state(record: &TrajectoryTableRecord) -> TrajectoryRecordState {
    if record.cell.is_error == Some(true) {
        return TrajectoryRecordState::Error;
    }
    if record.cell.kind == TrajectoryCellKind::Compacted && record.cell.time_seconds.is_none() {
        return TrajectoryRecordState::Running;
    }
    if matches!(
        record.cell.kind,
        TrajectoryCellKind::Tool | TrajectoryCellKind::Subtool
    ) && record.cell.output_detail.is_none()
    {
        return TrajectoryRecordState::Running;
    }
    TrajectoryRecordState::Complete
}

/// Returns the source user-facing lifecycle label.
#[must_use]
pub const fn trajectory_status_label(state: TrajectoryRecordState) -> &'static str {
    match state {
        TrajectoryRecordState::Error => "Failed",
        TrajectoryRecordState::Running => "Pending",
        TrajectoryRecordState::Complete => "Completed",
    }
}

/// Returns the source turn-section label.
#[must_use]
pub fn trajectory_section_label(turn: Option<u64>) -> String {
    turn.map_or_else(|| "Between turns".to_owned(), |turn| format!("Turn {turn}"))
}

/// Adds the disjoint input-token buckets when any was recorded.
#[must_use]
pub fn trajectory_input_total(usage: TrajectoryUsage) -> Option<u64> {
    let values = [usage.input, usage.cache_read, usage.cache_write];
    values
        .iter()
        .any(Option::is_some)
        .then(|| values.into_iter().flatten().sum())
}

/// Formats the compact assistant or request timing duration.
#[must_use]
pub fn format_trajectory_detail_duration(milliseconds: f64) -> String {
    if milliseconds < 1_000.0 {
        return format!("{:.0} ms", js_round(milliseconds));
    }
    if milliseconds < 10_000.0 {
        format!("{:.2} s", milliseconds / 1_000.0)
    } else {
        format!("{:.1} s", milliseconds / 1_000.0)
    }
}

/// Formats total assistant generation time using source fallback labels.
#[must_use]
pub fn trajectory_assistant_total_time(metrics: &AssistantMetricDetail) -> String {
    if !metrics.timing_recorded {
        return "Not recorded".to_owned();
    }
    let Some(start) = metrics.step_start_time else {
        return "Step start unavailable".to_owned();
    };
    let Some(completed) = metrics.completed_time else {
        return "Pending".to_owned();
    };
    format_trajectory_detail_duration((completed - start).max(0.0))
}

/// Formats assistant time-to-first-token using source fallback labels.
#[must_use]
pub fn trajectory_assistant_ttft(metrics: &AssistantMetricDetail) -> String {
    if !metrics.timing_recorded {
        return "Not recorded".to_owned();
    }
    let Some(start) = metrics.step_start_time else {
        return "Step start unavailable".to_owned();
    };
    let Some(first) = metrics.first_token_time else {
        return "First token unavailable".to_owned();
    };
    format_trajectory_detail_duration((first - start).max(0.0))
}

/// Formats assistant post-first-token generation time.
#[must_use]
pub fn trajectory_assistant_generation_time(metrics: &AssistantMetricDetail) -> String {
    if !metrics.timing_recorded {
        return "First token unavailable".to_owned();
    }
    let Some(first) = metrics.first_token_time else {
        return "First token unavailable".to_owned();
    };
    let Some(completed) = metrics.completed_time else {
        return "Pending".to_owned();
    };
    format_trajectory_detail_duration((completed - first).max(0.0))
}

/// Formats assistant decoding throughput using source fallback labels.
#[must_use]
pub fn trajectory_assistant_throughput(metrics: &AssistantMetricDetail) -> String {
    if !metrics.usage_provided {
        return "Usage unavailable".to_owned();
    }
    let Some(tokens) = metrics.output_tokens else {
        return "Output tokens unavailable".to_owned();
    };
    if !metrics.timing_recorded {
        return "First token unavailable".to_owned();
    }
    let Some(first) = metrics.first_token_time else {
        return "First token unavailable".to_owned();
    };
    let Some(completed) = metrics.completed_time else {
        return "Pending".to_owned();
    };
    let generation_seconds = (completed - first) / 1_000.0;
    if generation_seconds <= 0.0 {
        return "Duration too short".to_owned();
    }
    #[allow(clippy::cast_precision_loss)]
    let rate = tokens as f64 / generation_seconds;
    format!("{rate:.1} tok/s")
}

/// Builds the source label for a model-visible message producer.
#[must_use]
pub fn trajectory_message_source_label(source: &Value) -> String {
    let Some(properties) = source.as_object() else {
        return "Unknown".to_owned();
    };
    match properties.get("kind").and_then(Value::as_str) {
        Some("user") => "User".to_owned(),
        Some("plugin") => properties
            .get("plugin")
            .and_then(Value::as_str)
            .filter(|plugin| !plugin.is_empty())
            .map_or_else(
                || "Plugin".to_owned(),
                |plugin| format!("Plugin · {plugin}"),
            ),
        Some("goal") => properties
            .get("round")
            .and_then(Value::as_f64)
            .filter(|round| *round > 0.0)
            .map_or_else(
                || "Goal".to_owned(),
                |round| format!("Goal · Round {round}"),
            ),
        Some(kind) if !kind.is_empty() => {
            let mut characters = kind.chars();
            let first = characters
                .next()
                .map(|character| character.to_uppercase().collect::<String>())
                .unwrap_or_default();
            format!("{first}{}", characters.as_str())
        }
        _ => "Unknown".to_owned(),
    }
}

/// Returns the exact inspector tabs available to one record.
#[must_use]
pub fn trajectory_detail_tabs(record: &TrajectoryTableRecord) -> Vec<TrajectoryDetailTabItem> {
    const fn tab(id: TrajectoryDetailTab, label: &'static str) -> TrajectoryDetailTabItem {
        TrajectoryDetailTabItem { id, label }
    }
    match record.cell.kind {
        TrajectoryCellKind::System if record.cell.previous_prompt_detail.is_some() => vec![
            tab(TrajectoryDetailTab::Diff, "Diff"),
            tab(TrajectoryDetailTab::SystemPrompt, "System Prompt"),
            tab(TrajectoryDetailTab::Tools, "Tools"),
        ],
        TrajectoryCellKind::System => vec![
            tab(TrajectoryDetailTab::SystemPrompt, "System Prompt"),
            tab(TrajectoryDetailTab::Tools, "Tools"),
        ],
        TrajectoryCellKind::Compacted => vec![
            tab(TrajectoryDetailTab::Overview, "Summary"),
            tab(TrajectoryDetailTab::Raw, "Raw Output"),
        ],
        TrajectoryCellKind::User | TrajectoryCellKind::Context | TrajectoryCellKind::Message => {
            let mut tabs = vec![
                tab(TrajectoryDetailTab::Overview, "Summary"),
                tab(TrajectoryDetailTab::Rendered, "Preview"),
                tab(TrajectoryDetailTab::Raw, "Raw"),
            ];
            if record.cell.message_source.is_some() {
                tabs.push(tab(TrajectoryDetailTab::Source, "Source"));
            }
            tabs
        }
        TrajectoryCellKind::Tool | TrajectoryCellKind::Subtool => {
            let mut tabs = vec![tab(TrajectoryDetailTab::Overview, "Summary")];
            if record
                .cell
                .input_detail
                .as_ref()
                .is_some_and(|value| !value.is_empty())
            {
                tabs.push(tab(TrajectoryDetailTab::Input, "Payload"));
            }
            if record
                .cell
                .output_detail
                .as_ref()
                .is_some_and(|value| !value.is_empty())
            {
                tabs.push(tab(TrajectoryDetailTab::Output, "Result"));
            }
            tabs.extend([
                tab(TrajectoryDetailTab::Schema, "Schema"),
                tab(TrajectoryDetailTab::Timing, "Timing"),
            ]);
            tabs
        }
    }
}

/// Returns whether an assistant record contains no visible response outside tool calls.
#[must_use]
pub fn trajectory_is_tool_call_only(cell: &TrajectoryCell) -> bool {
    cell.kind == TrajectoryCellKind::Message
        && cell.output_detail.as_ref().is_none_or(String::is_empty)
        && cell.thinking_detail.as_ref().is_none_or(String::is_empty)
        && cell.text == "Tool call only"
}

/// Builds the source-equivalent dense ledger display text.
///
/// # Errors
///
/// Returns the shared Markdown preview parser's diagnostic.
pub fn trajectory_record_display_text(cell: &TrajectoryCell) -> Result<String, String> {
    if trajectory_is_tool_call_only(cell) {
        return Ok(String::new());
    }
    if let Some(markdown) = &cell.preview_markdown {
        let preview = trajectory_preview_text(markdown)?;
        return Ok(if cell.text.is_empty() {
            preview
        } else if preview.is_empty() {
            cell.text.clone()
        } else {
            format!("{} · {preview}", cell.text)
        });
    }
    if !cell.text.is_empty() {
        return Ok(cell.text.clone());
    }
    let markdown = match cell.kind {
        TrajectoryCellKind::User | TrajectoryCellKind::Context => cell.input_detail.as_deref(),
        TrajectoryCellKind::Message => cell
            .output_detail
            .as_deref()
            .or(cell.thinking_detail.as_deref()),
        _ => None,
    };
    markdown.map_or_else(|| Ok(String::new()), trajectory_preview_text)
}

/// Builds the source-equivalent result preview.
///
/// # Errors
///
/// Returns the shared Markdown preview parser's diagnostic.
pub fn trajectory_record_result_text(cell: &TrajectoryCell) -> Result<Option<String>, String> {
    cell.result_preview_markdown.as_ref().map_or_else(
        || Ok(cell.result.clone()),
        |markdown| trajectory_preview_text(markdown).map(Some),
    )
}

/// Splits a Tool/Subtool display string on the source separator.
#[must_use]
pub fn trajectory_tool_call_text_parts(
    kind: TrajectoryCellKind,
    text: &str,
) -> Option<TrajectoryToolCallTextParts> {
    if !matches!(kind, TrajectoryCellKind::Tool | TrajectoryCellKind::Subtool) {
        return None;
    }
    Some(text.split_once(" · ").map_or_else(
        || TrajectoryToolCallTextParts {
            name: text.to_owned(),
            arguments: None,
        },
        |(name, arguments)| TrajectoryToolCallTextParts {
            name: name.to_owned(),
            arguments: Some(arguments.to_owned()),
        },
    ))
}

/// Resolves parent assistant and top-level Tool record indexes.
#[must_use]
pub fn trajectory_parent_records(
    records: &[TrajectoryTableRecord],
    record: &TrajectoryTableRecord,
) -> TrajectoryParentRecords {
    if !matches!(
        record.cell.kind,
        TrajectoryCellKind::Tool | TrajectoryCellKind::Subtool
    ) {
        return TrajectoryParentRecords::default();
    }
    let Some(at) = records
        .iter()
        .position(|candidate| candidate.cell.index == record.cell.index)
    else {
        return TrajectoryParentRecords::default();
    };
    let tool = if record.cell.kind == TrajectoryCellKind::Subtool {
        records[..at]
            .iter()
            .rev()
            .take_while(|candidate| {
                candidate.turn == record.turn && candidate.group == record.group
            })
            .find(|candidate| candidate.cell.kind == TrajectoryCellKind::Tool)
    } else {
        None
    };
    let parent_call_id = tool
        .and_then(|tool| tool.cell.call_id.as_deref())
        .or(record.cell.call_id.as_deref());
    let message = parent_call_id.and_then(|parent_call_id| {
        records.iter().find(|candidate| {
            candidate.turn == record.turn
                && candidate.cell.kind == TrajectoryCellKind::Message
                && candidate
                    .cell
                    .source_blocks
                    .iter()
                    .any(|block| block.call_id.as_deref() == Some(parent_call_id))
        })
    });
    TrajectoryParentRecords {
        message: message.map(|message| message.cell.index),
        tool: tool.map(|tool| tool.cell.index),
    }
}

/// Parses any non-null JSON object or array container.
#[must_use]
pub fn parse_trajectory_json_container(value: &str) -> Option<Value> {
    serde_json::from_str(value)
        .ok()
        .filter(|value| matches!(value, Value::Object(_) | Value::Array(_)))
}

/// Parses the exact object-shaped Tool schema accepted by the inspector.
#[must_use]
pub fn parse_trajectory_tool_schema(value: &str) -> Option<ParsedTrajectoryToolSchema> {
    let parsed: Value = serde_json::from_str(value).ok()?;
    let schema = parsed.as_object()?;
    let name = schema.get("name")?.as_str()?.to_owned();
    let description = schema.get("description")?.as_str()?.to_owned();
    let parameters = schema.get("parameters")?.clone();
    if !parameters.is_object() {
        return None;
    }
    Some(ParsedTrajectoryToolSchema {
        name,
        description,
        parameters,
    })
}

fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}
