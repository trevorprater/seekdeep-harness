//! Deterministic `TrajectoryView` request, fold, and partial projections.

use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use indexmap::IndexSet;
use serde_json::{Map, Value};

use crate::{
    TrajectoryRecordState, TrajectoryRequestNumber, TrajectoryRequestPurpose,
    TrajectorySearchIndex, TrajectoryTimelineMode, TrajectoryTurnModel, TrajectoryUsage,
    trajectory_record_id,
};

struct OrderedRequest<'a> {
    seq: u64,
    request: Option<&'a Value>,
    node: Option<&'a Value>,
}

/// Work requested when a new search layout snapshot arrives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrajectorySearchOffer {
    /// Layout content did not change, or a throttle is already pending.
    None,
    /// Initial indexing ran synchronously and consumers must refresh.
    Updated,
    /// Start one 3-second boundary timer.
    Schedule,
}

/// Target-portable owner for the View's throttled incremental search index.
#[derive(Debug, Default)]
pub struct TrajectoryViewSearchController {
    index: TrajectorySearchIndex,
    initialized: bool,
    timer_pending: bool,
    latest: Option<Rc<Vec<Vec<TrajectoryTurnModel>>>>,
}

impl TrajectoryViewSearchController {
    /// Creates an empty search controller.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Offers the latest finalized/partial layouts and requests boundary work.
    pub fn offer(&mut self, layouts: &Rc<Vec<Vec<TrajectoryTurnModel>>>) -> TrajectorySearchOffer {
        if self
            .latest
            .as_ref()
            .is_some_and(|latest| latest.as_ref() == layouts.as_ref())
        {
            return TrajectorySearchOffer::None;
        }
        self.latest = Some(layouts.clone());
        if !self.initialized {
            self.initialized = true;
            return if self.index.update(layouts) {
                TrajectorySearchOffer::Updated
            } else {
                TrajectorySearchOffer::None
            };
        }
        if self.timer_pending {
            TrajectorySearchOffer::None
        } else {
            self.timer_pending = true;
            TrajectorySearchOffer::Schedule
        }
    }

    /// Fires the pending boundary timer against the latest offered layouts.
    pub fn fire(&mut self) -> bool {
        if !self.timer_pending {
            return false;
        }
        self.timer_pending = false;
        self.latest
            .as_ref()
            .is_some_and(|latest| self.index.update(latest))
    }

    /// Cancels one pending boundary timer during View disposal.
    pub fn cancel(&mut self) {
        self.timer_pending = false;
    }

    /// Runs the current index's source query semantics.
    #[must_use]
    pub fn search(&self, query: &str) -> Option<IndexSet<String>> {
        self.index.search(query)
    }
}

/// Returns the largest current display index.
#[must_use]
pub fn last_trajectory_cell_index(turns: &[TrajectoryTurnModel]) -> usize {
    turns
        .iter()
        .flat_map(|turn| &turn.groups)
        .flat_map(|group| &group.cells)
        .map(|cell| cell.index)
        .max()
        .unwrap_or(0)
}

/// Produces the structure-only partial used by the timeline projection.
///
/// # Errors
///
/// Returns an unknown or malformed Assistant block diagnostic.
pub fn trajectory_timeline_partial(partial: Option<&Value>) -> Result<Option<Value>, String> {
    let Some(partial) = partial else {
        return Ok(None);
    };
    let blocks = partial
        .get("blocks")
        .and_then(Value::as_array)
        .ok_or_else(|| "trajectory partial omitted blocks".to_owned())?;
    let blocks = blocks
        .iter()
        .map(timeline_block)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(Value::Object(Map::from_iter([
        (
            "turn".to_owned(),
            partial
                .get("turn")
                .cloned()
                .ok_or_else(|| "trajectory partial omitted turn".to_owned())?,
        ),
        (
            "step".to_owned(),
            partial
                .get("step")
                .cloned()
                .ok_or_else(|| "trajectory partial omitted step".to_owned())?,
        ),
        ("blocks".to_owned(), Value::Array(blocks)),
    ]))))
}

