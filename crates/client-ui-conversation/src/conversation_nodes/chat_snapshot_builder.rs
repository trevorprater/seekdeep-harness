use std::{cmp::Ordering, rc::Rc};

use super::json_value;
use indexmap::IndexMap;
use seekdeep_client_runtime::ConversationValue as Value;
use seekdeep_client_runtime::{
    AssemblerViewBuilder, AssemblerViewDefinition, ConversationAssemblerError,
    ConversationBoundaryStatus, ConversationLocation, ConversationTimelineSnapshot,
    ConversationViewNode, ConversationVisibility,
};

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
            .map(|node| json_value(&node.key))
            .collect::<Vec<_>>();
        Rc::new(Value::object([
            ("encoding", json_value(CHAT_SNAPSHOT_ENCODING)),
            ("order", Value::array(&order)),
            (
                "nodes",
                Value::array(
                    &self
                        .nodes
                        .values()
                        .map(|node| node_value(node))
                        .collect::<Vec<_>>(),
                ),
            ),
            ("locations", location_index(&visible)),
            ("timeline", timeline_value(timeline)),
            ("legacy", legacy_value(self.nodes.values(), timeline)),
        ]))
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
    Value::object([
        ("key", json_value(&node.key)),
        ("kind", json_value(&node.kind)),
        ("id", json_value(&node.id)),
        ("target", json_value(&node.target)),
        ("anchorSeq", json_value(&chat.anchor_seq)),
        ("location", location_value(&chat.location)),
        (
            "visibility",
            json_value(match chat.visibility {
                ConversationVisibility::Visible => "visible",
                ConversationVisibility::Hidden => "hidden",
            }),
        ),
        ("data", node.data.as_ref().clone()),
    ])
}

fn location_value(location: &ConversationLocation) -> Value {
    match location {
        ConversationLocation::Session => Value::object([("kind", json_value("session"))]),
        ConversationLocation::Turn { turn } => Value::object([
            ("kind", json_value("turn")),
            ("turn", json_value(&turn.turn)),
            ("turnStatus", json_value(status_name(turn.status))),
            ("turnEnd", json_value(&turn.end.as_ref().map(|end| end.seq))),
            (
                "turnTail",
                json_value(
                    &turn
                        .data
                        .get("turn-tail")
                        .map(|value| value.as_ref().clone()),
                ),
            ),
        ]),
        ConversationLocation::Step { turn, step } => Value::object([
            ("kind", json_value("step")),
            ("turn", json_value(&turn.turn)),
            ("step", json_value(&step.step)),
            ("turnStatus", json_value(status_name(turn.status))),
            ("turnEnd", json_value(&turn.end.as_ref().map(|end| end.seq))),
            ("stepStatus", json_value(status_name(step.status))),
            ("stepEnd", json_value(&step.end.as_ref().map(|end| end.seq))),
            (
                "turnTail",
                json_value(
                    &turn
                        .data
                        .get("turn-tail")
                        .map(|value| value.as_ref().clone()),
                ),
            ),
            (
                "assistantStep",
                json_value(
                    &step
                        .data
                        .get("assistant-step")
                        .map(|value| value.as_ref().clone()),
                ),
            ),
        ]),
        ConversationLocation::Unresolved => Value::object([("kind", json_value("unresolved"))]),
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
                turns
                    .entry(turn.turn)
                    .or_default()
                    .push(json_value(&node.key));
            }
            ConversationLocation::Step { turn, step } => {
                turns
                    .entry(turn.turn)
                    .or_default()
                    .push(json_value(&node.key));
                steps
                    .entry((turn.turn, step.step))
                    .or_default()
                    .push(json_value(&node.key));
            }
            ConversationLocation::Session | ConversationLocation::Unresolved => {}
        }
    }
    Value::object([
        ("turns", json_value(&turns.into_iter().collect::<Vec<_>>())),
        (
            "steps",
            json_value(
                &steps
                    .into_iter()
                    .map(|((turn, step), keys)| (turn, step, keys))
                    .collect::<Vec<_>>(),
            ),
        ),
    ])
}

