//! Trajectory root Tool lifecycle with nested Code Dispatch calls.

use std::rc::Rc;

use indexmap::{IndexMap, IndexSet};
use seekdeep_client_runtime::{
    AssemblerNodeDefinition, ConversationAssemblerError, ConversationBoundaryStatus,
    ConversationLocation, ConversationMatch, ConversationMatchResult, ConversationMatchRole,
    ConversationNodeContext,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{TRAJECTORY_TARGET, trajectory_node_at};

/// Tool lifecycle Definition kind.
pub const TRAJECTORY_TOOL_KIND: &str = "trajectory-tool-call";
const MAX_DEPTH: usize = 256;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ToolState {
    root_id: String,
    calls: IndexMap<String, Value>,
    children: IndexMap<String, Vec<String>>,
    parents: IndexMap<String, String>,
}

/// Builds the trajectory-owned Tool lifecycle Definition.
#[must_use]
pub fn trajectory_tool_definition() -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: TRAJECTORY_TOOL_KIND.to_owned(),
        target: Some(TRAJECTORY_TARGET.to_owned()),
        match_event: Rc::new(|event| {
            let matched = match event.event_type.as_str() {
                "tool/call" => Some(ConversationMatchResult {
                    id: js_member_string(&event.data, "callId"),
                    role: ConversationMatchRole::Start,
                }),
                "tool/result" => Some(ConversationMatchResult {
                    id: js_member_string(
                        event
                            .data
                            .get("message")
                            .and_then(|message| message.get("source"))
                            .unwrap_or(&Value::Null),
                        "callId",
                    ),
                    role: ConversationMatchRole::Update,
                }),
                "tool/code-dispatch-start" | "tool/code-dispatch" => event
                    .data
                    .get("rootCallId")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map(|id| ConversationMatchResult {
                        id: id.to_owned(),
                        role: ConversationMatchRole::Update,
                    }),
                _ => None,
            };
            Ok(matched)
        }),
        start: Rc::new(|_context, accepted, _reader| {
            let root = root_call(accepted)?;
            let root_id = block_call_id(&root)?.to_owned();
            encode(&ToolState {
                root_id: root_id.clone(),
                calls: IndexMap::from_iter([(root_id, root)]),
                children: IndexMap::new(),
                parents: IndexMap::new(),
            })
            .map(Some)
        }),
        update: Rc::new(|context, accepted| {
            let Some(previous) = context.state.as_deref() else {
                return Ok(None);
            };
            let state = decode(previous)?;
            let next = if accepted.event.event_type == "tool/result" {
                update_root_result(state, accepted)?
            } else {
                update_dispatch(state, accepted)?
            };
            encode(&next).map(Some)
        }),
        publication: None,
        build_location_data: None,
        build_view_node: Some(Rc::new(build_tool_view_node)),
    }
}

fn build_tool_view_node(
    context: &ConversationNodeContext,
) -> Result<Option<Rc<seekdeep_client_runtime::ConversationViewNode>>, ConversationAssemblerError> {
    let state = match context.state.as_deref() {
        Some(state) => Some(decode(state)?),
        None => fallback_state(context)?,
    };
    let Some(state) = state else {
        return Ok(None);
    };
    let Some(root) = project_call(
        &state,
        &state.root_id,
        interruption(context),
        &IndexSet::new(),
        1,
    )?
    else {
        return Ok(None);
    };
    let first_match_seq = || {
        context
            .matches
            .borrow()
            .first()
            .map_or(0.0, |accepted| u64_as_f64(accepted.event.seq))
    };
    let anchor_seq = context.start.as_ref().map_or_else(
        || {
            if is_settled(&root) {
                root.get("seq")
                    .and_then(Value::as_f64)
                    .unwrap_or_else(first_match_seq)
            } else {
                first_match_seq()
            }
        },
        |start| u64_as_f64(start.event.seq),
    );
    Ok(Some(trajectory_node_at(
        context,
        anchor_seq,
        json!({"kind": "tool", "root": root}),
    )))
}

