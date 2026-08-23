//! Conversation Context lifecycle, dependency, Location, and view publication parity.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use indexmap::IndexMap;
use seekdeep_client_runtime::{
    AssemblerEventDefinitions, AssemblerNodeDefinition, AssemblerViewBuilder,
    AssemblerViewDefinition, AssemblerViewDefinitions, ConversationAssemblerError,
    ConversationContextReader, ConversationEventInput, ConversationLocation,
    ConversationLocationData, ConversationLocationDataScope, ConversationLocationEvent,
    ConversationMatch, ConversationMatchResult, ConversationMatchRole, ConversationNodeAssembler,
    ConversationNodeContext, ConversationPublication, ConversationTimelineSnapshot,
    ConversationViewNode, conversation_context_key,
};
use serde_json::{Map, Value, json};

struct EventDefinitions {
    entries: Vec<Rc<AssemblerNodeDefinition>>,
    fallback: Option<Rc<AssemblerNodeDefinition>>,
}

impl AssemblerEventDefinitions for EventDefinitions {
    fn entries(&self) -> Vec<Rc<AssemblerNodeDefinition>> {
        self.entries.clone()
    }

    fn fallback_entry(&self) -> Option<Rc<AssemblerNodeDefinition>> {
        self.fallback.clone()
    }
}

struct ViewDefinitions(Vec<Rc<AssemblerViewDefinition>>);

impl AssemblerViewDefinitions for ViewDefinitions {
    fn entries(&self) -> Vec<Rc<AssemblerViewDefinition>> {
        self.0.clone()
    }
}

struct TestViewBuilder {
    nodes: IndexMap<String, Rc<ConversationViewNode>>,
    order: Vec<String>,
    apply_calls: Rc<Cell<u64>>,
}

impl TestViewBuilder {
    fn snapshot(&self) -> Rc<Value> {
        let nodes = self
            .nodes
            .iter()
            .map(|(key, node)| (key.clone(), node.data.as_ref().clone()))
            .collect::<Map<_, _>>();
        Rc::new(json!({"order":self.order,"nodes":nodes}))
    }
}

impl AssemblerViewBuilder for TestViewBuilder {
    fn empty(&self) -> Rc<Value> {
        self.snapshot()
    }

    fn replace(
        &mut self,
        nodes: &[Rc<ConversationViewNode>],
        _timeline: Rc<ConversationTimelineSnapshot>,
    ) -> Result<Rc<Value>, ConversationAssemblerError> {
        self.nodes = nodes
            .iter()
            .map(|node| (node.key.clone(), node.clone()))
            .collect();
        self.order = nodes.iter().map(|node| node.key.clone()).collect();
        Ok(self.snapshot())
    }

    fn apply(
        &mut self,
        upserts: &[Rc<ConversationViewNode>],
        _timeline: Rc<ConversationTimelineSnapshot>,
    ) -> Result<Rc<Value>, ConversationAssemblerError> {
        self.apply_calls.set(self.apply_calls.get() + 1);
        for node in upserts {
            if !self.nodes.contains_key(&node.key) {
                self.order.push(node.key.clone());
            }
            self.nodes.insert(node.key.clone(), node.clone());
        }
        Ok(self.snapshot())
    }
}

fn view(apply_calls: Rc<Cell<u64>>) -> Rc<AssemblerViewDefinition> {
    Rc::new(AssemblerViewDefinition {
        target: "chat".to_owned(),
        create: Rc::new(move || {
            Box::new(TestViewBuilder {
                nodes: IndexMap::new(),
                order: Vec::new(),
                apply_calls: apply_calls.clone(),
            })
        }),
    })
}

fn assembler(
    definitions: Vec<Rc<AssemblerNodeDefinition>>,
    fallback: Option<Rc<AssemblerNodeDefinition>>,
    apply_calls: Rc<Cell<u64>>,
) -> ConversationNodeAssembler {
    ConversationNodeAssembler::new(
        Rc::new(EventDefinitions {
            entries: definitions,
            fallback,
        }),
        Rc::new(ViewDefinitions(vec![view(apply_calls)])),
    )
}

fn at(seq: u64, event_type: &str, data: Value) -> ConversationEventInput {
    ConversationEventInput {
        event: ConversationLocationEvent::new(seq, event_type, data),
        view: None,
    }
}

fn node(context: &ConversationNodeContext, data: Value) -> Rc<ConversationViewNode> {
    Rc::new(ConversationViewNode {
        key: context.key.clone(),
        kind: context.kind.clone(),
        id: context.id.clone(),
        target: "chat".to_owned(),
        data: Rc::new(data),
    })
}

fn snapshot(assembler: &ConversationNodeAssembler) -> Rc<Value> {
    assembler.snapshot("chat").unwrap()
}

fn node_data(assembler: &ConversationNodeAssembler, key: &str) -> Value {
    snapshot(assembler)["nodes"][key].clone()
}

