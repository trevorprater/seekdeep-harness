//! Incremental trajectory contribution assembly into the stage-oriented snapshot.

use std::{
    hash::{Hash, Hasher},
    rc::Rc,
};

use indexmap::{IndexMap, IndexSet};
use seekdeep_client_runtime::{
    AssemblerViewBuilder, AssemblerViewDefinition, ConversationAssemblerError,
    ConversationPromptSnapshot, ConversationTimelineSnapshot, ConversationViewNode,
};
use serde_json::{Value, json};

use crate::{TrajectoryLocation, TrajectoryRequestHeaderState};

/// Numeric event identity used by the source `Map<number, Location>`.
#[derive(Clone, Copy, Debug)]
pub struct TrajectorySequence(f64);

impl TrajectorySequence {
    /// Creates one finite projected sequence.
    #[must_use]
    pub const fn new(value: f64) -> Self {
        Self(value)
    }

    /// Returns the numeric sequence.
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }
}

impl PartialEq for TrajectorySequence {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for TrajectorySequence {}

impl Hash for TrajectorySequence {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

/// Stage-oriented trajectory snapshot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrajectorySnapshot {
    /// Finalized event nodes in numeric sequence order.
    pub event_nodes: Vec<Value>,
    /// Location of finalized input nodes.
    pub event_locations: IndexMap<TrajectorySequence, TrajectoryLocation>,
    /// Provider requests in start sequence order.
    pub requests: Vec<Value>,
    /// Call-time Tool schema by call identity.
    pub call_schemas: IndexMap<String, Value>,
    /// Latest streaming Assistant partial.
    pub partial: Option<Value>,
    /// Running root Tool calls.
    pub running_calls: Vec<Value>,
}

/// Keyed incremental adapter retaining stable contribution order across content updates.
#[derive(Default)]
pub struct TrajectorySnapshotBuilder {
    nodes: IndexMap<String, Rc<ConversationViewNode>>,
    positions: IndexMap<String, usize>,
    contributions: Vec<Rc<ConversationViewNode>>,
}

impl TrajectorySnapshotBuilder {
    /// Creates an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the full target node set.
    #[must_use]
    pub fn replace_typed(&mut self, nodes: &[Rc<ConversationViewNode>]) -> TrajectorySnapshot {
        self.nodes.clear();
        for node in nodes {
            self.nodes.insert(node.key.clone(), node.clone());
        }
        self.rebuild_contributions();
        self.snapshot()
    }

    /// Applies changed target nodes, rebuilding order only for structural changes.
    #[must_use]
    pub fn apply_typed(&mut self, upserts: &[Rc<ConversationViewNode>]) -> TrajectorySnapshot {
        let mut structural = false;
        for node in upserts {
            let previous = self.nodes.insert(node.key.clone(), node.clone());
            if previous
                .as_ref()
                .is_none_or(|previous| placement_anchor(previous) != placement_anchor(node))
            {
                structural = true;
                continue;
            }
            if let Some(position) = self.positions.get(&node.key).copied() {
                self.contributions[position] = node.clone();
            } else {
                structural = true;
            }
        }
        if structural {
            self.rebuild_contributions();
        }
        self.snapshot()
    }