/// Returns the NUL-delimited structural identity of one partial block list.
///
/// # Errors
///
/// Returns a malformed Tool-call identity diagnostic.
pub fn trajectory_partial_structure_signature(partial: Option<&Value>) -> Result<String, String> {
    let Some(partial) = partial else {
        return Ok(String::new());
    };
    let blocks = partial
        .get("blocks")
        .and_then(Value::as_array)
        .ok_or_else(|| "trajectory partial omitted blocks".to_owned())?;
    blocks
        .iter()
        .map(|block| {
            let kind = required_string(block, "kind")?;
            if kind != "tool-call" {
                return Ok(kind.to_owned());
            }
            Ok(format!(
                "{kind}:{}:{}",
                required_string(block, "callId")?,
                required_string(block, "name")?
            ))
        })
        .collect::<Result<Vec<_>, String>>()
        .map(|parts| parts.join("\0"))
}

/// Converts source request/node usage buckets to the table contract.
#[must_use]
pub fn trajectory_request_usage(value: Option<&Value>) -> Option<TrajectoryUsage> {
    let value = value?.as_object()?;
    Some(TrajectoryUsage {
        input: optional_u64(value, "inputTokens"),
        cache_read: optional_u64(value, "cacheReadTokens"),
        cache_write: optional_u64(value, "cacheWriteTokens"),
        output: optional_u64(value, "outputTokens"),
        reasoning: optional_u64(value, "reasoningTokens"),
    })
}

/// Adds only usage buckets present in either operand.
#[must_use]
pub fn add_trajectory_usage(
    total: Option<TrajectoryUsage>,
    usage: Option<TrajectoryUsage>,
) -> Option<TrajectoryUsage> {
    let usage = usage?;
    Some(TrajectoryUsage {
        input: optional_sum(total.and_then(|value| value.input), usage.input),
        cache_read: optional_sum(total.and_then(|value| value.cache_read), usage.cache_read),
        cache_write: optional_sum(total.and_then(|value| value.cache_write), usage.cache_write),
        output: optional_sum(total.and_then(|value| value.output), usage.output),
        reasoning: optional_sum(total.and_then(|value| value.reasoning), usage.reasoning),
    })
}