fn context_number(context: &ConversationNodeContext) -> i64 {
    context.state.as_deref().and_then(Value::as_i64).unwrap()
}

fn basic_definition(
    kind: &str,
    matcher: impl Fn(
        &ConversationLocationEvent,
    ) -> Result<Option<ConversationMatchResult>, ConversationAssemblerError>
    + 'static,
    start: impl Fn(
        &ConversationNodeContext,
        &Rc<ConversationMatch>,
        &mut dyn ConversationContextReader,
    ) -> Result<Option<Rc<Value>>, ConversationAssemblerError>
    + 'static,
    update: impl Fn(
        &ConversationNodeContext,
        &Rc<ConversationMatch>,
    ) -> Result<Option<Rc<Value>>, ConversationAssemblerError>
    + 'static,
) -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: kind.to_owned(),
        target: Some("chat".to_owned()),
        match_event: Rc::new(matcher),
        start: Rc::new(start),
        update: Rc::new(update),
        publication: None,
        build_location_data: None,
        build_view_node: None,
    }
}

#[test]
fn exact_business_id_append_updates_only_its_context_and_keeps_match_collection_identity() {
    let starts = Rc::new(Cell::new(0));
    let updates = Rc::new(Cell::new(0));
    let match_collections = Rc::new(RefCell::new(Vec::<usize>::new()));
    let observed_starts = starts.clone();
    let observed_collections = match_collections.clone();
    let observed_updates = updates.clone();
    let update_collections = match_collections.clone();
    let mut definition = basic_definition(
        "tool",
        |event| {
            let result = match event.event_type.as_str() {
                "tool/call" => Some(ConversationMatchResult {
                    id: event.data["callId"].as_str().unwrap().to_owned(),
                    role: ConversationMatchRole::Start,
                }),
                "tool/result" => Some(ConversationMatchResult {
                    id: event.data["callId"].as_str().unwrap().to_owned(),
                    role: ConversationMatchRole::Update,
                }),
                _ => None,
            };
            Ok(result)
        },
        move |context, accepted, _reader| {
            observed_starts.set(observed_starts.get() + 1);
            observed_collections
                .borrow_mut()
                .push(Rc::as_ptr(&context.matches) as usize);
            Ok(Some(Rc::new(json!({
                "callSeq":accepted.event.seq,
                "results":0
            }))))
        },
        move |context, _accepted| {
            observed_updates.set(observed_updates.get() + 1);
            update_collections
                .borrow_mut()
                .push(Rc::as_ptr(&context.matches) as usize);
            Ok(Some(Rc::new(json!({
                "callSeq":context.state.as_ref().unwrap()["callSeq"],
                "results":context.state.as_ref().unwrap()["results"].as_u64().unwrap()+1
            }))))
        },
    );
    definition.build_view_node = Some(Rc::new(|context| {
        Ok(Some(node(
            context,
            context.state.as_deref().cloned().unwrap_or(Value::Null),
        )))
    }));
    let apply_calls = Rc::new(Cell::new(0));
    let mut assembler = assembler(vec![Rc::new(definition)], None, apply_calls);
    assembler
        .replace_window(
            &[
                at(1, "tool/call", json!({"callId":"a"})),
                at(2, "tool/call", json!({"callId":"b"})),
            ],
            false,
        )
        .unwrap();
    assembler.flush().unwrap();
    starts.set(0);

    assembler
        .append(&at(3, "tool/result", json!({"callId":"a"})))
        .unwrap();
    assembler.flush().unwrap();
    assert_eq!(starts.get(), 0);
    assert_eq!(updates.get(), 1);
    assert_eq!(
        node_data(&assembler, &conversation_context_key("tool", "a")),
        json!({"callSeq":1,"results":1})
    );
    assert_eq!(
        node_data(&assembler, &conversation_context_key("tool", "b")),
        json!({"callSeq":2,"results":0})
    );
    let collections = match_collections.borrow();
    assert_eq!(collections[0], collections[2]);
    assert_ne!(collections[0], collections[1]);
}