    #[allow(clippy::too_many_lines)] // One ordered fold mirrors the closed contribution union.
    fn snapshot(&self) -> TrajectorySnapshot {
        let mut headers_by_step = IndexMap::<String, TrajectoryRequestHeaderState>::new();
        for contribution in &self.contributions {
            if contribution.data.get("kind").and_then(Value::as_str) != Some("request-header") {
                continue;
            }
            let Some(header) = contribution.data.get("header").cloned() else {
                continue;
            };
            let Ok(header) = serde_json::from_value::<TrajectoryRequestHeaderState>(header) else {
                continue;
            };
            if let Some(key) = header_step_key(&header) {
                headers_by_step.insert(key, header);
            }
        }

        let mut finalized = Vec::<Value>::new();
        let mut event_locations = IndexMap::new();
        let mut requests = Vec::<Value>::new();
        let mut boundaries = Vec::<(f64, i64)>::new();
        let mut turn_endings = Vec::<(i64, i64, Option<String>)>::new();
        let mut call_schemas = IndexMap::<String, Value>::new();
        let mut consumed_prompt_changes = IndexSet::<u64>::new();
        let mut previous_header = None::<TrajectoryRequestHeaderState>;
        let mut previous_tools = IndexMap::<String, Value>::new();
        let mut partial = None;
        let mut running_calls = Vec::new();

        for contribution in &self.contributions {
            let data = contribution.data.as_ref();
            match data.get("kind").and_then(Value::as_str) {
                Some("request-header") => {
                    if let Some(header) = data.get("header").cloned().and_then(|header| {
                        serde_json::from_value::<TrajectoryRequestHeaderState>(header).ok()
                    }) {
                        previous_tools = index_tools(&header.prompt);
                        previous_header = Some(header);
                    }
                }
                Some("node") => {
                    if let Some(node) = data.get("node").cloned() {
                        if let Some(seq) = node.get("seq").and_then(Value::as_f64)
                            && let Some(placement) = &contribution.placement
                        {
                            event_locations.insert(
                                TrajectorySequence::new(seq),
                                TrajectoryLocation::from(&placement.location),
                            );
                        }
                        finalized.push(node);
                    }
                }
                Some("assistant") => {
                    let request = data.get("request").cloned();
                    let header = request.as_ref().and_then(|request| {
                        header_for(request, &headers_by_step, previous_header.as_ref())
                    });
                    if let Some(node) = data.get("node").cloned() {
                        finalized.push(with_request_config(
                            node,
                            header.map(|header| &header.prompt),
                        ));
                    }
                    if let Some(value) = data.get("partial").filter(|value| !value.is_null()) {
                        partial = Some(value.clone());
                    }
                    if let Some(request) = request {
                        let include_change = header
                            .and_then(|header| header.change.as_ref().map(|_| header.seq))
                            .is_some_and(|seq| !consumed_prompt_changes.contains(&seq));
                        requests.push(apply_header(request, header, include_change));
                        if include_change && let Some(header) = header {
                            consumed_prompt_changes.insert(header.seq);
                        }
                    }
                }
                Some("tool") => {
                    if let Some(root) = data.get("root").cloned() {
                        if root.get("kind").is_some() {
                            finalized.push(root.clone());
                        } else {
                            running_calls.push(root.clone());
                        }
                        if let (Some(header), Some(anchor)) =
                            (previous_header.as_ref(), placement_anchor(contribution))
                            && u64_as_f64(header.seq) < anchor
                        {
                            capture_schemas(&root, &previous_tools, &mut call_schemas);
                        }
                    }
                }
                Some("compaction") => {
                    if let Some(request) = data.get("request") {
                        requests.push(request.clone());
                    }
                }
                Some("session-end") => {
                    if let (Some(seq), Some(time)) = (
                        data.get("seq").and_then(Value::as_f64),
                        data.get("time").and_then(Value::as_i64),
                    ) {
                        boundaries.push((seq, time));
                    }
                }
                _ => {
                    if let (Some(turn), Some(time)) = (
                        data.get("turn").and_then(Value::as_i64),
                        data.get("time").and_then(Value::as_i64),
                    ) {
                        turn_endings.push((
                            turn,
                            time,
                            data.get("error")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned),
                        ));
                    }
                }
            }
        }

        requests
            .sort_by(|left, right| request_start_seq(left).total_cmp(&request_start_seq(right)));
        interrupt_compactions(&mut requests, &boundaries);
        apply_turn_errors(&mut requests, &turn_endings);
        finalized.sort_by(|left, right| node_seq(left).total_cmp(&node_seq(right)));
        TrajectorySnapshot {
            event_nodes: finalized,
            event_locations,
            requests,
            call_schemas,
            partial,
            running_calls,
        }
    }

