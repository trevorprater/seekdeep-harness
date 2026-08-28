//! Trajectory list fold from stage snapshot into Turn, group, and cell models.

use std::rc::Rc;

use indexmap::{IndexMap, IndexSet};
use serde_json::{Map, Value, json};
use url::Url;

use crate::{
    AssistantMetricDetail, TrajectoryCell, TrajectoryCellKind, TrajectoryGroupModel,
    TrajectorySequence, TrajectorySnapshot, TrajectorySourceBlock, TrajectoryTurnModel,
    format_elapsed_seconds,
};

#[derive(Clone)]
struct LaidCell {
    cell: TrajectoryCell,
    abs_time: Option<f64>,
    tool_name: Option<String>,
    call_id: Option<String>,
    sub_calls: Vec<Value>,
}

struct LaidGroup {
    title: String,
    laid: Vec<LaidCell>,
}

#[derive(Default)]
struct TurnBucket {
    groups: Vec<LaidGroup>,
}

enum LayoutEntry {
    Node { node: Value, node_index: usize },
    Compaction { request: Value },
    System { request: Value, change: Value },
    Request { request: Value },
}

impl LayoutEntry {
    fn order(&self) -> f64 {
        match self {
            Self::Node { node, .. } => number(node, "seq").unwrap_or_default(),
            Self::Compaction { request } | Self::Request { request } => {
                number(request, "startSeq").unwrap_or_default()
            }
            Self::System { change, .. } if string(change, "kind") == Some("initial") => {
                f64::NEG_INFINITY
            }
            Self::System { change, .. } => number(change, "seq").unwrap_or_default(),
        }
    }
}