#[test]
fn prepend_collects_updates_then_replays_once_when_the_start_arrives() {
    let updates = Rc::new(Cell::new(0));
    let observed = updates.clone();
    let mut definition = basic_definition(
        "linear",
        |event| {
            Ok(match event.event_type.as_str() {
                "linear/start" => Some(ConversationMatchResult {
                    id: "one".to_owned(),
                    role: ConversationMatchRole::Start,
                }),
                "linear/update" => Some(ConversationMatchResult {
                    id: "one".to_owned(),
                    role: ConversationMatchRole::Update,
                }),
                _ => None,
            })
        },
        |_context, _accepted, _reader| Ok(Some(Rc::new(json!(0)))),
        move |context, _accepted| {
            observed.set(observed.get() + 1);
            Ok(Some(Rc::new(json!(context_number(context) + 1))))
        },
    );
    definition.build_view_node = Some(Rc::new(|context| {
        Ok(Some(node(
            context,
            context
                .state
                .as_deref()
                .cloned()
                .unwrap_or(json!("pending")),
        )))
    }));
    let mut assembler = assembler(vec![Rc::new(definition)], None, Rc::new(Cell::new(0)));
    assembler
        .replace_window(
            &[
                at(10, "linear/update", json!({})),
                at(11, "linear/update", json!({})),
            ],
            true,
        )
        .unwrap();
    assembler.flush().unwrap();
    assert_eq!(
        node_data(&assembler, &conversation_context_key("linear", "one")),
        "pending"
    );
    assembler
        .prepend(
            &[
                at(1, "linear/start", json!({})),
                at(2, "linear/update", json!({})),
            ],
            false,
        )
        .unwrap();
    assembler.flush().unwrap();
    assert_eq!(updates.get(), 3);
    assert_eq!(
        node_data(&assembler, &conversation_context_key("linear", "one")),
        3
    );
}

#[test]
fn start_after_an_earlier_update_fails_without_reverse_folding() {
    let definition = basic_definition(
        "invalid",
        |event| {
            Ok(match event.event_type.as_str() {
                "turn/end" => Some(ConversationMatchResult {
                    id: "one".to_owned(),
                    role: ConversationMatchRole::Start,
                }),
                "turn/start" => Some(ConversationMatchResult {
                    id: "one".to_owned(),
                    role: ConversationMatchRole::Update,
                }),
                _ => None,
            })
        },
        |_context, _accepted, _reader| Ok(Some(Rc::new(Value::Null))),
        |context, _accepted| Ok(context.state.clone()),
    );
    let mut assembler = assembler(vec![Rc::new(definition)], None, Rc::new(Cell::new(0)));
    let error = assembler
        .replace_window(
            &[
                at(1, "turn/start", json!({"turn":1})),
                at(2, "turn/end", json!({"turn":1})),
            ],
            false,
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("received an update before its start Match")
    );
}

fn source_definition(
    kind: &str,
    start_type: &'static str,
    update_type: &'static str,
) -> AssemblerNodeDefinition {
    let mut definition = basic_definition(
        kind,
        move |event| {
            Ok(if event.event_type == start_type {
                Some(ConversationMatchResult {
                    id: "one".to_owned(),
                    role: ConversationMatchRole::Start,
                })
            } else if event.event_type == update_type {
                Some(ConversationMatchResult {
                    id: "one".to_owned(),
                    role: ConversationMatchRole::Update,
                })
            } else {
                None
            })
        },
        |_context, accepted, _reader| {
            Ok(Some(Rc::new(
                accepted
                    .event
                    .data
                    .get("value")
                    .cloned()
                    .unwrap_or(json!(1)),
            )))
        },
        |_context, accepted| Ok(Some(Rc::new(accepted.event.data["value"].clone()))),
    );
    definition.build_view_node = Some(Rc::new(|_context| Ok(None)));
    definition
}

#[test]
fn reader_window_gap_replays_when_prepend_supplies_a_nearer_predecessor_or_closes_the_gap() {
    let starts = Rc::new(Cell::new(0));
    let observed = starts.clone();
    let mut consumer = basic_definition(
        "consumer",
        |event| {
            Ok(
                (event.event_type == "consumer/start").then(|| ConversationMatchResult {
                    id: "one".to_owned(),
                    role: ConversationMatchRole::Start,
                }),
            )
        },
        move |_context, _accepted, reader| {
            observed.set(observed.get() + 1);
            Ok(Some(Rc::new(
                reader
                    .previous("source")
                    .map_or(json!(-1), |previous| previous.state.as_ref().clone()),
            )))
        },
        |context, _accepted| Ok(context.state.clone()),
    );
    consumer.build_view_node = Some(Rc::new(|context| {
        Ok(Some(node(
            context,
            context.state.as_deref().cloned().unwrap(),
        )))
    }));
    let mut with_predecessor = assembler(
        vec![
            Rc::new(source_definition("source", "source/start", "source/update")),
            Rc::new(consumer),
        ],
        None,
        Rc::new(Cell::new(0)),
    );
    with_predecessor
        .replace_window(&[at(10, "consumer/start", json!({}))], true)
        .unwrap();
    with_predecessor.flush().unwrap();
    assert_eq!(
        node_data(
            &with_predecessor,
            &conversation_context_key("consumer", "one")
        ),
        -1
    );
    with_predecessor
        .prepend(&[at(5, "source/start", json!({"value":7}))], false)
        .unwrap();
    with_predecessor.flush().unwrap();
    assert_eq!(starts.get(), 2);
    assert_eq!(
        node_data(
            &with_predecessor,
            &conversation_context_key("consumer", "one")
        ),
        7
    );

    let empty_starts = Rc::new(Cell::new(0));
    let observed = empty_starts.clone();
    let mut consumer = basic_definition(
        "consumer",
        |event| {
            Ok(
                (event.event_type == "consumer/start").then(|| ConversationMatchResult {
                    id: "one".to_owned(),
                    role: ConversationMatchRole::Start,
                }),
            )
        },
        move |_context, _accepted, reader| {
            observed.set(observed.get() + 1);
            Ok(Some(Rc::new(json!(
                reader.previous("missing").map_or(-1, |_| 1)
            ))))
        },
        |context, _accepted| Ok(context.state.clone()),
    );
    consumer.build_view_node = Some(Rc::new(|context| {
        Ok(Some(node(
            context,
            context.state.as_deref().cloned().unwrap(),
        )))
    }));
    let mut empty = assembler(vec![Rc::new(consumer)], None, Rc::new(Cell::new(0)));
    empty
        .replace_window(&[at(10, "consumer/start", json!({}))], true)
        .unwrap();
    assert_eq!(
        empty.prepend(&[], false).unwrap(),
        ConversationPublication::Immediate
    );
    assert_eq!(empty_starts.get(), 2);
}

