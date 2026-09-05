use std::{cmp::Ordering, rc::Rc};

use indexmap::IndexMap;
use seekdeep_client_runtime::{
    AssemblerViewBuilder, AssemblerViewDefinition, ConversationAssemblerError,
    ConversationBoundaryStatus, ConversationLocation, ConversationTimelineSnapshot,
    ConversationViewNode, ConversationVisibility,
};
use serde_json::{Value, json};

/// Chat target name and encoded snapshot marker.
pub const CHAT_VIEW_TARGET: &str = "chat";
/// Marker read by the browser Session snapshot normalizer.
pub const CHAT_SNAPSHOT_ENCODING: &str = "seekdeep-chat-v1";

/// Incremental keyed Chat snapshot builder.
#[derive(Default)]
pub struct ConversationChatSnapshotBuilder {
    nodes: IndexMap<String, Rc<ConversationViewNode>>,
}

impl ConversationChatSnapshotBuilder {
    fn snapshot(&self, timeline: &ConversationTimelineSnapshot) -> Rc<Value> {
        let visible = ordered_visible(self.nodes.values());
        let order = visible
            .iter()
            .map(|node| Value::String(node.key.clone()))
            .collect::<Vec<_>>();
        Rc::new(json!({
            "encoding": CHAT_SNAPSHOT_ENCODING,
            "order": order,
            "nodes": self.nodes.values().map(|node| node_value(node)).collect::<Vec<_>>(),
            "locations": location_index(&visible),
            "timeline": timeline_value(timeline),
            "legacy": legacy_value(self.nodes.values(), timeline),
        }))
    }
}

impl AssemblerViewBuilder for ConversationChatSnapshotBuilder {
    fn empty(&self) -> Rc<Value> {
        self.snapshot(&ConversationTimelineSnapshot::default())
    }

    fn replace(
        &mut self,
        nodes: &[Rc<ConversationViewNode>],
        timeline: Rc<ConversationTimelineSnapshot>,
    ) -> Result<Rc<Value>, ConversationAssemblerError> {
        self.nodes = nodes
            .iter()
            .map(|node| (node.key.clone(), node.clone()))
            .collect();
        Ok(self.snapshot(&timeline))
    }

    fn apply(
        &mut self,
        upserts: &[Rc<ConversationViewNode>],
        timeline: Rc<ConversationTimelineSnapshot>,
    ) -> Result<Rc<Value>, ConversationAssemblerError> {
        for node in upserts {
            self.nodes.insert(node.key.clone(), node.clone());
        }
        Ok(self.snapshot(&timeline))
    }
}

/// Builds the Chat target view definition.
#[must_use]
pub fn conversation_chat_view_definition() -> AssemblerViewDefinition {
    AssemblerViewDefinition {
        target: CHAT_VIEW_TARGET.to_owned(),
        create: Rc::new(|| Box::new(ConversationChatSnapshotBuilder::default())),
    }
}

fn ordered_visible<'a>(
    nodes: impl Iterator<Item = &'a Rc<ConversationViewNode>>,
) -> Vec<Rc<ConversationViewNode>> {
    let mut nodes = nodes
        .filter(|node| {
            node.chat
                .as_ref()
                .is_some_and(|chat| chat.visibility == ConversationVisibility::Visible)
        })
        .cloned()
        .collect::<Vec<_>>();
    nodes.sort_by(|left, right| {
        let left_anchor = left.chat.as_ref().map_or(0.0, |chat| chat.anchor_seq);
        let right_anchor = right.chat.as_ref().map_or(0.0, |chat| chat.anchor_seq);
        left_anchor
            .partial_cmp(&right_anchor)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.key.cmp(&right.key))
    });
    nodes
}

fn node_value(node: &ConversationViewNode) -> Value {
    let chat = node.chat.as_ref().expect("Chat target node has metadata");
    json!({
        "key": node.key,
        "kind": node.kind,
        "id": node.id,
        "target": node.target,
        "anchorSeq": chat.anchor_seq,
        "location": location_value(&chat.location),
        "visibility": match chat.visibility {
            ConversationVisibility::Visible => "visible",
            ConversationVisibility::Hidden => "hidden",
        },
        "data": node.data.as_ref().clone(),
    })
}