    fn rebuild_contributions(&mut self) {
        self.contributions = self.nodes.values().cloned().collect();
        self.contributions.sort_by(|left, right| {
            placement_anchor(left)
                .unwrap_or_default()
                .total_cmp(&placement_anchor(right).unwrap_or_default())
                .then_with(|| left.key.cmp(&right.key))
        });
        self.positions.clear();
        for (index, contribution) in self.contributions.iter().enumerate() {
            self.positions.insert(contribution.key.clone(), index);
        }
    }
}

impl AssemblerViewBuilder for TrajectorySnapshotBuilder {
    fn empty(&self) -> Rc<Value> {
        Rc::new(snapshot_value(&TrajectorySnapshot::default()))
    }

    fn replace(
        &mut self,
        nodes: &[Rc<ConversationViewNode>],
        _timeline: Rc<ConversationTimelineSnapshot>,
    ) -> Result<Rc<Value>, ConversationAssemblerError> {
        Ok(Rc::new(snapshot_value(&self.replace_typed(nodes))))
    }

    fn apply(
        &mut self,
        upserts: &[Rc<ConversationViewNode>],
        _timeline: Rc<ConversationTimelineSnapshot>,
    ) -> Result<Rc<Value>, ConversationAssemblerError> {
        Ok(Rc::new(snapshot_value(&self.apply_typed(upserts))))
    }
}

/// Builds the native trajectory view Definition.
#[must_use]
pub fn trajectory_view_definition() -> AssemblerViewDefinition {
    AssemblerViewDefinition {
        target: "trajectory".to_owned(),
        create: Rc::new(|| Box::new(TrajectorySnapshotBuilder::new())),
    }
}

fn step_key(turn: i64, step: i64) -> String {
    format!("{turn}\0{step}")
}

fn header_step_key(header: &TrajectoryRequestHeaderState) -> Option<String> {
    match &header.location {
        TrajectoryLocation::Step { turn, step } => Some(step_key(
            i64::try_from(turn.turn).ok()?,
            i64::try_from(step.step).ok()?,
        )),
        TrajectoryLocation::Session
        | TrajectoryLocation::Turn { .. }
        | TrajectoryLocation::Unresolved => None,
    }
}

fn header_for<'a>(
    request: &Value,
    headers_by_step: &'a IndexMap<String, TrajectoryRequestHeaderState>,
    previous: Option<&'a TrajectoryRequestHeaderState>,
) -> Option<&'a TrajectoryRequestHeaderState> {
    let turn = request.get("turn").and_then(Value::as_i64)?;
    let step = request.get("step").and_then(Value::as_i64)?;
    headers_by_step.get(&step_key(turn, step)).or_else(|| {
        previous.filter(|header| {
            u64_as_f64(header.seq)
                < request
                    .get("startSeq")
                    .and_then(Value::as_f64)
                    .unwrap_or_default()
        })
    })
}

fn apply_header(
    mut request: Value,
    header: Option<&TrajectoryRequestHeaderState>,
    include_change: bool,
) -> Value {
    let (Some(header), Some(request)) = (header, request.as_object_mut()) else {
        return request;
    };
    request.insert(
        "prompt".to_owned(),
        serde_json::to_value(&header.prompt).unwrap_or(Value::Null),
    );
    request.insert(
        "requestConfig".to_owned(),
        serde_json::to_value(&header.prompt.config).unwrap_or(Value::Null),
    );
    if include_change && let Some(change) = &header.change {
        request.insert(
            "promptChange".to_owned(),
            serde_json::to_value(change).unwrap_or(Value::Null),
        );
    }
    Value::Object(request.clone())
}

fn with_request_config(mut node: Value, prompt: Option<&ConversationPromptSnapshot>) -> Value {
    if let (Some(prompt), Some(node)) = (prompt, node.as_object_mut()) {
        node.insert(
            "requestConfig".to_owned(),
            serde_json::to_value(&prompt.config).unwrap_or(Value::Null),
        );
    }
    node
}