#[test]
#[allow(clippy::too_many_lines)] // One dependency graph covers direct and transitive replay order.
fn append_replays_direct_and_transitive_reader_dependents_in_start_order() {
    let mut consumer = basic_definition(
        "consumer",
        |event| {
            Ok(
                (event.event_type == "consumer/start").then(|| ConversationMatchResult {
                    id: "one".to_owned(),
                    role: ConversationMatchRole::Start,
                }),
            )
        },
        |_context, _accepted, reader| {
            Ok(Some(Rc::new(
                reader
                    .previous("source")
                    .map_or(json!(-1), |previous| previous.state.as_ref().clone()),
            )))
        },
        |context, _accepted| Ok(context.state.clone()),
    );
    consumer.build_view_node = Some(Rc::new(|context| {
        Ok(Some(node(
            context,
            context.state.as_deref().cloned().unwrap(),
        )))
    }));
    let mut direct = assembler(
        vec![
            Rc::new(source_definition("source", "source/start", "source/update")),
            Rc::new(consumer),
        ],
        None,
        Rc::new(Cell::new(0)),
    );
    direct
        .replace_window(
            &[
                at(1, "source/start", json!({"value":1})),
                at(2, "consumer/start", json!({})),
            ],
            false,
        )
        .unwrap();
    assert_eq!(
        direct
            .append(&at(3, "source/update", json!({"value":2})))
            .unwrap(),
        ConversationPublication::Immediate
    );
    direct.flush().unwrap();
    assert_eq!(
        node_data(&direct, &conversation_context_key("consumer", "one")),
        2
    );

    let mut middle = basic_definition(
        "middle",
        |event| {
            Ok(
                (event.event_type == "middle/start").then(|| ConversationMatchResult {
                    id: "one".to_owned(),
                    role: ConversationMatchRole::Start,
                }),
            )
        },
        |_context, _accepted, reader| {
            let a = reader
                .previous("a")
                .and_then(|previous| previous.state.as_i64())
                .unwrap_or(0);
            let x = reader
                .previous("x")
                .and_then(|previous| previous.state.as_i64())
                .unwrap_or(0);
            Ok(Some(Rc::new(json!(a + x))))
        },
        |context, _accepted| Ok(context.state.clone()),
    );
    middle.build_view_node = Some(Rc::new(|_context| Ok(None)));
    let mut final_node = basic_definition(
        "final",
        |event| {
            Ok(
                (event.event_type == "final/start").then(|| ConversationMatchResult {
                    id: "one".to_owned(),
                    role: ConversationMatchRole::Start,
                }),
            )
        },
        |_context, _accepted, reader| {
            let a = reader
                .previous("a")
                .and_then(|previous| previous.state.as_i64())
                .unwrap_or(0);
            let middle = reader
                .previous("middle")
                .and_then(|previous| previous.state.as_i64())
                .unwrap_or(0);
            Ok(Some(Rc::new(json!(a * 100 + middle))))
        },
        |context, _accepted| Ok(context.state.clone()),
    );
    final_node.build_view_node = Some(Rc::new(|context| {
        Ok(Some(node(
            context,
            context.state.as_deref().cloned().unwrap(),
        )))
    }));
    let mut transitive = assembler(
        vec![
            Rc::new(source_definition("a", "a/start", "a/update")),
            Rc::new(source_definition("x", "x/start", "x/update")),
            Rc::new(middle),
            Rc::new(final_node),
        ],
        None,
        Rc::new(Cell::new(0)),
    );
    transitive
        .replace_window(
            &[
                at(1, "a/start", json!({"value":1})),
                at(2, "x/start", json!({"value":10})),
                at(3, "middle/start", json!({})),
                at(4, "final/start", json!({})),
            ],
            false,
        )
        .unwrap();
    transitive
        .append(&at(5, "x/update", json!({"value":20})))
        .unwrap();
    transitive
        .append(&at(6, "a/update", json!({"value":2})))
        .unwrap();
    transitive.flush().unwrap();
    assert_eq!(
        node_data(&transitive, &conversation_context_key("final", "one")),
        222
    );
}