fn location_value(location: &ConversationLocation) -> Value {
    match location {
        ConversationLocation::Session => json!({"kind": "session"}),
        ConversationLocation::Turn { turn } => json!({
            "kind": "turn", "turn": turn.turn, "turnStatus": status_name(turn.status),
            "turnEnd": turn.end.as_ref().map(|end| end.seq),
            "turnTail": turn.data.get("turn-tail").map(|value| value.as_ref().clone()),
        }),
        ConversationLocation::Step { turn, step } => {
            json!({
                "kind": "step", "turn": turn.turn, "step": step.step,
                "turnStatus": status_name(turn.status),
                "turnEnd": turn.end.as_ref().map(|end| end.seq),
                "stepStatus": status_name(step.status),
                "stepEnd": step.end.as_ref().map(|end| end.seq),
                "turnTail": turn.data.get("turn-tail").map(|value| value.as_ref().clone()),
                "assistantStep": step.data.get("assistant-step").map(|value| value.as_ref().clone()),
            })
        }
        ConversationLocation::Unresolved => json!({"kind": "unresolved"}),
    }
}

fn location_index(nodes: &[Rc<ConversationViewNode>]) -> Value {
    let mut turns = IndexMap::<u64, Vec<Value>>::new();
    let mut steps = IndexMap::<(u64, u64), Vec<Value>>::new();
    for node in nodes {
        let Some(chat) = &node.chat else {
            continue;
        };
        match &chat.location {
            ConversationLocation::Turn { turn } => {
                turns.entry(turn.turn).or_default().push(json!(node.key));
            }
            ConversationLocation::Step { turn, step } => {
                turns.entry(turn.turn).or_default().push(json!(node.key));
                steps
                    .entry((turn.turn, step.step))
                    .or_default()
                    .push(json!(node.key));
            }
            ConversationLocation::Session | ConversationLocation::Unresolved => {}
        }
    }
    json!({
        "turns": turns.into_iter().map(|(turn, keys)| json!([turn, keys])).collect::<Vec<_>>(),
        "steps": steps.into_iter().map(|((turn, step), keys)| json!([turn, step, keys])).collect::<Vec<_>>(),
    })
}