fn root_call(accepted: &ConversationMatch) -> Result<Value, ConversationAssemblerError> {
    if accepted.event.event_type != "tool/call" {
        return Err(ConversationAssemblerError::new(
            "trajectory-tool-call start requires tool/call",
        ));
    }
    let data = &accepted.event.data;
    Ok(json!({
        "callId": js_member_string(data, "callId"),
        "name": data.get("name").cloned().unwrap_or(Value::Null),
        "argsRaw": data.get("arguments").cloned().unwrap_or(Value::Null),
        "turn": data.get("turn").cloned().unwrap_or(Value::Null),
        "step": data.get("step").cloned().unwrap_or(Value::Null),
        "time": accepted.event.time,
        "callView": match_view(accepted, "call").unwrap_or(Value::Null),
        "subCalls": [],
    }))
}

fn root_result(
    accepted: &ConversationMatch,
    previous: Option<&Value>,
) -> Result<Option<Value>, ConversationAssemblerError> {
    if accepted.event.event_type != "tool/result" {
        return Ok(None);
    }
    let data = &accepted.event.data;
    let result = data
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
        .and_then(|content| content.first())
        .ok_or_else(|| ConversationAssemblerError::new("tool/result omitted first content"))?;
    let source = data
        .get("message")
        .and_then(|message| message.get("source"))
        .unwrap_or(&Value::Null);
    let mut block = Map::from_iter([
        ("kind".to_owned(), json!("tool-result")),
        ("seq".to_owned(), json!(accepted.event.seq)),
        ("time".to_owned(), json!(accepted.event.time)),
        (
            "callId".to_owned(),
            json!(js_member_string(source, "callId")),
        ),
        (
            "call".to_owned(),
            previous.map_or(Value::Null, |previous| {
                json!({
                    "name": previous.get("name").cloned().unwrap_or(Value::Null),
                    "argsRaw": previous.get("argsRaw").cloned().unwrap_or(Value::Null),
                })
            }),
        ),
        (
            "callTime".to_owned(),
            previous
                .and_then(|previous| previous.get("time"))
                .cloned()
                .unwrap_or(Value::Null),
        ),
        (
            "content".to_owned(),
            result.get("content").cloned().unwrap_or(Value::Null),
        ),
        (
            "isError".to_owned(),
            json!(result.get("isError").and_then(Value::as_bool) == Some(true)),
        ),
        (
            "callView".to_owned(),
            previous
                .and_then(|previous| previous.get("callView"))
                .cloned()
                .unwrap_or(Value::Null),
        ),
        (
            "resultView".to_owned(),
            match_view(accepted, "result").unwrap_or(Value::Null),
        ),
        ("subCalls".to_owned(), json!([])),
    ]);
    copy_present(&mut block, data, "error");
    copy_present(&mut block, data, "meta");
    Ok(Some(Value::Object(block)))
}

fn child_call(
    accepted: &ConversationMatch,
    data: &Value,
) -> Result<Value, ConversationAssemblerError> {
    Ok(json!({
        "callId": required_string(data, "subCallId")?,
        "name": data.get("name").cloned().unwrap_or(Value::Null),
        "argsRaw": json_stringify(data.get("arguments"))?,
        "turn": location_turn(&accepted.location),
        "step": location_step(&accepted.location),
        "time": accepted.event.time,
        "callView": null,
        "subCalls": [],
    }))
}

fn child_result(
    accepted: &ConversationMatch,
    data: &Value,
    previous: Option<&Value>,
) -> Result<Value, ConversationAssemblerError> {
    Ok(json!({
        "kind": "tool-result",
        "seq": accepted.event.seq,
        "time": accepted.event.time,
        "callId": required_string(data, "subCallId")?,
        "call": {
            "name": data.get("name").cloned().unwrap_or(Value::Null),
            "argsRaw": json_stringify(data.get("arguments"))?,
        },
        "callTime": previous.filter(|block| !is_settled(block))
            .and_then(|block| block.get("time")).cloned().unwrap_or(Value::Null),
        "content": data.get("content").cloned().unwrap_or_else(|| json!([])),
        "isError": data.get("isError").and_then(Value::as_bool) == Some(true),
        "callView": null,
        "resultView": null,
        "subCalls": [],
    }))
}

fn update_root_result(
    mut state: ToolState,
    accepted: &ConversationMatch,
) -> Result<ToolState, ConversationAssemblerError> {
    let previous = state
        .calls
        .get(&state.root_id)
        .filter(|block| !is_settled(block));
    let Some(result) = root_result(accepted, previous)? else {
        return Ok(state);
    };
    state.calls.insert(state.root_id.clone(), result);
    Ok(state)
}