fn location_status(location: &ConversationLocation) -> &'static str {
    match location {
        ConversationLocation::Step { step, .. } => match step.status {
            seekdeep_client_runtime::ConversationBoundaryStatus::Open => "open",
            seekdeep_client_runtime::ConversationBoundaryStatus::Closed => "closed",
            seekdeep_client_runtime::ConversationBoundaryStatus::Unknown => "unknown",
        },
        _ => "missing",
    }
}

#[test]
fn closing_a_step_replays_location_state_and_updates_only_owned_nodes() {
    let starts = Rc::new(Cell::new(0));
    let observed = starts.clone();
    let mut definition = basic_definition(
        "step-probe",
        |event| {
            Ok(
                (event.event_type == "step/start").then(|| ConversationMatchResult {
                    id: format!("{}:{}", event.data["turn"], event.data["step"]),
                    role: ConversationMatchRole::Start,
                }),
            )
        },
        move |context, accepted, _reader| {
            assert!(context.state.is_none());
            observed.set(observed.get() + 1);
            Ok(Some(Rc::new(json!(location_status(&accepted.location)))))
        },
        |context, _accepted| Ok(context.state.clone()),
    );
    definition.build_view_node = Some(Rc::new(|context| {
        Ok(Some(node(
            context,
            context.state.as_deref().cloned().unwrap(),
        )))
    }));
    let apply_calls = Rc::new(Cell::new(0));
    let mut assembler = assembler(vec![Rc::new(definition)], None, apply_calls.clone());
    assembler
        .replace_window(
            &[
                at(1, "turn/start", json!({"turn":1})),
                at(2, "step/start", json!({"turn":1,"step":1})),
            ],
            false,
        )
        .unwrap();
    assembler.flush().unwrap();
    let key = conversation_context_key("step-probe", "1:1");
    assert_eq!(node_data(&assembler, &key), "open");
    assembler
        .append(&at(3, "step/end", json!({"turn":1,"step":1})))
        .unwrap();
    assembler.flush().unwrap();
    assert_eq!(starts.get(), 2);
    assert_eq!(apply_calls.get(), 1);
    assert_eq!(node_data(&assembler, &key), "closed");
}

#[test]
fn step_then_turn_location_data_publish_in_phase_order_and_keep_reader_identity() {
    let mut definition = basic_definition(
        "scope-probe",
        |event| {
            Ok(match event.event_type.as_str() {
                "step/start" => Some(ConversationMatchResult {
                    id: "1:1".to_owned(),
                    role: ConversationMatchRole::Start,
                }),
                "scope/update" => Some(ConversationMatchResult {
                    id: "1:1".to_owned(),
                    role: ConversationMatchRole::Update,
                }),
                _ => None,
            })
        },
        |_context, _accepted, _reader| Ok(Some(Rc::new(json!({"turn":1,"step":1,"value":1})))),
        |_context, accepted| {
            Ok(Some(Rc::new(json!({
                "turn":1,"step":1,"value":accepted.event.data["value"]
            }))))
        },
    );
    definition.build_location_data = Some(Rc::new(|context, scope| {
        let state = context.state.as_ref().unwrap();
        if scope == ConversationLocationDataScope::Step {
            return Ok(Some(Rc::new(ConversationLocationData::Step {
                turn: 1,
                step: Some(1),
                key: "scope-probe".to_owned(),
                value: Rc::new(json!({"value":state["value"]})),
            })));
        }
        let seen = context.start.as_ref().and_then(|accepted| {
            if let ConversationLocation::Step { step, .. } = &accepted.location {
                step.data
                    .get("scope-probe")
                    .and_then(|value| value["value"].as_i64())
            } else {
                None
            }
        });
        Ok(Some(Rc::new(ConversationLocationData::Turn {
            turn: 1,
            key: "scope-probe".to_owned(),
            value: Rc::new(json!({"valueSeenFromStep":seen.unwrap_or(-1)})),
        })))
    }));
    definition.build_view_node = Some(Rc::new(|context| {
        let Some(ConversationLocation::Step { turn, step }) =
            context.start.as_ref().map(|accepted| &accepted.location)
        else {
            return Ok(None);
        };
        Ok(Some(node(
            context,
            json!({
                "step":step.data.get("scope-probe").unwrap()["value"],
                "turn":turn.data.get("scope-probe").unwrap()["valueSeenFromStep"]
            }),
        )))
    }));
    let mut assembler = assembler(vec![Rc::new(definition)], None, Rc::new(Cell::new(0)));
    assembler
        .replace_window(
            &[
                at(1, "turn/start", json!({"turn":1})),
                at(2, "step/start", json!({"turn":1,"step":1})),
            ],
            false,
        )
        .unwrap();
    assembler.flush().unwrap();
    let key = conversation_context_key("scope-probe", "1:1");
    assert_eq!(node_data(&assembler, &key), json!({"step":1,"turn":1}));
    assembler
        .append(&at(3, "scope/update", json!({"turn":1,"step":1,"value":2})))
        .unwrap();
    assembler.flush().unwrap();
    assert_eq!(node_data(&assembler, &key), json!({"step":2,"turn":2}));
}

