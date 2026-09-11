use std::rc::Rc;

use indexmap::{IndexMap, IndexSet};
use seekdeep_client_runtime::{
    AssemblerNodeDefinition, ConversationAssemblerError, ConversationBoundaryStatus,
    ConversationLocation, ConversationMatch, ConversationMatchResult, ConversationMatchRole,
    ConversationNodeContext, ConversationValue as Value,
};
use seekdeep_lossless_json::JsonString;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    CHAT_INTERRUPTED_FOLLOWUP_OFFSET, chat_node, is_append_surface_event, js_string,
    sequence_anchor,
};

/// Root Tool lifecycle and nested Code Dispatch definition kind.
pub const TOOL_CALL_KIND: &str = "tool-call";
const MAX_DEPTH: usize = 256;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ToolState {
    root_id: String,
    calls: IndexMap<String, Value>,
    children: IndexMap<String, Vec<String>>,
    parents: IndexMap<String, String>,
}

/// Builds the root Tool lifecycle and nested Code Dispatch definition.
#[must_use]
pub fn conversation_tool_definition() -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: TOOL_CALL_KIND.to_owned(),
        target: Some("chat".to_owned()),
        match_event: Rc::new(|event| {
            let matched = match event.event_type.as_str() {
                "tool/call" => Some(ConversationMatchResult {
                    id: js_member_string(&event.data, "callId"),
                    role: ConversationMatchRole::Start,
                }),
                "tool/result" if is_append_surface_event(event) => Some(ConversationMatchResult {
                    id: js_member_string(&event.data["message"]["source"], "callId"),
                    role: ConversationMatchRole::Update,
                }),
                "tool/code-dispatch-start" | "tool/code-dispatch" => event
                    .data
                    .get_value("rootCallId")
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
            .map_or(0.0, |accepted| sequence_anchor(accepted.event.seq))
    };
    let anchor_seq = context.start.as_ref().map_or_else(
        || {
            if is_settled(&state.calls[&state.root_id]) {
                state.calls[&state.root_id]
                    .get_value("seq")
                    .and_then(Value::as_f64)
                    .unwrap_or_else(first_match_seq)
            } else {
                first_match_seq()
            }
        },
        |start| sequence_anchor(start.event.seq),
    );
    Ok(Some(chat_node(
        context,
        TOOL_CALL_KIND,
        anchor_seq,
        Value::object([("root", root)]),
    )))
}

fn root_call(accepted: &ConversationMatch) -> Result<Value, ConversationAssemblerError> {
    if accepted.event.event_type != "tool/call" {
        return Err(ConversationAssemblerError::new(
            "tool-call start requires tool/call",
        ));
    }
    let data = &accepted.event.data;
    Ok(Value::object([
        ("callId", json!(js_member_string(data, "callId")).into()),
        ("name", data["name"].clone()),
        ("argsRaw", data["arguments"].clone()),
        ("turn", data["turn"].clone()),
        ("step", data["step"].clone()),
        ("time", json!(accepted.event.time).into()),
        (
            "callView",
            match_view(accepted, "call").unwrap_or_else(|| json!(null).into()),
        ),
        ("subCalls", Value::array(&[])),
    ]))
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
        .get_value("message")
        .and_then(|message| message.get_value("content"))
        .and_then(Value::as_array)
        .and_then(|content| content.first())
        .ok_or_else(|| ConversationAssemblerError::new("tool/result omitted first content"))?;
    let source = &data["message"]["source"];
    let mut block = vec![
        ("kind", json!("tool-result").into()),
        ("seq", json!(accepted.event.seq).into()),
        ("time", json!(accepted.event.time).into()),
        ("callId", json!(js_member_string(source, "callId")).into()),
        (
            "call",
            previous.map_or_else(
                || json!(null).into(),
                |previous| {
                    Value::object([
                        ("name", previous["name"].clone()),
                        ("argsRaw", previous["argsRaw"].clone()),
                    ])
                },
            ),
        ),
        (
            "callTime",
            previous
                .and_then(|previous| previous.get_value("time"))
                .cloned()
                .unwrap_or_else(|| json!(null).into()),
        ),
        ("content", result["content"].clone()),
        (
            "isError",
            json!(result.get_value("isError").and_then(Value::as_bool) == Some(true)).into(),
        ),
        (
            "callView",
            previous
                .and_then(|previous| previous.get_value("callView"))
                .cloned()
                .unwrap_or_else(|| json!(null).into()),
        ),
        (
            "resultView",
            match_view(accepted, "result").unwrap_or_else(|| json!(null).into()),
        ),
        ("subCalls", Value::array(&[])),
    ];
    copy_present(&mut block, data, "error");
    if let Some(meta) = data.get_value("meta") {
        block.push(("meta", meta.clone()));
    }
    Ok(Some(Value::object(block)))
}

fn child_call(
    accepted: &ConversationMatch,
    data: &Value,
) -> Result<Value, ConversationAssemblerError> {
    Ok(Value::object([
        ("callId", json!(required_string(data, "subCallId")?).into()),
        ("name", data["name"].clone()),
        ("argsRaw", json_stringify(data.get_value("arguments"))),
        ("turn", json!(location_turn(&accepted.location)).into()),
        ("step", json!(location_step(&accepted.location)).into()),
        ("time", json!(accepted.event.time).into()),
        ("callView", json!(null).into()),
        ("subCalls", Value::array(&[])),
    ]))
}

fn child_result(
    accepted: &ConversationMatch,
    data: &Value,
    previous: Option<&Value>,
) -> Result<Value, ConversationAssemblerError> {
    Ok(Value::object([
        ("kind", json!("tool-result").into()),
        ("seq", json!(accepted.event.seq).into()),
        ("time", json!(accepted.event.time).into()),
        ("callId", json!(required_string(data, "subCallId")?).into()),
        (
            "call",
            Value::object([
                ("name", data["name"].clone()),
                ("argsRaw", json_stringify(data.get_value("arguments"))),
            ]),
        ),
        (
            "callTime",
            previous
                .filter(|block| !is_settled(block))
                .and_then(|block| block.get_value("time"))
                .cloned()
                .unwrap_or_else(|| json!(null).into()),
        ),
        (
            "content",
            data.get_value("content")
                .cloned()
                .unwrap_or_else(|| Value::array(&[])),
        ),
        (
            "isError",
            json!(data.get_value("isError").and_then(Value::as_bool) == Some(true)).into(),
        ),
        ("callView", json!(null).into()),
        ("resultView", json!(null).into()),
        ("subCalls", Value::array(&[])),
    ]))
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
        return with_sub_calls(block.clone(), &[]).map(Some);
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
        return with_sub_calls(block.clone(), &sub_calls).map(Some);
    }
    let (seq, time) = interrupted_at.expect("checked");
    Ok(Some(Value::object([
        ("kind", json!("tool-result").into()),
        (
            "seq",
            json!(sequence_anchor(seq) + CHAT_INTERRUPTED_FOLLOWUP_OFFSET).into(),
        ),
        ("time", json!(time).into()),
        ("callId", block["callId"].clone()),
        (
            "call",
            Value::object([
                ("name", block["name"].clone()),
                ("argsRaw", block["argsRaw"].clone()),
            ]),
        ),
        ("callTime", block["time"].clone()),
        ("content", Value::array(&[])),
        ("isError", json!(true).into()),
        (
            "error",
            json!({"name": "Interrupted", "code": "interrupted"}).into(),
        ),
        ("callView", block["callView"].clone()),
        ("resultView", json!(null).into()),
        ("subCalls", Value::array(&sub_calls)),
    ])))
}