fn timeline_value(timeline: &ConversationTimelineSnapshot) -> Value {
    let turns = timeline
        .turn_order
        .iter()
        .filter_map(|number| timeline.turns.get(number))
        .map(|turn| {
            let steps = turn
                .steps
                .iter()
                .map(|step| {
                    Value::object([
                        ("turn", json_value(&step.turn)),
                        ("step", json_value(&step.step)),
                        (
                            "start",
                            json_value(&step.start.as_ref().map(|event| event.wire_value())),
                        ),
                        (
                            "end",
                            json_value(&step.end.as_ref().map(|event| event.wire_value())),
                        ),
                        ("status", json_value(status_name(step.status))),
                        (
                            "data",
                            Value::object([(
                                "assistant-step",
                                json_value(
                                    &step
                                        .data
                                        .get("assistant-step")
                                        .map(|value| value.as_ref().clone()),
                                ),
                            )]),
                        ),
                    ])
                })
                .collect::<Vec<_>>();
            Value::object([
                ("turn", json_value(&turn.turn)),
                (
                    "start",
                    json_value(&turn.start.as_ref().map(|event| event.wire_value())),
                ),
                (
                    "end",
                    json_value(&turn.end.as_ref().map(|event| event.wire_value())),
                ),
                ("status", json_value(status_name(turn.status))),
                (
                    "data",
                    Value::object([(
                        "turn-tail",
                        json_value(
                            &turn
                                .data
                                .get("turn-tail")
                                .map(|value| value.as_ref().clone()),
                        ),
                    )]),
                ),
                ("steps", Value::array(&steps)),
            ])
        })
        .collect::<Vec<_>>();
    Value::object([
        ("turnOrder", json_value(timeline.turn_order.as_ref())),
        ("turns", Value::array(&turns)),
    ])
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
            .get_value("seq")
            .and_then(Value::as_f64)
            .unwrap_or(left.0)
            .partial_cmp(
                &right
                    .1
                    .get_value("seq")
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
                json_value(&(
                    turn.turn,
                    Value::object([
                        ("startTime", json_value(&start.time)),
                        (
                            "endTime",
                            json_value(&turn.end.as_ref().map(|end| end.time)),
                        ),
                    ]),
                ))
            })
        })
        .collect::<Vec<_>>();
    let turn_ends = timeline
        .turn_order
        .iter()
        .filter_map(|number| timeline.turns.get(number))
        .filter_map(|turn| {
            turn.end
                .as_ref()
                .map(|end| json_value(&(turn.turn, end.seq)))
        })
        .collect::<Vec<_>>();
    Value::object([
        (
            "nodes",
            json_value(
                &finalized
                    .into_iter()
                    .map(|(_, node)| node)
                    .collect::<Vec<_>>(),
            ),
        ),
        ("turnTimings", Value::array(&turn_timings)),
        ("turnEnds", Value::array(&turn_ends)),
        (
            "partial",
            partials
                .pop()
                .map_or_else(|| json_value(&()), |(_, partial)| partial),
        ),
        (
            "runningCalls",
            json_value(
                &running
                    .into_iter()
                    .map(|(_, call)| call)
                    .collect::<Vec<_>>(),
            ),
        ),
    ])
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
            if data.get_value("status").and_then(Value::as_str) == Some("running") {
                if visible {
                    partials.push((
                        anchor,
                        Value::object([
                            ("turn", json_value(&data.get_value("turn"))),
                            ("step", json_value(&data.get_value("step"))),
                            (
                                "blocks",
                                data.get_value("blocks")
                                    .cloned()
                                    .unwrap_or_else(|| Value::array(&[])),
                            ),
                        ]),
                    ));
                }
            } else if let Some(final_node) = data.get_value("finalNode") {
                finalized.push((anchor, final_node.clone()));
            }
        }
        "tool-call" => {
            if let Some(root) = data.get_value("root") {
                if root.get_value("kind").is_some() {
                    finalized.push((anchor, root.clone()));
                } else {
                    running.push((anchor, root.clone()));
                }
            }
        }
        "manual-compaction" => {
            if let Some(command) = data.get_value("command") {
                finalized.push((anchor, command.clone()));
            }
            if let Some(compaction) = data
                .get_value("compaction")
                .filter(|value| !value.is_null())
            {
                finalized.push((anchor, compaction.clone()));
            }
        }
        "model-retry" => {
            if let Some(attempts) = data.get_value("attempts").and_then(Value::as_array) {
                finalized.extend(attempts.iter().cloned().map(|attempt| (anchor, attempt)));
            }
        }
        _ => {}
    }
}