#[test]
fn timeline_changes_publish_even_without_a_claiming_business_definition() {
    let apply_calls = Rc::new(Cell::new(0));
    let mut assembler = assembler(Vec::new(), None, apply_calls.clone());
    assembler.replace_window(&[], false).unwrap();
    assembler.flush().unwrap();
    assembler
        .append(&at(1, "turn/start", json!({"turn":1})))
        .unwrap();
    assembler.flush().unwrap();
    assert_eq!(apply_calls.get(), 1);
    assert_eq!(snapshot(&assembler)["order"], json!([]));
}

fn fallback_definition(starts: Rc<Cell<u64>>) -> Rc<AssemblerNodeDefinition> {
    let observed = starts;
    let mut fallback = basic_definition(
        "fallback",
        |event| {
            Ok(Some(ConversationMatchResult {
                id: event.seq.to_string(),
                role: ConversationMatchRole::Start,
            }))
        },
        move |_context, _accepted, _reader| {
            observed.set(observed.get() + 1);
            Ok(Some(Rc::new(json!("fallback"))))
        },
        |context, _accepted| Ok(context.state.clone()),
    );
    fallback.build_view_node = Some(Rc::new(|context| {
        Ok(Some(node(
            context,
            context.state.as_deref().cloned().unwrap(),
        )))
    }));
    Rc::new(fallback)
}

#[test]
fn fallback_is_target_specific_and_state_only_claims_do_not_suppress_it() {
    let state_only = Rc::new(AssemblerNodeDefinition {
        kind: "state-only".to_owned(),
        target: None,
        match_event: Rc::new(|event| {
            Ok(
                (event.event_type == "event").then(|| ConversationMatchResult {
                    id: "one".to_owned(),
                    role: ConversationMatchRole::Start,
                }),
            )
        }),
        start: Rc::new(|_context, _accepted, _reader| Ok(Some(Rc::new(Value::Null)))),
        update: Rc::new(|context, _accepted| Ok(context.state.clone())),
        publication: None,
        build_location_data: None,
        build_view_node: None,
    });
    let starts = Rc::new(Cell::new(0));
    let mut state_claim = assembler(
        vec![state_only],
        Some(fallback_definition(starts.clone())),
        Rc::new(Cell::new(0)),
    );
    state_claim
        .replace_window(&[at(1, "event", json!({}))], false)
        .unwrap();
    state_claim.flush().unwrap();
    assert_eq!(starts.get(), 1);
    assert_eq!(snapshot(&state_claim)["order"].as_array().unwrap().len(), 1);

    let mut other_target = basic_definition(
        "other",
        |event| {
            Ok(
                (event.event_type == "event").then(|| ConversationMatchResult {
                    id: "one".to_owned(),
                    role: ConversationMatchRole::Start,
                }),
            )
        },
        |_context, _accepted, _reader| Ok(Some(Rc::new(Value::Null))),
        |context, _accepted| Ok(context.state.clone()),
    );
    other_target.target = Some("trajectory".to_owned());
    other_target.build_view_node = Some(Rc::new(|_context| Ok(None)));
    let starts = Rc::new(Cell::new(0));
    let mut other_claim = assembler(
        vec![Rc::new(other_target)],
        Some(fallback_definition(starts.clone())),
        Rc::new(Cell::new(0)),
    );
    other_claim
        .replace_window(&[at(1, "event", json!({}))], false)
        .unwrap();
    assert_eq!(starts.get(), 1);

    let mut same_target = basic_definition(
        "same",
        |event| {
            Ok(
                (event.event_type == "event").then(|| ConversationMatchResult {
                    id: "one".to_owned(),
                    role: ConversationMatchRole::Start,
                }),
            )
        },
        |_context, _accepted, _reader| Ok(Some(Rc::new(Value::Null))),
        |context, _accepted| Ok(context.state.clone()),
    );
    same_target.build_view_node = Some(Rc::new(|_context| Ok(None)));
    let starts = Rc::new(Cell::new(0));
    let mut same_claim = assembler(
        vec![Rc::new(same_target)],
        Some(fallback_definition(starts.clone())),
        Rc::new(Cell::new(0)),
    );
    same_claim
        .replace_window(&[at(1, "event", json!({}))], false)
        .unwrap();
    assert_eq!(starts.get(), 0);
}