/// Folds a typed trajectory snapshot into Turn and group rows.
#[must_use]
#[allow(clippy::too_many_lines)] // One chronological fold mirrors the source's closed node union.
pub fn derive_trajectory_layout(input: &TrajectorySnapshot) -> Vec<TrajectoryTurnModel> {
    let nodes = &input.event_nodes;
    let result_by_call = index_results(nodes);
    let mut call_by_id = result_by_call.clone();
    for call in &input.running_calls {
        if let Some(call_id) = string(call, "callId") {
            call_by_id.insert(call_id.to_owned(), call.clone());
        }
    }
    let emitted_call_ids = index_assistant_call_ids(nodes);
    let following_assistants = index_following_assistants(nodes);
    let mut call_start_by_id = IndexMap::<String, f64>::new();
    for result in result_by_call.values() {
        if let (Some(call_id), Some(started_at)) =
            (string(result, "callId"), finite_member(result, "callTime"))
        {
            call_start_by_id.insert(call_id.to_owned(), started_at);
        }
    }
    for call in &input.running_calls {
        if let (Some(call_id), Some(started_at)) =
            (string(call, "callId"), finite_member(call, "time"))
        {
            call_start_by_id.insert(call_id.to_owned(), started_at);
        }
    }

    let mut turns = IndexMap::<i64, TurnBucket>::new();
    let mut standalone_compactions = Vec::<TurnBucket>::new();
    let mut cell_index = 0_usize;
    let mut previous_abs_time = None;
    let mut last_assistant_turn = None::<i64>;

    let mut represented_requests = IndexSet::<String>::new();
    for node in nodes {
        if string(node, "kind") == Some("assistant")
            && integer(node, "step").is_some_and(|step| step > 0)
        {
            represented_requests.insert(step_key(
                integer(node, "turn").unwrap_or_default(),
                integer(node, "step").unwrap_or_default(),
            ));
        }
    }
    if let Some(partial) = input.partial.as_ref()
        && integer(partial, "step").is_some_and(|step| step > 0)
    {
        represented_requests.insert(step_key(
            integer(partial, "turn").unwrap_or_default(),
            integer(partial, "step").unwrap_or_default(),
        ));
    }
    for call in &input.running_calls {
        if integer(call, "step").is_some_and(|step| step > 0) {
            represented_requests.insert(step_key(
                integer(call, "turn").unwrap_or_default(),
                integer(call, "step").unwrap_or_default(),
            ));
        }
    }

    let mut entries = Vec::<LayoutEntry>::new();
    entries.extend(
        nodes
            .iter()
            .cloned()
            .enumerate()
            .map(|(node_index, node)| LayoutEntry::Node { node, node_index }),
    );
    for request in &input.requests {
        match string(request, "purpose") {
            Some("compaction") => entries.push(LayoutEntry::Compaction {
                request: request.clone(),
            }),
            Some("assistant") => {
                if let (Some(change), Some(_)) =
                    (request.get("promptChange"), request.get("prompt"))
                {
                    entries.push(LayoutEntry::System {
                        request: request.clone(),
                        change: change.clone(),
                    });
                }
                let key = step_key(
                    integer(request, "turn").unwrap_or_default(),
                    integer(request, "step").unwrap_or_default(),
                );
                if !represented_requests.contains(&key) {
                    entries.push(LayoutEntry::Request {
                        request: request.clone(),
                    });
                }
            }
            Some(_) | None => {}
        }
    }
    entries.sort_by(|left, right| left.order().total_cmp(&right.order()));

    for entry in entries {
        match entry {
            LayoutEntry::Request { request } => {
                let turn = integer(&request, "turn").unwrap_or_default();
                let step = integer(&request, "step").unwrap_or_default();
                cell_index += 1;
                let started_at = finite_member(&request, "startedAt");
                let completed_at = finite_member(&request, "completedAt");
                let mut cell = TrajectoryCell::new(cell_index, TrajectoryCellKind::Message, "");
                cell.source_seq = safe_seq(&request, "startSeq");
                cell.request_only = Some(true);
                cell.time_seconds =
                    completed_at.and_then(|later| duration_seconds(later, started_at));
                cell.started_at = started_at;
                if string(&request, "status") == Some("error") {
                    cell.is_error = Some(true);
                }
                push_step(
                    &mut turns,
                    turn,
                    step,
                    vec![LaidCell::plain(cell, started_at)],
                );
                previous_abs_time = completed_at.or(started_at).or(previous_abs_time);
            }
            LayoutEntry::System { request, change } => {
                let turn = if string(&change, "kind") == Some("initial") {
                    first_visible_turn(nodes, input.partial.as_ref())
                } else {
                    enclosing_prompt_turn(
                        nodes,
                        number(&change, "seq").unwrap_or_default(),
                        input.partial.as_ref(),
                    )
                };
                cell_index += 1;
                let mut cell = TrajectoryCell::new(
                    cell_index,
                    TrajectoryCellKind::System,
                    prompt_change_label(&change),
                );
                cell.source_seq = safe_seq(&change, "seq");
                cell.prompt_detail = request.get("prompt").cloned();
                cell.previous_prompt_detail = change.get("previous").cloned();
                cell.time_seconds = Some(0.0);
                cell.started_at = finite_member(&change, "time");
                push_message(
                    &mut turns,
                    turn,
                    LaidCell::plain(cell, finite_member(&change, "time")),
                );
                previous_abs_time = finite_member(&change, "time").or(previous_abs_time);
            }
            LayoutEntry::Compaction { request } => {
                cell_index += 1;
                let raw_output = request.get("rawOutput").or_else(|| request.get("summary"));
                let thinking_detail = raw_output.map_or_else(String::new, detail_reasoning);
                let status = string(&request, "status").unwrap_or_default();
                let summary = request.get("summary");
                let text = match status {
                    "running" => "Compacting context…".to_owned(),
                    "error" => string(&request, "error")
                        .unwrap_or("Compaction failed")
                        .to_owned(),
                    _ if summary.is_none() => "Context compacted".to_owned(),
                    _ => String::new(),
                };
                let mut cell = TrajectoryCell::new(cell_index, TrajectoryCellKind::Compacted, text);
                cell.source_seq = safe_seq(&request, "startSeq");
                if status == "complete" {
                    cell.preview_markdown = summary.and_then(preview_content);
                }
                if let Some(summary) = summary {
                    cell.output_detail = Some(detail_content(summary));
                    cell.output_blocks = blocks(summary).iter().map(source_block).collect();
                }
                if !thinking_detail.is_empty() {
                    cell.thinking_detail = Some(thinking_detail);
                }
                if let Some(raw_output) = raw_output {
                    cell.source_blocks = blocks(raw_output).iter().map(source_block).collect();
                }
                if status == "error" {
                    cell.is_error = Some(true);
                }
                let started_at = finite_member(&request, "startedAt");
                let completed_at = finite_member(&request, "completedAt");
                cell.time_seconds =
                    completed_at.and_then(|later| duration_seconds(later, started_at));
                cell.started_at = started_at;
                attach_usage(&mut cell, request.get("usage"));
                let compaction = TurnBucket {
                    groups: vec![LaidGroup {
                        title: format!("Compaction {}", display_number(&request, "startSeq")),
                        laid: vec![LaidCell::plain(cell, started_at)],
                    }],
                };
                if request.get("turn").is_none_or(Value::is_null) {
                    standalone_compactions.push(compaction);
                } else {
                    turns
                        .entry(integer(&request, "turn").unwrap_or_default())
                        .or_default()
                        .groups
                        .extend(compaction.groups);
                }
                previous_abs_time = completed_at.or(started_at).or(previous_abs_time);
            }
            LayoutEntry::Node { node, node_index } => {
                fold_node(
                    &node,
                    node_index,
                    nodes,
                    &following_assistants,
                    &input.event_locations,
                    input.partial.as_ref(),
                    &result_by_call,
                    &call_start_by_id,
                    &call_by_id,
                    &emitted_call_ids,
                    &mut turns,
                    &mut cell_index,
                    &mut previous_abs_time,
                    &mut last_assistant_turn,
                );
            }
        }
    }

    if let Some(partial) = input.partial.as_ref() {
        let fake = json!({
            "kind": "assistant",
            "seq": 9_007_199_254_740_991_f64,
            "time": 0,
            "turn": partial.get("turn").cloned().unwrap_or(Value::Null),
            "step": partial.get("step").cloned().unwrap_or(Value::Null),
            "blocks": partial.get("blocks").cloned().unwrap_or_else(|| json!([])),
        });
        let laid = with_sub_calls(expand_assistant(
            &fake,
            cell_index + 1,
            previous_abs_time,
            &result_by_call,
            &call_start_by_id,
            &call_by_id,
            true,
        ));
        let turn = integer(partial, "turn").unwrap_or_default();
        let step = integer(partial, "step").unwrap_or_default();
        if step > 0 {
            push_step(&mut turns, turn, step, laid.clone());
        } else {
            for cell in laid.clone() {
                push_message(&mut turns, turn, cell);
            }
        }
        if let Some(last) = laid.last() {
            cell_index = last.cell.index;
        }
    }

    let seen_calls = collect_call_ids(&turns);
    for call in &input.running_calls {
        let Some(call_id) = string(call, "callId") else {
            continue;
        };
        if seen_calls.contains(call_id) {
            continue;
        }
        cell_index += 1;
        let name = string(call, "name").unwrap_or_default();
        let args = string(call, "argsRaw").unwrap_or_default();
        let mut cell = TrajectoryCell::new(cell_index, TrajectoryCellKind::Tool, name);
        cell.preview_markdown = (!args.is_empty()).then(|| args.to_owned());
        cell.input_detail = Some(args.to_owned());
        cell.call_id = Some(call_id.to_owned());
        cell.time_seconds = None;
        cell.started_at = finite_member(call, "time");
        let subs = blocks_member(call, "subCalls").to_vec();
        let mut laid = vec![LaidCell {
            abs_time: None,
            tool_name: Some(name.to_owned()),
            call_id: Some(call_id.to_owned()),
            sub_calls: subs.clone(),
            cell,
        }];
        for child in expand_sub_calls(&subs, cell_index) {
            cell_index = child.cell.index;
            laid.push(child);
        }
        let turn = integer(call, "turn").unwrap_or_default();
        let step = integer(call, "step").unwrap_or_default();
        if step > 0 {
            push_step(&mut turns, turn, step, laid);
        } else {
            for cell in laid {
                push_message(&mut turns, turn, cell);
            }
        }
    }

    if let Some(prologue) = turns.shift_remove(&0) {
        let mut first = turns.shift_remove(&1).unwrap_or_default();
        let mut groups = prologue.groups;
        groups.append(&mut first.groups);
        turns.insert_before(0, 1, TurnBucket { groups });
    }

    for bucket in turns.values_mut().chain(&mut standalone_compactions) {
        for group in &mut bucket.groups {
            for laid in &mut group.laid {
                attach_tool_schema(laid, &input.call_schemas);
            }
        }
    }

    let mut output = turns
        .into_iter()
        .map(|(turn, bucket)| to_turn_model(Some(turn), bucket))
        .chain(
            standalone_compactions
                .into_iter()
                .map(|bucket| to_turn_model(None, bucket)),
        )
        .collect::<Vec<_>>();
    output.sort_by_key(first_cell_index);
    output
}