fn fallback_state(
    context: &ConversationNodeContext,
) -> Result<Option<ToolState>, ConversationAssemblerError> {
    let matches = context.matches.borrow();
    let Some(result_match) = matches
        .iter()
        .find(|accepted| accepted.event.event_type == "tool/result")
    else {
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
        && step.status == ConversationBoundaryStatus::Closed
        && let Some(end) = &step.end
    {
        return Some((end.seq, end.time));
    }
    match location {
        ConversationLocation::Step { turn, .. } | ConversationLocation::Turn { turn }
            if turn.status == ConversationBoundaryStatus::Closed =>
        {
            turn.end.as_ref().map(|end| (end.seq, end.time))
        }
        _ => None,
    }
}

fn with_sub_calls(
    mut block: Value,
    sub_calls: &[Value],
) -> Result<Value, ConversationAssemblerError> {
    block
        .insert("subCalls", Value::array(sub_calls))
        .map_err(|_| ConversationAssemblerError::new("Tool block must be an object"))?;
    Ok(block)
}

fn block_call_id(block: &Value) -> Result<&str, ConversationAssemblerError> {
    block
        .get_value("callId")
        .and_then(Value::as_str)
        .ok_or_else(|| ConversationAssemblerError::new("Tool block omitted callId"))
}

fn match_view(accepted: &ConversationMatch, expected: &str) -> Option<Value> {
    let view = accepted.view.as_deref()?;
    (view.get_value("for").and_then(Value::as_str) == Some(expected))
        .then(|| view.get_value("view").cloned())
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
    block.get_value("kind").is_some()
}

fn required_string<'a>(value: &'a Value, key: &str) -> Result<&'a str, ConversationAssemblerError> {
    value
        .get_value(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ConversationAssemblerError::new(format!("dispatch omitted {key}")))
}

fn json_stringify(value: Option<&Value>) -> Value {
    value.map_or_else(
        || json!(null).into(),
        |value| JsonString::from(value.stringify()).into(),
    )
}

fn js_member_string(value: &Value, key: &str) -> String {
    value
        .get_value(key)
        .map_or_else(|| "undefined".to_owned(), js_string)
}

fn copy_present(output: &mut Vec<(&'static str, Value)>, input: &Value, key: &'static str) {
    if let Some(value) = input.get_value(key) {
        output.push((key, value.clone()));
    }
}

fn encode(state: &ToolState) -> Result<Rc<Value>, ConversationAssemblerError> {
    Value::from_serialize(state)
        .map(Rc::new)
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}

fn decode(value: &Value) -> Result<ToolState, ConversationAssemblerError> {
    value
        .deserialize()
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}