#[test]
fn publication_uses_the_highest_claimed_cadence_and_context_keys_use_utf16_length() {
    let cadence_definition = |kind: &str, cadence| {
        let mut definition = basic_definition(
            kind,
            |event| {
                Ok(
                    (event.event_type == "pulse").then(|| ConversationMatchResult {
                        id: "one".to_owned(),
                        role: ConversationMatchRole::Start,
                    }),
                )
            },
            |_context, _accepted, _reader| Ok(Some(Rc::new(Value::Null))),
            |context, _accepted| Ok(context.state.clone()),
        );
        definition.target = None;
        definition.publication = Some(Rc::new(move |_accepted| Ok(cadence)));
        Rc::new(definition)
    };
    let mut assembler = assembler(
        vec![
            cadence_definition("none", ConversationPublication::None),
            cadence_definition("frame", ConversationPublication::AnimationFrame),
        ],
        None,
        Rc::new(Cell::new(0)),
    );
    assembler.replace_window(&[], false).unwrap();
    assert_eq!(
        assembler.append(&at(1, "pulse", json!({}))).unwrap(),
        ConversationPublication::AnimationFrame
    );
    assert_eq!(conversation_context_key("😀x", "id"), "3:😀xid");
}

#[test]
fn withdrawing_nodes_undefined_state_and_duplicate_starts_fail_before_corrupting_views() {
    let mut toggle = basic_definition(
        "toggle",
        |event| {
            Ok(match event.event_type.as_str() {
                "toggle/start" => Some(ConversationMatchResult {
                    id: "one".to_owned(),
                    role: ConversationMatchRole::Start,
                }),
                "toggle/hide" => Some(ConversationMatchResult {
                    id: "one".to_owned(),
                    role: ConversationMatchRole::Update,
                }),
                _ => None,
            })
        },
        |_context, _accepted, _reader| Ok(Some(Rc::new(json!(true)))),
        |_context, _accepted| Ok(Some(Rc::new(json!(false)))),
    );
    toggle.build_view_node = Some(Rc::new(|context| {
        Ok((context.state.as_deref() == Some(&json!(true))).then(|| node(context, json!(true))))
    }));
    let mut withdrawal = assembler(vec![Rc::new(toggle)], None, Rc::new(Cell::new(0)));
    withdrawal
        .replace_window(&[at(1, "toggle/start", json!({}))], false)
        .unwrap();
    withdrawal.flush().unwrap();
    withdrawal.append(&at(2, "toggle/hide", json!({}))).unwrap();
    assert!(
        withdrawal
            .flush()
            .unwrap_err()
            .to_string()
            .contains("withdrew materialized target \"chat\"")
    );
    assert_eq!(snapshot(&withdrawal)["order"].as_array().unwrap().len(), 1);

    let undefined_start = basic_definition(
        "undefined-start",
        |event| {
            Ok(
                (event.event_type == "start").then(|| ConversationMatchResult {
                    id: "one".to_owned(),
                    role: ConversationMatchRole::Start,
                }),
            )
        },
        |_context, _accepted, _reader| Ok(None),
        |context, _accepted| Ok(context.state.clone()),
    );
    let mut invalid = assembler(vec![Rc::new(undefined_start)], None, Rc::new(Cell::new(0)));
    assert!(
        invalid
            .replace_window(&[at(1, "start", json!({}))], false)
            .unwrap_err()
            .to_string()
            .contains("Definition \"undefined-start\" returned undefined from start()")
    );

    let mut duplicate = basic_definition(
        "single-start",
        |event| {
            Ok(
                (event.event_type == "start").then(|| ConversationMatchResult {
                    id: "one".to_owned(),
                    role: ConversationMatchRole::Start,
                }),
            )
        },
        |_context, accepted, _reader| Ok(Some(Rc::new(json!(accepted.event.seq)))),
        |context, _accepted| Ok(context.state.clone()),
    );
    duplicate.build_view_node = Some(Rc::new(|context| {
        Ok(Some(node(
            context,
            context.state.as_deref().cloned().unwrap(),
        )))
    }));
    let mut duplicate = assembler(vec![Rc::new(duplicate)], None, Rc::new(Cell::new(0)));
    duplicate
        .replace_window(&[at(1, "start", json!({}))], false)
        .unwrap();
    duplicate.flush().unwrap();
    assert!(
        duplicate
            .append(&at(2, "start", json!({})))
            .unwrap_err()
            .to_string()
            .contains("received more than one start Match")
    );
    duplicate.flush().unwrap();
    assert_eq!(
        node_data(&duplicate, &conversation_context_key("single-start", "one")),
        1
    );
}