fn index_tools(prompt: &ConversationPromptSnapshot) -> IndexMap<String, Value> {
    prompt
        .tools
        .iter()
        .filter_map(|tool| {
            tool.get("name")
                .and_then(Value::as_str)
                .map(|name| (name.to_owned(), tool.clone()))
        })
        .collect()
}

fn capture_schemas(
    block: &Value,
    tools_by_name: &IndexMap<String, Value>,
    output: &mut IndexMap<String, Value>,
) {
    let name = if block.get("kind").is_some() {
        block
            .get("call")
            .and_then(|call| call.get("name"))
            .and_then(Value::as_str)
    } else {
        block.get("name").and_then(Value::as_str)
    };
    if let (Some(call_id), Some(schema)) = (
        block.get("callId").and_then(Value::as_str),
        name.and_then(|name| tools_by_name.get(name)),
    ) {
        output.insert(call_id.to_owned(), schema.clone());
    }
    for child in block
        .get("subCalls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        capture_schemas(child, tools_by_name, output);
    }
}

fn interrupt_compactions(requests: &mut [Value], boundaries: &[(f64, i64)]) {
    let mut next_request = 0_usize;
    let mut running = Vec::<usize>::new();
    for (seq, time) in boundaries {
        while let Some(request) = requests.get(next_request) {
            if request_start_seq(request) >= *seq {
                break;
            }
            if request.get("purpose").and_then(Value::as_str) == Some("compaction")
                && request.get("status").and_then(Value::as_str) == Some("running")
            {
                running.push(next_request);
            }
            next_request += 1;
        }
        let index = loop {
            let Some(index) = running.pop() else {
                break None;
            };
            if requests[index].get("status").and_then(Value::as_str) == Some("running") {
                break Some(index);
            }
        };
        let Some(index) = index else {
            continue;
        };
        let Some(request) = requests[index].as_object_mut() else {
            continue;
        };
        request.insert("completedAt".to_owned(), json!(time));
        request.insert("status".to_owned(), json!("error"));
        request.insert(
            "error".to_owned(),
            json!("Compaction was interrupted before completion."),
        );
    }
}

fn apply_turn_errors(requests: &mut [Value], endings: &[(i64, i64, Option<String>)]) {
    let mut last_assistant = IndexMap::<i64, usize>::new();
    for (index, request) in requests.iter().enumerate() {
        if request.get("purpose").and_then(Value::as_str) == Some("assistant")
            && let Some(turn) = request.get("turn").and_then(Value::as_i64)
        {
            last_assistant.insert(turn, index);
        }
    }
    for (turn, time, error) in endings {
        let (Some(error), Some(index)) = (error, last_assistant.get(turn).copied()) else {
            continue;
        };
        let Some(request) = requests[index].as_object_mut() else {
            continue;
        };
        if request.get("completedAt").is_none_or(Value::is_null) {
            request.insert("completedAt".to_owned(), json!(time));
        }
        request.insert("status".to_owned(), json!("error"));
        request.insert("error".to_owned(), json!(error));
    }
}

fn snapshot_value(snapshot: &TrajectorySnapshot) -> Value {
    let event_locations = snapshot
        .event_locations
        .iter()
        .map(|(seq, location)| json!([seq.get(), location]))
        .collect::<Vec<_>>();
    json!({
        "eventNodes": snapshot.event_nodes,
        "eventLocations": event_locations,
        "requests": snapshot.requests,
        "callSchemas": snapshot.call_schemas,
        "partial": snapshot.partial,
        "runningCalls": snapshot.running_calls,
    })
}

fn placement_anchor(node: &ConversationViewNode) -> Option<f64> {
    node.placement
        .as_ref()
        .map(|placement| placement.anchor_seq)
}

fn request_start_seq(request: &Value) -> f64 {
    request
        .get("startSeq")
        .and_then(Value::as_f64)
        .unwrap_or_default()
}

fn node_seq(node: &Value) -> f64 {
    node.get("seq").and_then(Value::as_f64).unwrap_or_default()
}

fn u64_as_f64(value: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}