fn update_dispatch(
    mut state: ToolState,
    accepted: &ConversationMatch,
) -> Result<ToolState, ConversationAssemblerError> {
    if !matches!(
        accepted.event.event_type.as_str(),
        "tool/code-dispatch-start" | "tool/code-dispatch"
    ) {
        return Ok(state);
    }
    let data = &accepted.event.data;
    let parent_id = js_member_string(data, "parentCallId");
    let child_id = js_member_string(data, "subCallId");
    let siblings = state.children.get(&parent_id).cloned().unwrap_or_default();
    let at = siblings.iter().position(|id| id == &child_id);
    if at.is_none() && !accepts_edge(&state, &parent_id, &child_id) {
        return Ok(state);
    }
    if accepted.event.event_type == "tool/code-dispatch-start" && at.is_some() {
        return Ok(state);
    }
    let block = if accepted.event.event_type == "tool/code-dispatch-start" {
        child_call(accepted, data)?
    } else {
        child_result(accepted, data, state.calls.get(&child_id))?
    };
    state.calls.insert(child_id.clone(), block);
    if at.is_some() {
        return Ok(state);
    }
    let mut next_siblings = siblings;
    next_siblings.push(child_id.clone());
    state.children.insert(parent_id.clone(), next_siblings);
    state.parents.insert(child_id, parent_id);
    Ok(state)
}

fn accepts_edge(state: &ToolState, parent: &str, child: &str) -> bool {
    if parent == child || state.parents.contains_key(child) {
        return false;
    }
    let mut cursor = Some(parent);
    let mut parent_depth = 0_usize;
    let mut ancestors = IndexSet::new();
    while let Some(current) = cursor {
        if current == child || !ancestors.insert(current.to_owned()) {
            return false;
        }
        parent_depth += 1;
        cursor = state.parents.get(current).map(String::as_str);
    }

    let mut pending = vec![(child.to_owned(), 1_usize)];
    let mut descendants = IndexSet::new();
    let mut subtree_depth = 0_usize;
    let mut at = 0_usize;
    while at < pending.len() {
        let (call_id, depth) = pending[at].clone();
        at += 1;
        if !descendants.insert(call_id.clone()) {
            return false;
        }
        subtree_depth = subtree_depth.max(depth);
        if let Some(children) = state.children.get(&call_id) {
            pending.extend(children.iter().map(|nested| (nested.clone(), depth + 1)));
        }
    }
    parent_depth + subtree_depth <= MAX_DEPTH
}

fn project_call(
    state: &ToolState,
    call_id: &str,
    interrupted_at: Option<(u64, i64)>,
    visited: &IndexSet<String>,
    depth: usize,
) -> Result<Option<Value>, ConversationAssemblerError> {
    let Some(block) = state.calls.get(call_id) else {
        return Ok(None);
    };
    if visited.contains(call_id) || depth > MAX_DEPTH {
        return with_sub_calls(block.clone(), Vec::new()).map(Some);
    }
    let mut next_visited = visited.clone();
    next_visited.insert(call_id.to_owned());
    let mut sub_calls = Vec::new();
    for child_id in state.children.get(call_id).into_iter().flatten() {
        if let Some(child) =
            project_call(state, child_id, interrupted_at, &next_visited, depth + 1)?
        {
            sub_calls.push(child);
        }
    }
    if is_settled(block) || interrupted_at.is_none() {
        return with_sub_calls(block.clone(), sub_calls).map(Some);
    }
    let (seq, time) = interrupted_at.expect("checked");
    Ok(Some(json!({
        "kind": "tool-result",
        "seq": u64_as_f64(seq) - 0.8,
        "time": time,
        "callId": block.get("callId").cloned().unwrap_or(Value::Null),
        "call": {
            "name": block.get("name").cloned().unwrap_or(Value::Null),
            "argsRaw": block.get("argsRaw").cloned().unwrap_or(Value::Null),
        },
        "callTime": block.get("time").cloned().unwrap_or(Value::Null),
        "content": [],
        "isError": true,
        "error": {"name": "Interrupted", "code": "interrupted"},
        "callView": block.get("callView").cloned().unwrap_or(Value::Null),
        "resultView": null,
        "subCalls": sub_calls,
    })))
}