/// Derives session-global assistant and compaction request numbers in source order.
///
/// # Errors
///
/// Returns malformed request identities or unknown lifecycle states.
#[allow(clippy::too_many_lines)]
pub fn derive_trajectory_request_numbers(
    nodes: &[Value],
    requests: &[Value],
) -> Result<Vec<TrajectoryRequestNumber>, String> {
    let mut assistants_by_step = BTreeMap::<String, &Value>::new();
    for node in nodes {
        if node.get("kind").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let step = required_u64(node, "step")?;
        if step == 0 {
            continue;
        }
        let turn = required_u64(node, "turn")?;
        assistants_by_step.insert(format!("{turn}\0{step}"), node);
    }
    let requests_by_step = requests
        .iter()
        .filter(|request| request.get("purpose").and_then(Value::as_str) == Some("assistant"))
        .map(|request| {
            Ok((
                format!(
                    "{}\0{}",
                    required_u64(request, "turn")?,
                    required_u64(request, "step")?
                ),
                request,
            ))
        })
        .collect::<Result<BTreeMap<_, _>, String>>()?;

    let mut ordered = requests
        .iter()
        .map(|request| {
            let purpose = required_string(request, "purpose")?;
            let node = if purpose == "assistant" {
                assistants_by_step
                    .get(&format!(
                        "{}\0{}",
                        required_u64(request, "turn")?,
                        required_u64(request, "step")?
                    ))
                    .copied()
            } else {
                None
            };
            Ok(OrderedRequest {
                seq: required_u64(request, "startSeq")?,
                request: Some(request),
                node,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    for (key, node) in &assistants_by_step {
        if !requests_by_step.contains_key(key) {
            ordered.push(OrderedRequest {
                seq: required_u64(node, "seq")?,
                request: None,
                node: Some(node),
            });
        }
    }
    ordered.sort_by_key(|entry| entry.seq);

    let mut numbered = Vec::new();
    let mut cumulative = None;
    for (index, entry) in ordered.into_iter().enumerate() {
        let usage_source = entry
            .request
            .and_then(|request| request.get("usage"))
            .filter(|usage| !usage.is_null())
            .or_else(|| entry.node.and_then(|node| node.get("usage")));
        let usage = trajectory_request_usage(usage_source);
        cumulative = add_trajectory_usage(cumulative, usage);
        let purpose = entry
            .request
            .and_then(|request| request.get("purpose"))
            .and_then(Value::as_str);
        let number = u64::try_from(index + 1).map_err(|error| error.to_string())?;
        if purpose != Some("compaction") {
            let turn = entry
                .request
                .and_then(|request| request.get("turn"))
                .and_then(Value::as_u64)
                .or_else(|| {
                    entry
                        .node
                        .and_then(|node| node.get("turn"))
                        .and_then(Value::as_u64)
                })
                .ok_or_else(|| "assistant request omitted turn".to_owned())?;
            let step = entry
                .request
                .and_then(|request| request.get("step"))
                .and_then(Value::as_u64)
                .or_else(|| {
                    entry
                        .node
                        .and_then(|node| node.get("step"))
                        .and_then(Value::as_u64)
                })
                .ok_or_else(|| "assistant request omitted step".to_owned())?;
            numbered.push(TrajectoryRequestNumber {
                seq: Some(entry.seq),
                group: format!("Step {step}"),
                number,
                status: entry.request.and_then(request_status).transpose()?,
                started_at: entry
                    .request
                    .and_then(|value| finite_member(value, "startedAt")),
                completed_at: entry
                    .request
                    .and_then(|value| finite_member(value, "completedAt")),
                error: entry
                    .request
                    .and_then(|value| value.get("error"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                retry: entry
                    .request
                    .and_then(|value| value.get("retry"))
                    .and_then(Value::as_u64),
                max_retries: entry
                    .request
                    .and_then(|value| value.get("maxRetries"))
                    .and_then(Value::as_u64),
                retry_delay_ms: entry
                    .request
                    .and_then(|value| finite_member(value, "retryDelayMs")),
                result_seq: entry
                    .request
                    .and_then(|value| value.get("resultSeq"))
                    .and_then(Value::as_u64),
                provider: provenance_member(entry.request, entry.node, "provider"),
                model: provenance_member(entry.request, entry.node, "model"),
                request_config: first_present(entry.request, entry.node, "requestConfig"),
                usage,
                cumulative_usage: cumulative,
                purpose: TrajectoryRequestPurpose::Assistant,
                turn: Some(turn),
                step,
            });
            continue;
        }
        let Some(request) = entry.request else {
            return Err("compaction entry omitted request".to_owned());
        };
        numbered.push(TrajectoryRequestNumber {
            seq: Some(entry.seq),
            group: format!("Compaction {}", entry.seq),
            number,
            status: request_status(request).transpose()?,
            started_at: finite_member(request, "startedAt"),
            completed_at: finite_member(request, "completedAt"),
            error: request
                .get("error")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            retry: None,
            max_retries: None,
            retry_delay_ms: None,
            result_seq: Some(entry.seq),
            provider: provenance_member(Some(request), None, "provider"),
            model: provenance_member(Some(request), None, "model"),
            request_config: request.get("requestConfig").cloned(),
            usage,
            cumulative_usage: cumulative,
            purpose: TrajectoryRequestPurpose::Compaction,
            turn: request.get("turn").and_then(Value::as_u64),
            step: 0,
        });
    }
    Ok(numbered)
}

/// Derives the active timeline projection mode.
#[must_use]
pub const fn trajectory_timeline_mode(
    actual_duration: bool,
    actual_time: bool,
) -> TrajectoryTimelineMode {
    match (actual_duration, actual_time) {
        (false, false) => TrajectoryTimelineMode::Sequence,
        (false, true) => TrajectoryTimelineMode::Time,
        (true, false) => TrajectoryTimelineMode::Duration,
        (true, true) => TrajectoryTimelineMode::Actual,
    }
}

/// Returns turns with at least two visible non-System content records.
#[must_use]
pub fn trajectory_collapsible_turn_ids(turns: &[TrajectoryTurnModel]) -> Vec<u64> {
    turns
        .iter()
        .filter_map(|turn| {
            let turn_id = turn.turn?;
            (turn
                .groups
                .iter()
                .flat_map(|group| &group.cells)
                .filter(|cell| {
                    cell.request_only != Some(true)
                        && cell.kind != crate::TrajectoryCellKind::System
                })
                .count()
                > 1)
            .then_some(turn_id)
        })
        .collect()
}

/// Returns stable Assistant identities followed immediately by Tool/Subtool rows.
#[must_use]
pub fn trajectory_collapsible_assistant_ids(turns: &[TrajectoryTurnModel]) -> Vec<String> {
    let mut ids = Vec::new();
    for turn in turns {
        let cells = turn
            .groups
            .iter()
            .flat_map(|group| &group.cells)
            .collect::<Vec<_>>();
        for pair in cells.windows(2) {
            if pair[0].kind == crate::TrajectoryCellKind::Message
                && matches!(
                    pair[1].kind,
                    crate::TrajectoryCellKind::Tool | crate::TrajectoryCellKind::Subtool
                )
            {
                ids.push(trajectory_record_id(pair[0]));
            }
        }
    }
    ids
}

/// Returns whether every available fold identity is selected.
#[must_use]
pub fn all_trajectory_folds_selected<T: Ord>(available: &[T], selected: &BTreeSet<T>) -> bool {
    !available.is_empty() && available.iter().all(|value| selected.contains(value))
}

fn timeline_block(block: &Value) -> Result<Value, String> {
    let kind = required_string(block, "kind")?;
    match kind {
        "text" => Ok(serde_json::json!({"kind": "text", "text": ""})),
        "reasoning" => Ok(serde_json::json!({"kind": "reasoning", "text": ""})),
        "image" => Ok(block.clone()),
        "tool-call" => Ok(serde_json::json!({
            "kind": "tool-call",
            "callId": required_string(block, "callId")?,
            "name": required_string(block, "name")?,
            "argsRaw": "",
        })),
        "other" => Ok(serde_json::json!({"kind": "other", "block": null})),
        _ => Err(format!("unknown Assistant block kind {kind:?}")),
    }
}

fn request_status(request: &Value) -> Option<Result<TrajectoryRecordState, String>> {
    request
        .get("status")
        .and_then(Value::as_str)
        .map(|status| match status {
            "complete" => Ok(TrajectoryRecordState::Complete),
            "running" => Ok(TrajectoryRecordState::Running),
            "error" => Ok(TrajectoryRecordState::Error),
            _ => Err(format!("unknown trajectory request status {status:?}")),
        })
}

fn provenance_member(request: Option<&Value>, node: Option<&Value>, key: &str) -> Option<String> {
    request
        .and_then(|value| value.get("provenance"))
        .and_then(|value| value.get(key))
        .and_then(Value::as_str)
        .or_else(|| {
            node.and_then(|value| value.get("provenance"))
                .and_then(|value| value.get(key))
                .and_then(Value::as_str)
        })
        .map(ToOwned::to_owned)
}

fn first_present(request: Option<&Value>, node: Option<&Value>, key: &str) -> Option<Value> {
    request
        .and_then(|value| value.get(key))
        .filter(|value| !value.is_null())
        .or_else(|| node.and_then(|value| value.get(key)))
        .cloned()
}

fn optional_sum(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    (left.is_some() || right.is_some()).then(|| left.unwrap_or(0) + right.unwrap_or(0))
}

fn finite_member(value: &Value, key: &str) -> Option<f64> {
    value
        .get(key)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
}

fn optional_u64(value: &Map<String, Value>, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn required_string<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("trajectory value omitted string {key}"))
}

fn required_u64(value: &Value, key: &str) -> Result<u64, String> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("trajectory value omitted u64 {key}"))
}