fn timeline_value(timeline: &ConversationTimelineSnapshot) -> Value {
    json!({
        "turnOrder": timeline.turn_order.as_ref(),
        "turns": timeline.turn_order.iter().filter_map(|number| timeline.turns.get(number)).map(|turn| {
            json!({
                "turn": turn.turn,
                "start": turn.start.as_ref().map(|event| event.wire_value()),
                "end": turn.end.as_ref().map(|event| event.wire_value()),
                "status": status_name(turn.status),
                "data": {"turn-tail": turn.data.get("turn-tail").map(|value| value.as_ref().clone())},
                "steps": turn.steps.iter().map(|step| json!({
                    "turn": step.turn,
                    "step": step.step,
                    "start": step.start.as_ref().map(|event| event.wire_value()),
                    "end": step.end.as_ref().map(|event| event.wire_value()),
                    "status": status_name(step.status),
                    "data": {"assistant-step": step.data.get("assistant-step").map(|value| value.as_ref().clone())},
                })).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
    })
}

fn status_name(status: ConversationBoundaryStatus) -> &'static str {
    match status {
        ConversationBoundaryStatus::Open => "open",
        ConversationBoundaryStatus::Closed => "closed",
        ConversationBoundaryStatus::Unknown => "unknown",
    }
}

fn legacy_value<'a>(
    nodes: impl Iterator<Item = &'a Rc<ConversationViewNode>>,
    timeline: &ConversationTimelineSnapshot,
) -> Value {
    let mut finalized = Vec::<(f64, Value)>::new();
    let mut running = Vec::<(f64, Value)>::new();
    let mut partials = Vec::<(f64, Value)>::new();
    for node in nodes {
        let Some(chat) = &node.chat else {
            continue;
        };
        legacy_contribution(
            node,
            chat.anchor_seq,
            &mut finalized,
            &mut running,
            &mut partials,
        );
    }
    finalized.sort_by(|left, right| {
        left.1
            .get("seq")
            .and_then(Value::as_f64)
            .unwrap_or(left.0)
            .partial_cmp(
                &right
                    .1
                    .get("seq")
                    .and_then(Value::as_f64)
                    .unwrap_or(right.0),
            )
            .unwrap_or(Ordering::Equal)
    });
    running.sort_by(|left, right| left.0.partial_cmp(&right.0).unwrap_or(Ordering::Equal));
    partials.sort_by(|left, right| left.0.partial_cmp(&right.0).unwrap_or(Ordering::Equal));
    let turn_timings = timeline
        .turn_order
        .iter()
        .filter_map(|number| timeline.turns.get(number))
        .filter_map(|turn| {
            turn.start.as_ref().map(|start| {
                json!([turn.turn, {
                    "startTime": start.time,
                    "endTime": turn.end.as_ref().map(|end| end.time),
                }])
            })
        })
        .collect::<Vec<_>>();
    let turn_ends = timeline
        .turn_order
        .iter()
        .filter_map(|number| timeline.turns.get(number))
        .filter_map(|turn| turn.end.as_ref().map(|end| json!([turn.turn, end.seq])))
        .collect::<Vec<_>>();
    json!({
        "nodes": finalized.into_iter().map(|(_, node)| node).collect::<Vec<_>>(),
        "turnTimings": turn_timings,
        "turnEnds": turn_ends,
        "partial": partials.pop().map_or(Value::Null, |(_, partial)| partial),
        "runningCalls": running.into_iter().map(|(_, call)| call).collect::<Vec<_>>(),
    })
}

fn legacy_contribution(
    node: &ConversationViewNode,
    anchor: f64,
    finalized: &mut Vec<(f64, Value)>,
    running: &mut Vec<(f64, Value)>,
    partials: &mut Vec<(f64, Value)>,
) {
    let visible = node
        .chat
        .as_ref()
        .is_some_and(|chat| chat.visibility == ConversationVisibility::Visible);
    if !visible && node.kind != "assistant-step" {
        return;
    }
    let data = node.data.as_ref();
    match node.kind.as_str() {
        "user" | "steering" | "context" | "command" | "compaction" | "turn-error"
        | "turn-max-tokens" | "unknown" => finalized.push((anchor, data.clone())),
        "assistant-step" => {
            if data.get("status").and_then(Value::as_str) == Some("running") {
                if visible {
                    partials.push((
                        anchor,
                        json!({
                            "turn": data.get("turn").cloned().unwrap_or(Value::Null),
                            "step": data.get("step").cloned().unwrap_or(Value::Null),
                            "blocks": data.get("blocks").cloned().unwrap_or_else(|| json!([])),
                        }),
                    ));
                }
            } else if let Some(final_node) = data.get("finalNode") {
                finalized.push((anchor, final_node.clone()));
            }
        }
        "tool-call" => {
            if let Some(root) = data.get("root") {
                if root.get("kind").is_some() {
                    finalized.push((anchor, root.clone()));
                } else {
                    running.push((anchor, root.clone()));
                }
            }
        }
        "manual-compaction" => {
            if let Some(command) = data.get("command") {
                finalized.push((anchor, command.clone()));
            }
            if let Some(compaction) = data.get("compaction").filter(|value| !value.is_null()) {
                finalized.push((anchor, compaction.clone()));
            }
        }
        "model-retry" => {
            if let Some(attempts) = data.get("attempts").and_then(Value::as_array) {
                finalized.extend(attempts.iter().cloned().map(|attempt| (anchor, attempt)));
            }
        }
        _ => {}
    }
}