fn fallback_state(
    context: &ConversationNodeContext,
) -> Result<Option<ToolState>, ConversationAssemblerError> {
    let matches = context.matches.borrow();
    let result_match = matches
        .iter()
        .find(|accepted| accepted.event.event_type == "tool/result");
    let Some(result_match) = result_match else {
        return Ok(None);
    };
    let Some(root) = root_result(result_match, None)? else {
        return Ok(None);
    };
    let root_id = block_call_id(&root)?.to_owned();
    let mut state = ToolState {
        root_id: root_id.clone(),
        calls: IndexMap::from_iter([(root_id, root)]),
        children: IndexMap::new(),
        parents: IndexMap::new(),
    };
    for accepted in matches.iter() {
        state = update_dispatch(state, accepted)?;
    }
    Ok(Some(state))
}

fn interruption(context: &ConversationNodeContext) -> Option<(u64, i64)> {
    let location = &context.start.as_ref()?.location;
    if let ConversationLocation::Step { step, .. } = location
        && matches!(step.status, ConversationBoundaryStatus::Closed)
        && let Some(end) = &step.end
    {
        return Some((end.seq, end.time));
    }
    match location {
        ConversationLocation::Step { turn, .. } | ConversationLocation::Turn { turn }
            if matches!(turn.status, ConversationBoundaryStatus::Closed) =>
        {
            turn.end.as_ref().map(|end| (end.seq, end.time))
        }
        _ => None,
    }
}

fn with_sub_calls(
    mut block: Value,
    sub_calls: Vec<Value>,
) -> Result<Value, ConversationAssemblerError> {
    block
        .as_object_mut()
        .ok_or_else(|| ConversationAssemblerError::new("trajectory Tool block must be an object"))?
        .insert("subCalls".to_owned(), Value::Array(sub_calls));
    Ok(block)
}

fn block_call_id(block: &Value) -> Result<&str, ConversationAssemblerError> {
    block
        .get("callId")
        .and_then(Value::as_str)
        .ok_or_else(|| ConversationAssemblerError::new("trajectory Tool block omitted callId"))
}

fn match_view(accepted: &ConversationMatch, expected: &str) -> Option<Value> {
    let view = accepted.view.as_deref()?;
    (view.get("for").and_then(Value::as_str) == Some(expected))
        .then(|| view.get("view").cloned())
        .flatten()
}

fn location_turn(location: &ConversationLocation) -> u64 {
    match location {
        ConversationLocation::Turn { turn } | ConversationLocation::Step { turn, .. } => turn.turn,
        ConversationLocation::Session | ConversationLocation::Unresolved => 0,
    }
}

fn location_step(location: &ConversationLocation) -> u64 {
    match location {
        ConversationLocation::Step { step, .. } => step.step,
        ConversationLocation::Session
        | ConversationLocation::Turn { .. }
        | ConversationLocation::Unresolved => 0,
    }
}

fn is_settled(block: &Value) -> bool {
    block.get("kind").is_some()
}

fn required_string<'a>(value: &'a Value, key: &str) -> Result<&'a str, ConversationAssemblerError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ConversationAssemblerError::new(format!("dispatch omitted {key}")))
}

fn json_stringify(value: Option<&Value>) -> Result<Value, ConversationAssemblerError> {
    value.map_or(Ok(Value::Null), |value| {
        serde_json::to_string(value)
            .map(Value::String)
            .map_err(|error| ConversationAssemblerError::new(error.to_string()))
    })
}

fn js_member_string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .map_or_else(|| "undefined".to_owned(), js_string)
}

fn js_string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Array(values) => values.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

fn copy_present(output: &mut Map<String, Value>, input: &Value, key: &str) {
    if let Some(value) = input.get(key) {
        output.insert(key.to_owned(), value.clone());
    }
}

fn encode(state: &ToolState) -> Result<Rc<Value>, ConversationAssemblerError> {
    serde_json::to_value(state)
        .map(Rc::new)
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}

fn decode(value: &Value) -> Result<ToolState, ConversationAssemblerError> {
    serde_json::from_value(value.clone())
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}

fn u64_as_f64(value: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}