impl LaidCell {
    fn plain(cell: TrajectoryCell, abs_time: Option<f64>) -> Self {
        Self {
            cell,
            abs_time,
            tool_name: None,
            call_id: None,
            sub_calls: Vec::new(),
        }
    }
}

fn push_message(turns: &mut IndexMap<i64, TurnBucket>, turn: i64, laid: LaidCell) {
    let groups = &mut turns.entry(turn).or_default().groups;
    if groups.last().is_some_and(|group| group.title == "Message") {
        groups.last_mut().expect("present").laid.push(laid);
    } else {
        groups.push(LaidGroup {
            title: "Message".to_owned(),
            laid: vec![laid],
        });
    }
}

fn push_step(turns: &mut IndexMap<i64, TurnBucket>, turn: i64, step: i64, laid: Vec<LaidCell>) {
    if laid.is_empty() {
        return;
    }
    let title = format!("Step {step}");
    let groups = &mut turns.entry(turn).or_default().groups;
    if let Some(group) = groups.iter_mut().find(|group| group.title == title) {
        group.laid.extend(laid);
    } else {
        groups.push(LaidGroup { title, laid });
    }
}

fn push_step_input(
    turns: &mut IndexMap<i64, TurnBucket>,
    turn: i64,
    step: i64,
    laid: Vec<LaidCell>,
) {
    if laid.is_empty() {
        return;
    }
    let title = format!("Step {step}");
    let groups = &mut turns.entry(turn).or_default().groups;
    if let Some(group) = groups.iter_mut().find(|group| group.title == title) {
        let at = group
            .laid
            .iter()
            .position(|entry| entry.cell.request_only == Some(true))
            .unwrap_or(group.laid.len());
        group.laid.splice(at..at, laid);
    } else {
        groups.push(LaidGroup { title, laid });
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn fold_node(
    node: &Value,
    node_index: usize,
    nodes: &[Value],
    following_assistants: &[Option<Value>],
    event_locations: &IndexMap<TrajectorySequence, crate::TrajectoryLocation>,
    partial: Option<&Value>,
    result_by_call: &IndexMap<String, Value>,
    call_start_by_id: &IndexMap<String, f64>,
    call_by_id: &IndexMap<String, Value>,
    emitted_call_ids: &IndexSet<String>,
    turns: &mut IndexMap<i64, TurnBucket>,
    cell_index: &mut usize,
    previous_abs_time: &mut Option<f64>,
    last_assistant_turn: &mut Option<i64>,
) {
    match string(node, "kind") {
        Some("user") => {
            let turn = enclosing_user_turn(
                following_assistants
                    .get(node_index)
                    .and_then(Option::as_ref),
                partial,
                *last_assistant_turn,
            );
            *cell_index += 1;
            let mut cell = input_cell(node, *cell_index);
            cell.kind = TrajectoryCellKind::User;
            cell.opens_turn = Some(true);
            push_message(
                turns,
                turn,
                LaidCell::plain(cell, finite_member(node, "time")),
            );
            *previous_abs_time = finite_member(node, "time").or(*previous_abs_time);
        }
        Some("steering") => {
            let location = number(node, "seq")
                .and_then(|seq| event_locations.get(&TrajectorySequence::new(seq)));
            let (turn, step) = steering_placement(
                following_assistants
                    .get(node_index)
                    .and_then(Option::as_ref),
                partial,
                *last_assistant_turn,
                location,
            );
            *cell_index += 1;
            let mut cell = input_cell(node, *cell_index);
            cell.kind = TrajectoryCellKind::User;
            let laid = LaidCell::plain(cell, finite_member(node, "time"));
            if let Some(step) = step {
                push_step_input(turns, turn, step, vec![laid]);
            } else {
                push_message(turns, turn, laid);
            }
            *previous_abs_time = finite_member(node, "time").or(*previous_abs_time);
        }
        Some("assistant") => {
            let laid = with_sub_calls(expand_assistant(
                node,
                *cell_index + 1,
                *previous_abs_time,
                result_by_call,
                call_start_by_id,
                call_by_id,
                false,
            ));
            let turn = integer(node, "turn").unwrap_or_default();
            let step = integer(node, "step").unwrap_or_default();
            if step > 0 {
                push_step(turns, turn, step, laid.clone());
            } else {
                for cell in laid.clone() {
                    push_message(turns, turn, cell);
                }
            }
            if let Some(last) = laid.last() {
                *cell_index = last.cell.index;
            }
            *previous_abs_time = finite_member(node, "time").or(*previous_abs_time);
            *last_assistant_turn = Some(turn);
        }
        Some("context") => {
            let turn = enclosing_user_turn(
                following_assistants
                    .get(node_index)
                    .and_then(Option::as_ref),
                partial,
                *last_assistant_turn,
            );
            *cell_index += 1;
            let mut cell = input_cell(node, *cell_index);
            cell.kind = TrajectoryCellKind::Context;
            push_message(
                turns,
                turn,
                LaidCell::plain(cell, finite_member(node, "time")),
            );
            *previous_abs_time = finite_member(node, "time").or(*previous_abs_time);
        }
        Some("compaction") => {
            *previous_abs_time = finite_member(node, "time").or(*previous_abs_time);
        }
        Some("tool-result") => {
            let call_id = string(node, "callId").unwrap_or_default();
            if !emitted_call_ids.contains(call_id) {
                let tool_name = node
                    .get("call")
                    .filter(|call| !call.is_null())
                    .and_then(|call| string(call, "name"));
                let result_preview = summarize_result(node);
                *cell_index += 1;
                let mut cell =
                    TrajectoryCell::new(*cell_index, TrajectoryCellKind::Tool, String::new());
                cell.source_seq = safe_seq(node, "seq");
                if let Some(call) = node.get("call").filter(|call| !call.is_null()) {
                    summarize_call_into(
                        &mut cell,
                        string(call, "name").unwrap_or_default(),
                        string(call, "argsRaw").unwrap_or_default(),
                    );
                    cell.input_detail = string(call, "argsRaw").map(ToOwned::to_owned);
                } else {
                    result_as_text(&mut cell, Some(&result_preview));
                }
                cell.output_detail = Some(detail_result(node));
                cell.output_blocks = blocks_member(node, "content")
                    .iter()
                    .map(source_block)
                    .collect();
                apply_result_preview(&mut cell, Some(&result_preview));
                cell.call_id = Some(call_id.to_owned());
                cell.is_error = bool_member(node, "isError").then_some(true);
                cell.time_seconds = finite_member(node, "time")
                    .and_then(|later| duration_seconds(later, finite_member(node, "callTime")));
                cell.started_at = finite_member(node, "callTime");
                let subs = blocks_member(node, "subCalls").to_vec();
                let mut laid = vec![LaidCell {
                    abs_time: finite_member(node, "callTime")
                        .or_else(|| finite_member(node, "time")),
                    tool_name: tool_name.map(ToOwned::to_owned),
                    call_id: Some(call_id.to_owned()),
                    sub_calls: subs.clone(),
                    cell,
                }];
                for child in expand_sub_calls(&subs, *cell_index) {
                    *cell_index = child.cell.index;
                    laid.push(child);
                }
                push_step(turns, 0, 1, laid);
            }
            *previous_abs_time = finite_member(node, "time").or(*previous_abs_time);
        }
        Some(_) | None => {}
    }
    let _ = nodes;
}

fn input_cell(node: &Value, index: usize) -> TrajectoryCell {
    let mut cell = TrajectoryCell::new(index, TrajectoryCellKind::User, "");
    let content = node.get("content").unwrap_or(&Value::Null);
    cell.preview_markdown = preview_content(content);
    cell.source_seq = safe_seq(node, "seq");
    cell.message_source = node.get("source").cloned();
    cell.input_detail = Some(detail_content(content));
    cell.source_blocks = blocks(content).iter().map(source_block).collect();
    cell.time_seconds = Some(0.0);
    cell.started_at = finite_member(node, "time");
    cell
}

#[allow(clippy::too_many_lines)] // One Assistant record and its Tool blocks expand together.
fn expand_assistant(
    node: &Value,
    start_index: usize,
    previous_abs_time: Option<f64>,
    results: &IndexMap<String, Value>,
    call_starts: &IndexMap<String, f64>,
    calls: &IndexMap<String, Value>,
    streaming: bool,
) -> Vec<LaidCell> {
    let node_blocks = blocks_member(node, "blocks");
    if streaming && node_blocks.is_empty() {
        return Vec::new();
    }
    let mut output = Vec::new();
    let mut index = start_index - 1;
    let recorded_start = node
        .get("timing")
        .and_then(|timing| finite_member(timing, "stepStartTime"));
    let message_duration = if streaming {
        None
    } else {
        finite_member(node, "time")
            .and_then(|later| duration_seconds(later, recorded_start.or(previous_abs_time)))
    };
    let message_text = assistant_text(node_blocks, "text", streaming);
    let thinking_text = assistant_text(node_blocks, "reasoning", streaming);
    index += 1;
    let turn = integer(node, "turn").unwrap_or_default();
    let step = integer(node, "step").unwrap_or_default();
    let mut message = TrajectoryCell::new(index, TrajectoryCellKind::Message, "");
    message.record_id = Some(format!("assistant\0{turn}\0{step}"));
    message.source_seq = safe_seq(node, "seq");
    message.text = if message_text.is_empty() && thinking_text.is_empty() {
        summarize_assistant_activity(node_blocks)
    } else {
        String::new()
    };
    message.preview_markdown = if !message_text.is_empty() {
        Some(message_text.clone())
    } else if !thinking_text.is_empty() {
        Some(thinking_text.clone())
    } else {
        None
    };
    message.output_detail = (!message_text.is_empty()).then_some(message_text);
    message.thinking_detail = (!thinking_text.is_empty()).then_some(thinking_text);
    message.source_blocks = node_blocks.iter().map(assistant_source_block).collect();
    message.time_seconds = message_duration;
    message.started_at = recorded_start;
    attach_usage(&mut message, node.get("usage"));
    let usage = node.get("usage");
    message.assistant_metrics = Some(AssistantMetricDetail {
        timing_recorded: node.get("timing").is_some(),
        step_start_time: node
            .get("timing")
            .and_then(|timing| number(timing, "stepStartTime")),
        first_token_time: node
            .get("timing")
            .and_then(|timing| number(timing, "firstTokenTime")),
        completed_time: (!streaming).then(|| finite_member(node, "time")).flatten(),
        usage_provided: usage.is_some(),
        output_tokens: usage
            .and_then(|usage| usage.get("outputTokens"))
            .and_then(Value::as_u64),
    });
    output.push(LaidCell::plain(
        message,
        (!streaming).then(|| finite_member(node, "time")).flatten(),
    ));

    for block in node_blocks {
        if string(block, "kind") != Some("tool-call") {
            continue;
        }
        let call_id = string(block, "callId").unwrap_or_default();
        let result = results.get(call_id);
        let call_abs = call_starts.get(call_id).copied();
        let call_block = calls.get(call_id);
        let result_preview = result.map(summarize_result);
        index += 1;
        let name = string(block, "name").unwrap_or_default();
        let args = string(block, "argsRaw").unwrap_or_default();
        let mut cell = TrajectoryCell::new(index, TrajectoryCellKind::Tool, name);
        cell.preview_markdown = (!args.is_empty()).then(|| args.to_owned());
        cell.input_detail = Some(args.to_owned());
        cell.call_id = Some(call_id.to_owned());
        if let Some(result) = result {
            cell.output_detail = Some(detail_result(result));
            cell.output_blocks = blocks_member(result, "content")
                .iter()
                .map(source_block)
                .collect();
            apply_result_preview(&mut cell, result_preview.as_ref());
            cell.is_error = bool_member(result, "isError").then_some(true);
        }
        cell.time_seconds = if streaming {
            None
        } else {
            result.and_then(|result| {
                finite_member(result, "time")
                    .and_then(|later| duration_seconds(later, finite_member(result, "callTime")))
            })
        };
        cell.started_at = call_abs;
        output.push(LaidCell {
            cell,
            abs_time: call_abs,
            tool_name: Some(name.to_owned()),
            call_id: Some(call_id.to_owned()),
            sub_calls: call_block
                .map_or_else(Vec::new, |call| blocks_member(call, "subCalls").to_vec()),
        });
    }
    output
}

fn with_sub_calls(laid: Vec<LaidCell>) -> Vec<LaidCell> {
    if !laid.iter().any(|cell| !cell.sub_calls.is_empty()) {
        return laid;
    }
    let mut output = Vec::new();
    let mut index = laid
        .first()
        .map_or(0, |cell| cell.cell.index.saturating_sub(1));
    for mut cell in laid {
        index += 1;
        cell.cell.index = index;
        let subs = cell.sub_calls.clone();
        output.push(cell);
        for child in expand_sub_calls(&subs, index) {
            index = child.cell.index;
            output.push(child);
        }
    }
    output
}

fn expand_sub_calls(subs: &[Value], start_index: usize) -> Vec<LaidCell> {
    let mut output = Vec::new();
    let mut index = start_index;
    for sub in subs {
        let settled = sub.get("kind").is_some();
        let call_id = string(sub, "callId").unwrap_or_default();
        let name = if settled {
            sub.get("call")
                .filter(|call| !call.is_null())
                .and_then(|call| string(call, "name"))
                .unwrap_or(call_id)
        } else {
            string(sub, "name").unwrap_or_default()
        };
        let args = if settled {
            sub.get("call")
                .filter(|call| !call.is_null())
                .and_then(|call| string(call, "argsRaw"))
        } else {
            string(sub, "argsRaw")
        };
        let result_preview = settled.then(|| summarize_result(sub));
        index += 1;
        let mut cell = TrajectoryCell::new(index, TrajectoryCellKind::Subtool, name);
        if let Some(args) = args {
            cell.preview_markdown = (!args.is_empty()).then(|| args.to_owned());
            cell.input_detail = Some(args.to_owned());
        } else {
            result_as_text(&mut cell, result_preview.as_ref());
        }
        cell.call_id = Some(call_id.to_owned());
        if settled {
            cell.output_detail = Some(detail_result(sub));
            cell.output_blocks = blocks_member(sub, "content")
                .iter()
                .map(source_block)
                .collect();
            apply_result_preview(&mut cell, result_preview.as_ref());
            cell.is_error = bool_member(sub, "isError").then_some(true);
        }
        let started_at = if settled {
            finite_member(sub, "callTime")
        } else {
            finite_member(sub, "time")
        };
        cell.time_seconds = if settled {
            finite_member(sub, "time")
                .and_then(|later| duration_seconds(later, finite_member(sub, "callTime")))
        } else {
            None
        };
        cell.started_at = started_at;
        output.push(LaidCell {
            cell,
            abs_time: started_at,
            tool_name: Some(name.to_owned()),
            call_id: Some(call_id.to_owned()),
            sub_calls: Vec::new(),
        });
        let children = blocks_member(sub, "subCalls");
        for child in expand_sub_calls(children, index) {
            index = child.cell.index;
            output.push(child);
        }
    }
    output
}

/// Appends changing in-flight Assistant cells while sharing unaffected Turn identities.
#[must_use]
pub fn append_trajectory_partial_layout(
    turns: &[Rc<TrajectoryTurnModel>],
    partial: Option<&Value>,
    last_index: usize,
) -> Vec<Rc<TrajectoryTurnModel>> {
    let Some(partial) = partial else {
        return turns.to_vec();
    };
    let snapshot = TrajectorySnapshot {
        partial: Some(partial.clone()),
        ..TrajectorySnapshot::default()
    };
    let Some(mut streamed) = derive_trajectory_layout(&snapshot).into_iter().next() else {
        return turns.to_vec();
    };
    for group in &mut streamed.groups {
        for cell in &mut group.cells {
            cell.index += last_index;
        }
    }
    let Some(turn_index) = turns.iter().position(|turn| turn.turn == streamed.turn) else {
        let mut output = turns.to_vec();
        output.push(Rc::new(streamed));
        return output;
    };
    let current = turns[turn_index].as_ref();
    let mut groups = current.groups.clone();
    for streamed_group in streamed.groups {
        if let Some(group_index) = groups
            .iter()
            .position(|group| group.title == streamed_group.title)
        {
            let streamed_ids = streamed_group
                .cells
                .iter()
                .filter_map(|cell| cell.call_id.clone())
                .collect::<IndexSet<_>>();
            let mut cells = groups[group_index]
                .cells
                .iter()
                .filter(|cell| {
                    cell.request_only != Some(true)
                        && cell
                            .call_id
                            .as_ref()
                            .is_none_or(|call_id| !streamed_ids.contains(call_id))
                })
                .cloned()
                .collect::<Vec<_>>();
            cells.extend(streamed_group.cells);
            groups[group_index] = TrajectoryGroupModel {
                title: streamed_group.title,
                description: streamed_group.description,
                cells,
            };
        } else {
            groups.push(streamed_group);
        }
    }
    let mut output = turns.to_vec();
    output[turn_index] = Rc::new(TrajectoryTurnModel {
        turn: current.turn,
        groups,
    });
    output
}

fn to_turn_model(turn: Option<i64>, bucket: TurnBucket) -> TrajectoryTurnModel {
    TrajectoryTurnModel {
        turn: turn.and_then(|turn| u64::try_from(turn).ok()),
        groups: bucket
            .groups
            .into_iter()
            .map(|group| TrajectoryGroupModel {
                title: group.title,
                description: group_description(&group.laid),
                cells: group.laid.into_iter().map(|cell| cell.cell).collect(),
            })
            .collect(),
    }
}

fn first_cell_index(turn: &TrajectoryTurnModel) -> usize {
    turn.groups
        .iter()
        .flat_map(|group| &group.cells)
        .map(|cell| cell.index)
        .min()
        .unwrap_or(usize::MAX)
}

fn group_description(laid: &[LaidCell]) -> Option<String> {
    let mut parts = Vec::new();
    let mut times = Vec::new();
    for cell in laid {
        let Some(abs_time) = cell.abs_time.filter(|time| time.is_finite()) else {
            continue;
        };
        times.push(abs_time);
        if matches!(cell.cell.kind, TrajectoryCellKind::Tool)
            && let Some(duration) = cell
                .cell
                .time_seconds
                .filter(|duration| duration.is_finite())
        {
            times.push(abs_time + duration * 1_000.0);
        }
    }
    if times.len() >= 2 {
        let minimum = times.iter().copied().fold(f64::INFINITY, f64::min);
        let maximum = times.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        parts.push(format_elapsed_seconds(Some((maximum - minimum) / 1_000.0)));
    } else if let Some(time) = times.first()
        && let Some(duration) = laid
            .iter()
            .find(|cell| cell.abs_time == Some(*time))
            .and_then(|cell| cell.cell.time_seconds)
            .filter(|duration| duration.is_finite())
    {
        parts.push(format_elapsed_seconds(Some(duration)));
    }
    let mut tools = IndexMap::<String, usize>::new();
    for cell in laid {
        if matches!(cell.cell.kind, TrajectoryCellKind::Tool)
            && let Some(name) = &cell.tool_name
        {
            *tools.entry(name.clone()).or_default() += 1;
        }
    }
    parts.extend(tools.into_iter().map(|(name, count)| {
        if count > 1 {
            format!("{name}×{count}")
        } else {
            name
        }
    }));
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn attach_tool_schema(laid: &mut LaidCell, schemas: &IndexMap<String, Value>) {
    let Some(schema) = laid
        .call_id
        .as_ref()
        .and_then(|call_id| schemas.get(call_id))
    else {
        return;
    };
    laid.cell.schema_detail = serde_json::to_string_pretty(schema).ok();
}

fn attach_usage(cell: &mut TrajectoryCell, usage: Option<&Value>) {
    let Some(usage) = usage else {
        return;
    };
    cell.input = usage.get("inputTokens").and_then(Value::as_u64);
    cell.cache_read = usage.get("cacheReadTokens").and_then(Value::as_u64);
    cell.cache_write = usage.get("cacheWriteTokens").and_then(Value::as_u64);
    cell.output = usage.get("outputTokens").and_then(Value::as_u64);
    cell.think = usage.get("reasoningTokens").and_then(Value::as_u64);
}

fn summarize_assistant_activity(blocks: &[Value]) -> String {
    if blocks
        .iter()
        .any(|block| string(block, "kind") == Some("tool-call"))
    {
        "Tool call only".to_owned()
    } else {
        String::new()
    }
}

fn prompt_change_label(change: &Value) -> &'static str {
    match string(change, "kind") {
        Some("initial") => "Initial System Prompt",
        Some("system") => "System Prompt Updated",
        Some("tools") => "Tools Updated",
        Some(_) | None => "System Prompt and Tools Updated",
    }
}

fn assistant_source_block(block: &Value) -> TrajectorySourceBlock {
    match string(block, "kind") {
        Some("text") => source_text("text", string(block, "text").unwrap_or_default()),
        Some("reasoning") => source_text("thinking", string(block, "text").unwrap_or_default()),
        Some("tool-call") => TrajectorySourceBlock {
            kind: "tool-call".to_owned(),
            content: string(block, "argsRaw").unwrap_or_default().to_owned(),
            image_src: None,
            image_alt: None,
            call_id: string(block, "callId").map(ToOwned::to_owned),
            tool_name: string(block, "name").map(ToOwned::to_owned),
        },
        Some("image") => TrajectorySourceBlock {
            kind: "image".to_owned(),
            content: stringify_source_value(block.get("attachment").unwrap_or(&Value::Null)),
            image_src: None,
            image_alt: None,
            call_id: None,
            tool_name: None,
        },
        Some("other") => source_block(block.get("block").unwrap_or(&Value::Null)),
        Some(_) | None => source_block(block),
    }
}

fn source_block(value: &Value) -> TrajectorySourceBlock {
    let Some(block) = value.as_object() else {
        return source_text("unknown", &stringify_source_value(value));
    };
    let kind = block
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if let Some(text) = block.get("text").and_then(Value::as_str) {
        return source_text(
            if kind == "reasoning" {
                "thinking"
            } else {
                kind
            },
            text,
        );
    }
    let image_src = source_image(block);
    TrajectorySourceBlock {
        kind: kind.to_owned(),
        content: image_src
            .as_ref()
            .map_or_else(|| stringify_source_value(value), |_| String::new()),
        image_src,
        image_alt: block
            .get("alt")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        call_id: None,
        tool_name: None,
    }
}

fn source_text(kind: &str, content: &str) -> TrajectorySourceBlock {
    TrajectorySourceBlock {
        kind: kind.to_owned(),
        content: content.to_owned(),
        image_src: None,
        image_alt: None,
        call_id: None,
        tool_name: None,
    }
}

fn source_image(block: &Map<String, Value>) -> Option<String> {
    let kind = block.get("type")?.as_str()?;
    if !kind.to_lowercase().contains("image") {
        return None;
    }
    for key in ["url", "image_url"] {
        if let Some(value) = block.get(key).and_then(Value::as_str) {
            return safe_image_source(value);
        }
    }
    if let Some(data) = block.get("data").and_then(Value::as_str) {
        let media = ["mimeType", "mediaType", "media_type"]
            .iter()
            .find_map(|key| block.get(*key).and_then(Value::as_str))
            .unwrap_or("image/png");
        return safe_image_source(
            if data.starts_with("data:") {
                data.to_owned()
            } else {
                format!("data:{media};base64,{data}")
            }
            .as_str(),
        );
    }
    let source = block.get("source")?.as_object()?;
    if let Some(url) = source.get("url").and_then(Value::as_str) {
        return safe_image_source(url);
    }
    let data = source.get("data")?.as_str()?;
    let media = source
        .get("media_type")
        .and_then(Value::as_str)
        .unwrap_or("image/png");
    safe_image_source(format!("data:{media};base64,{data}").as_str())
}

fn safe_image_source(value: &str) -> Option<String> {
    if value.starts_with("data:image/") || value.starts_with("blob:") {
        return Some(value.to_owned());
    }
    Url::parse(value)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .map(|_| value.to_owned())
}

fn stringify_source_value(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| js_string(value))
}

fn enclosing_user_turn(
    following: Option<&Value>,
    partial: Option<&Value>,
    last_assistant_turn: Option<i64>,
) -> i64 {
    following
        .and_then(|assistant| integer(assistant, "turn"))
        .or_else(|| partial.and_then(|partial| integer(partial, "turn")))
        .or_else(|| last_assistant_turn.map(|turn| turn + 1))
        .unwrap_or(1)
}

fn steering_placement(
    following: Option<&Value>,
    partial: Option<&Value>,
    last_assistant_turn: Option<i64>,
    location: Option<&crate::TrajectoryLocation>,
) -> (i64, Option<i64>) {
    if let Some(crate::TrajectoryLocation::Step { turn, step }) = location {
        return (
            i64::try_from(turn.turn).unwrap_or_default(),
            i64::try_from(step.step).ok(),
        );
    }
    let located_turn = match location {
        Some(crate::TrajectoryLocation::Turn { turn }) => i64::try_from(turn.turn).ok(),
        _ => None,
    };
    if let Some(assistant) = following {
        let turn = integer(assistant, "turn").unwrap_or_default();
        if located_turn.is_none_or(|located| located == turn) {
            let step = integer(assistant, "step").filter(|step| *step > 0);
            return (turn, step);
        }
    }
    if let Some(partial) = partial {
        let turn = integer(partial, "turn").unwrap_or_default();
        if located_turn.is_none_or(|located| located == turn) {
            return (turn, integer(partial, "step").filter(|step| *step > 0));
        }
    }
    (
        located_turn.unwrap_or(last_assistant_turn.unwrap_or(1)),
        None,
    )
}

fn index_following_assistants(nodes: &[Value]) -> Vec<Option<Value>> {
    let mut following = vec![None; nodes.len()];
    let mut assistant = None;
    for index in (0..nodes.len()).rev() {
        following[index].clone_from(&assistant);
        if string(&nodes[index], "kind") == Some("assistant") {
            assistant = Some(nodes[index].clone());
        }
    }
    following
}

fn enclosing_prompt_turn(nodes: &[Value], seq: f64, partial: Option<&Value>) -> i64 {
    nodes
        .iter()
        .find(|node| {
            number(node, "seq").is_some_and(|candidate| candidate > seq)
                && string(node, "kind") == Some("assistant")
                && integer(node, "step").is_some_and(|step| step > 0)
        })
        .and_then(|node| integer(node, "turn"))
        .or_else(|| partial.and_then(|partial| integer(partial, "turn")))
        .unwrap_or(1)
}

fn first_visible_turn(nodes: &[Value], partial: Option<&Value>) -> i64 {
    nodes
        .iter()
        .filter(|node| string(node, "kind") == Some("assistant"))
        .filter_map(|node| integer(node, "turn"))
        .filter(|turn| *turn > 0)
        .chain(
            partial
                .and_then(|partial| integer(partial, "turn"))
                .filter(|turn| *turn > 0),
        )
        .min()
        .unwrap_or(1)
}

fn index_results(nodes: &[Value]) -> IndexMap<String, Value> {
    nodes
        .iter()
        .filter(|node| string(node, "kind") == Some("tool-result"))
        .filter_map(|node| string(node, "callId").map(|call_id| (call_id.to_owned(), node.clone())))
        .collect()
}

fn index_assistant_call_ids(nodes: &[Value]) -> IndexSet<String> {
    nodes
        .iter()
        .filter(|node| string(node, "kind") == Some("assistant"))
        .flat_map(|node| blocks_member(node, "blocks"))
        .filter(|block| string(block, "kind") == Some("tool-call"))
        .filter_map(|block| string(block, "callId").map(ToOwned::to_owned))
        .collect()
}

fn collect_call_ids(turns: &IndexMap<i64, TurnBucket>) -> IndexSet<String> {
    turns
        .values()
        .flat_map(|turn| &turn.groups)
        .flat_map(|group| &group.laid)
        .filter_map(|cell| cell.call_id.clone())
        .collect()
}

fn assistant_text(blocks: &[Value], kind: &str, streaming: bool) -> String {
    blocks
        .iter()
        .filter(|block| string(block, "kind") == Some(kind))
        .filter_map(|block| string(block, "text"))
        .filter(|text| !streaming || !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[derive(Clone)]
struct ResultPreview {
    result: Option<String>,
    markdown: Option<String>,
}

fn summarize_result(node: &Value) -> ResultPreview {
    if bool_member(node, "isError") {
        return ResultPreview {
            result: Some(
                node.get("error")
                    .and_then(|error| string(error, "code"))
                    .unwrap_or("error")
                    .to_owned(),
            ),
            markdown: None,
        };
    }
    for block in blocks_member(node, "content") {
        if string(block, "type") == Some("text")
            && let Some(text) = string(block, "text").filter(|text| !text.is_empty())
        {
            return ResultPreview {
                result: Some(String::new()),
                markdown: Some(text.to_owned()),
            };
        }
    }
    ResultPreview {
        result: Some("No output".to_owned()),
        markdown: None,
    }
}

fn apply_result_preview(cell: &mut TrajectoryCell, preview: Option<&ResultPreview>) {
    let Some(preview) = preview else {
        return;
    };
    cell.result.clone_from(&preview.result);
    cell.result_preview_markdown.clone_from(&preview.markdown);
}

fn result_as_text(cell: &mut TrajectoryCell, preview: Option<&ResultPreview>) {
    cell.text = preview
        .and_then(|preview| preview.result.clone())
        .unwrap_or_default();
    cell.preview_markdown = preview.and_then(|preview| preview.markdown.clone());
}

fn summarize_call_into(cell: &mut TrajectoryCell, name: &str, args: &str) {
    name.clone_into(&mut cell.text);
    cell.preview_markdown = (!args.is_empty()).then(|| args.to_owned());
}

fn detail_result(node: &Value) -> String {
    if bool_member(node, "isError") {
        return node.get("error").map_or_else(
            || "error".to_owned(),
            |error| {
                format!(
                    "{}: {}",
                    string(error, "name").unwrap_or_default(),
                    string(error, "code").unwrap_or_default()
                )
            },
        );
    }
    let content = blocks_member(node, "content");
    let text = content
        .iter()
        .filter(|block| string(block, "type") == Some("text"))
        .filter_map(|block| string(block, "text"))
        .collect::<Vec<_>>()
        .join("\n");
    if !text.is_empty() {
        return text;
    }
    if content.is_empty()
        || content.iter().all(|block| {
            string(block, "type") == Some("text") && string(block, "text").is_none_or(str::is_empty)
        })
    {
        "No output".to_owned()
    } else {
        serde_json::to_string_pretty(content).unwrap_or_default()
    }
}

fn detail_content(content: &Value) -> String {
    blocks(content)
        .iter()
        .filter(|block| string(block, "type") == Some("text"))
        .filter_map(|block| string(block, "text"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn detail_reasoning(content: &Value) -> String {
    blocks(content)
        .iter()
        .filter(|block| string(block, "type") == Some("reasoning"))
        .filter_map(|block| string(block, "text"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn preview_content(content: &Value) -> Option<String> {
    blocks(content)
        .iter()
        .find(|block| string(block, "type") == Some("text"))
        .and_then(|block| string(block, "text"))
        .map(ToOwned::to_owned)
}

fn duration_seconds(later: f64, earlier: Option<f64>) -> Option<f64> {
    let earlier = earlier.filter(|value| value.is_finite())?;
    later
        .is_finite()
        .then(|| ((later - earlier) / 1_000.0).max(0.0))
}

fn finite_member(value: &Value, key: &str) -> Option<f64> {
    number(value, key).filter(|value| value.is_finite())
}

fn blocks(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}

fn blocks_member<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn number(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(Value::as_f64)
}

fn integer(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

fn safe_seq(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn bool_member(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool) == Some(true)
}

fn display_number(value: &Value, key: &str) -> String {
    value
        .get(key)
        .map_or_else(|| "undefined".to_owned(), js_string)
}

fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(values) => values.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

fn step_key(turn: i64, step: i64) -> String {
    format!("{turn}\0{step}")
}