#[test]
fn undefined_update_unstable_nodes_and_invalid_location_data_fail_loud() {
    let mut undefined = basic_definition(
        "undefined-update",
        |event| {
            Ok(match event.event_type.as_str() {
                "start" => Some(ConversationMatchResult {
                    id: "one".to_owned(),
                    role: ConversationMatchRole::Start,
                }),
                "update" => Some(ConversationMatchResult {
                    id: "one".to_owned(),
                    role: ConversationMatchRole::Update,
                }),
                _ => None,
            })
        },
        |_context, _accepted, _reader| Ok(Some(Rc::new(json!(true)))),
        |_context, _accepted| Ok(None),
    );
    undefined.build_view_node = Some(Rc::new(|context| Ok(Some(node(context, json!(true))))));
    let mut undefined = assembler(vec![Rc::new(undefined)], None, Rc::new(Cell::new(0)));
    undefined
        .replace_window(&[at(1, "start", json!({}))], false)
        .unwrap();
    assert!(
        undefined
            .append(&at(2, "update", json!({})))
            .unwrap_err()
            .to_string()
            .contains("returned undefined from update()")
    );

    let mut unstable = basic_definition(
        "unstable",
        |event| {
            Ok(
                (event.event_type == "start").then(|| ConversationMatchResult {
                    id: "one".to_owned(),
                    role: ConversationMatchRole::Start,
                }),
            )
        },
        |_context, _accepted, _reader| Ok(Some(Rc::new(Value::Null))),
        |context, _accepted| Ok(context.state.clone()),
    );
    unstable.build_view_node = Some(Rc::new(|context| {
        Ok(Some(Rc::new(ConversationViewNode {
            key: "wrong".to_owned(),
            kind: context.kind.clone(),
            id: context.id.clone(),
            target: "chat".to_owned(),
            data: Rc::new(Value::Null),
        })))
    }));
    let mut unstable = assembler(vec![Rc::new(unstable)], None, Rc::new(Cell::new(0)));
    unstable
        .replace_window(&[at(1, "start", json!({}))], false)
        .unwrap();
    assert!(
        unstable
            .flush()
            .unwrap_err()
            .to_string()
            .contains("returned unstable key \"wrong\"")
    );

    let mut invalid_data = basic_definition(
        "owned",
        |event| {
            Ok(
                (event.event_type == "start").then(|| ConversationMatchResult {
                    id: "one".to_owned(),
                    role: ConversationMatchRole::Start,
                }),
            )
        },
        |_context, _accepted, _reader| Ok(Some(Rc::new(Value::Null))),
        |context, _accepted| Ok(context.state.clone()),
    );
    invalid_data.build_location_data = Some(Rc::new(|_context, scope| {
        Ok((scope == ConversationLocationDataScope::Step).then(|| {
            Rc::new(ConversationLocationData::Turn {
                turn: 1,
                key: "owned".to_owned(),
                value: Rc::new(Value::Null),
            })
        }))
    }));
    let mut invalid_data = assembler(vec![Rc::new(invalid_data)], None, Rc::new(Cell::new(0)));
    invalid_data
        .replace_window(&[at(1, "start", json!({}))], false)
        .unwrap();
    assert!(
        invalid_data
            .flush()
            .unwrap_err()
            .to_string()
            .contains("published turn data through its step scope")
    );
}

#[test]
fn turn_location_membership_changes_rebuild_existing_turn_nodes() {
    let mut definition = basic_definition(
        "turn-probe",
        |event| {
            Ok(
                (event.event_type == "turn/start").then(|| ConversationMatchResult {
                    id: event.data["turn"].to_string(),
                    role: ConversationMatchRole::Start,
                }),
            )
        },
        |_context, _accepted, _reader| Ok(Some(Rc::new(Value::Null))),
        |context, _accepted| Ok(context.state.clone()),
    );
    definition.build_view_node = Some(Rc::new(|context| {
        let count = context.start.as_ref().map_or(-1, |accepted| {
            if let ConversationLocation::Turn { turn } = &accepted.location {
                i64::try_from(turn.steps.len()).unwrap()
            } else {
                -1
            }
        });
        Ok(Some(node(context, json!(count))))
    }));
    let apply_calls = Rc::new(Cell::new(0));
    let mut assembler = assembler(vec![Rc::new(definition)], None, apply_calls.clone());
    assembler
        .replace_window(&[at(1, "turn/start", json!({"turn":1}))], false)
        .unwrap();
    assembler.flush().unwrap();
    let key = conversation_context_key("turn-probe", "1");
    assert_eq!(node_data(&assembler, &key), 0);
    assembler
        .append(&at(2, "step/start", json!({"turn":1,"step":1})))
        .unwrap();
    assembler.flush().unwrap();
    assert_eq!(apply_calls.get(), 1);
    assert_eq!(node_data(&assembler, &key), 1);
}
