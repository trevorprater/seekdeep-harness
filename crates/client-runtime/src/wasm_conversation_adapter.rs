//! JavaScript Definition adapters for the Rust-owned Conversation assembler.

use std::{cell::RefCell, rc::Rc};

use indexmap::IndexSet;
use js_sys::{Array, Function, Map, Object, Reflect};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};

use crate::{
    AssemblerEventDefinitions, AssemblerNodeDefinition, AssemblerViewBuilder,
    AssemblerViewDefinition, AssemblerViewDefinitions, ConversationAssemblerError,
    ConversationBoundaryStatus, ConversationContextReader, ConversationEventRegistry,
    ConversationLocation, ConversationLocationData, ConversationLocationDataScope,
    ConversationLocationDataStore, ConversationLocationEvent, ConversationMatch,
    ConversationMatchResult, ConversationMatchRole, ConversationNodeContext,
    ConversationNodeDefinition, ConversationPreviousContext, ConversationPublication,
    ConversationTimelineSnapshot, ConversationViewNode, ConversationViewRegistry, StepLocation,
    TurnLocation, wasm_session::js_to_json, wasm_session::json_to_js, wasm_session::render_js,
};

type BrowserNode = ConversationNodeDefinition<JsValue>;
type BrowserView = crate::ConversationViewDefinition<JsValue>;

pub(crate) fn browser_event_definitions(
    registry: Rc<ConversationEventRegistry<JsValue>>,
) -> Rc<dyn AssemblerEventDefinitions> {
    Rc::new(BrowserEventDefinitions {
        registry,
        cache: RefCell::new(Vec::new()),
    })
}

pub(crate) fn browser_view_definitions(
    registry: Rc<ConversationViewRegistry<JsValue>>,
) -> Rc<dyn AssemblerViewDefinitions> {
    Rc::new(BrowserViewDefinitions {
        registry,
        cache: RefCell::new(Vec::new()),
    })
}

struct BrowserEventDefinitions {
    registry: Rc<ConversationEventRegistry<JsValue>>,
    cache: RefCell<Vec<(Rc<BrowserNode>, Rc<AssemblerNodeDefinition>)>>,
}

impl BrowserEventDefinitions {
    fn adapt(&self, definition: &Rc<BrowserNode>) -> Rc<AssemblerNodeDefinition> {
        if let Some((_, adapted)) = self
            .cache
            .borrow()
            .iter()
            .find(|(known, _)| Rc::ptr_eq(known, definition))
        {
            return adapted.clone();
        }
        let adapted = adapt_node_definition(definition, self.registry.clone());
        self.cache
            .borrow_mut()
            .push((definition.clone(), adapted.clone()));
        adapted
    }
}

impl AssemblerEventDefinitions for BrowserEventDefinitions {
    fn entries(&self) -> Vec<Rc<AssemblerNodeDefinition>> {
        let entries = self.registry.entries();
        self.cache
            .borrow_mut()
            .retain(|(known, _)| entries.iter().any(|entry| Rc::ptr_eq(known, entry)));
        entries.iter().map(|entry| self.adapt(entry)).collect()
    }

    fn fallback_entry(&self) -> Option<Rc<AssemblerNodeDefinition>> {
        self.registry.fallback().map(|entry| self.adapt(&entry))
    }
}

struct BrowserViewDefinitions {
    registry: Rc<ConversationViewRegistry<JsValue>>,
    cache: RefCell<Vec<(Rc<BrowserView>, Rc<AssemblerViewDefinition>)>>,
}

impl AssemblerViewDefinitions for BrowserViewDefinitions {
    fn entries(&self) -> Vec<Rc<AssemblerViewDefinition>> {
        let entries = self.registry.entries();
        let mut cache = self.cache.borrow_mut();
        cache.retain(|(known, _)| entries.iter().any(|entry| Rc::ptr_eq(known, entry)));
        entries
            .iter()
            .map(|definition| {
                if let Some((_, adapted)) = cache
                    .iter()
                    .find(|(known, _)| Rc::ptr_eq(known, definition))
                {
                    return adapted.clone();
                }
                let adapted = adapt_view_definition(definition);
                cache.push((definition.clone(), adapted.clone()));
                adapted
            })
            .collect()
    }
}

#[allow(clippy::too_many_lines)]
fn adapt_node_definition(
    definition: &ConversationNodeDefinition<JsValue>,
    registry: Rc<ConversationEventRegistry<JsValue>>,
) -> Rc<AssemblerNodeDefinition> {
    let payload = definition.payload.clone();
    let match_payload = payload.clone();
    let match_event = Rc::new(move |event: &ConversationLocationEvent| {
        let result = call_method(
            &match_payload,
            "match",
            &[event_to_js(event).map_err(adapter_error)?],
        )
        .map_err(adapter_error)?;
        if result.is_null() {
            return Ok(None);
        }
        let id = required_string(&result, "id", "Conversation match").map_err(adapter_error)?;
        let role = match required_string(&result, "role", "Conversation match")
            .map_err(adapter_error)?
            .as_str()
        {
            "start" => ConversationMatchRole::Start,
            "update" => ConversationMatchRole::Update,
            role => {
                return Err(ConversationAssemblerError::new(format!(
                    "unknown Conversation match role {role:?}"
                )));
            }
        };
        Ok(Some(ConversationMatchResult { id, role }))
    });

    let start_payload = payload.clone();
    let start_registry = registry;
    let start = Rc::new(
        move |context: &ConversationNodeContext,
              accepted: &Rc<ConversationMatch>,
              reader: &mut dyn ConversationContextReader| {
            let kinds = start_registry
                .entries()
                .iter()
                .map(|entry| entry.kind.clone())
                .chain(start_registry.fallback().map(|entry| entry.kind.clone()))
                .collect::<IndexSet<_>>();
            let previous = Map::new();
            for kind in &kinds {
                if let Some(context) = reader.peek_previous(kind) {
                    previous.set(
                        &JsValue::from_str(kind),
                        &previous_context_to_js(&context).map_err(adapter_error)?,
                    );
                }
            }
            let requested = Rc::new(RefCell::new(IndexSet::<String>::new()));
            let requested_by_js = requested.clone();
            let previous_by_js = previous;
            let previous_fn = Closure::wrap(Box::new(move |kind: String| {
                requested_by_js.borrow_mut().insert(kind.clone());
                previous_by_js.get(&JsValue::from_str(&kind))
            }) as Box<dyn FnMut(String) -> JsValue>);
            let reader_face = Object::new();
            set(&reader_face, "previous", &previous_fn.into_js_value()).map_err(adapter_error)?;
            let result = call_method(
                &start_payload,
                "start",
                &[
                    context_to_js(context).map_err(adapter_error)?,
                    match_to_js(accepted).map_err(adapter_error)?,
                    reader_face.into(),
                ],
            )
            .map_err(adapter_error)?;
            for kind in requested.borrow().iter() {
                let _ = reader.previous(kind);
            }
            optional_json(&result)
        },
    );

    let update_payload = payload.clone();
    let update = Rc::new(
        move |context: &ConversationNodeContext, accepted: &Rc<ConversationMatch>| {
            let result = call_method(
                &update_payload,
                "update",
                &[
                    context_to_js(context).map_err(adapter_error)?,
                    match_to_js(accepted).map_err(adapter_error)?,
                ],
            )
            .map_err(adapter_error)?;
            optional_json(&result)
        },
    );

    let publication = optional_function(&payload, "publication").map(|function| {
        let payload = payload.clone();
        Rc::new(move |accepted: &ConversationMatch| {
            let result = function
                .call1(&payload, &match_to_js(accepted).map_err(adapter_error)?)
                .map_err(adapter_error)?;
            match result.as_string().as_deref() {
                Some("none") => Ok(ConversationPublication::None),
                Some("animation-frame") => Ok(ConversationPublication::AnimationFrame),
                Some("immediate") => Ok(ConversationPublication::Immediate),
                _ => Err(ConversationAssemblerError::new(
                    "Conversation publication must be none, animation-frame, or immediate",
                )),
            }
        }) as Rc<_>
    });

    let build_location_data = optional_function(&payload, "buildLocationData").map(|function| {
        let payload = payload.clone();
        Rc::new(
            move |context: &ConversationNodeContext, scope: ConversationLocationDataScope| {
                let result = function
                    .call2(
                        &payload,
                        &context_to_js(context).map_err(adapter_error)?,
                        &JsValue::from_str(match scope {
                            ConversationLocationDataScope::Step => "step",
                            ConversationLocationDataScope::Turn => "turn",
                        }),
                    )
                    .map_err(adapter_error)?;
                if result.is_null() {
                    return Ok(None);
                }
                location_data_from_js(&result)
                    .map(Some)
                    .map_err(adapter_error)
            },
        ) as Rc<_>
    });

    let build_view_node = optional_function(&payload, "buildViewNode").map(|function| {
        let payload = payload.clone();
        Rc::new(move |context: &ConversationNodeContext| {
            let result = function
                .call1(&payload, &context_to_js(context).map_err(adapter_error)?)
                .map_err(adapter_error)?;
            if result.is_null() {
                return Ok(None);
            }
            view_node_from_js(&result, context)
                .map(|node| Some(Rc::new(node)))
                .map_err(adapter_error)
        }) as Rc<_>
    });

    Rc::new(AssemblerNodeDefinition {
        kind: definition.kind.clone(),
        target: definition.target.clone(),
        match_event,
        start,
        update,
        publication,
        build_location_data,
        build_view_node,
    })
}

fn adapt_view_definition(definition: &BrowserView) -> Rc<AssemblerViewDefinition> {
    let payload = definition.payload.clone();
    Rc::new(AssemblerViewDefinition {
        target: definition.target.clone(),
        create: Rc::new(move || {
            let builder = call_method(&payload, "create", &[])
                .unwrap_or_else(|error| wasm_bindgen::throw_val(error));
            let empty = required(&builder, "empty", "Conversation view builder")
                .and_then(|value| js_to_json(&value))
                .map_or_else(|error| wasm_bindgen::throw_val(error), Rc::new);
            Box::new(BrowserViewBuilder { builder, empty })
        }),
    })
}

struct BrowserViewBuilder {
    builder: JsValue,
    empty: Rc<serde_json::Value>,
}

impl AssemblerViewBuilder for BrowserViewBuilder {
    fn empty(&self) -> Rc<serde_json::Value> {
        self.empty.clone()
    }

    fn replace(
        &mut self,
        nodes: &[Rc<ConversationViewNode>],
        timeline: Rc<ConversationTimelineSnapshot>,
    ) -> Result<Rc<serde_json::Value>, ConversationAssemblerError> {
        self.call("replace", nodes, &timeline)
    }

    fn apply(
        &mut self,
        upserts: &[Rc<ConversationViewNode>],
        timeline: Rc<ConversationTimelineSnapshot>,
    ) -> Result<Rc<serde_json::Value>, ConversationAssemblerError> {
        self.call("apply", upserts, &timeline)
    }
}

impl BrowserViewBuilder {
    fn call(
        &self,
        method: &str,
        nodes: &[Rc<ConversationViewNode>],
        timeline: &ConversationTimelineSnapshot,
    ) -> Result<Rc<serde_json::Value>, ConversationAssemblerError> {
        let input = Object::new();
        let nodes_array = Array::new();
        for node in nodes {
            nodes_array.push(&view_node_to_js(node).map_err(adapter_error)?);
        }
        set(
            &input,
            if method == "replace" {
                "nodes"
            } else {
                "upserts"
            },
            &nodes_array,
        )
        .map_err(adapter_error)?;
        set(
            &input,
            "timeline",
            &timeline_to_js(timeline).map_err(adapter_error)?,
        )
        .map_err(adapter_error)?;
        let result = call_method(&self.builder, method, &[input.into()]).map_err(adapter_error)?;
        js_to_json(&result).map(Rc::new).map_err(adapter_error)
    }
}

fn event_to_js(event: &ConversationLocationEvent) -> Result<JsValue, JsValue> {
    json_to_js(&event.wire_value())
}

fn match_to_js(accepted: &ConversationMatch) -> Result<JsValue, JsValue> {
    let value = Object::new();
    set(&value, "event", &event_to_js(&accepted.event)?)?;
    set(
        &value,
        "view",
        &accepted
            .view
            .as_ref()
            .map(|view| json_to_js(view))
            .transpose()?
            .unwrap_or(JsValue::UNDEFINED),
    )?;
    set(
        &value,
        "role",
        &JsValue::from_str(match accepted.role {
            ConversationMatchRole::Start => "start",
            ConversationMatchRole::Update => "update",
        }),
    )?;
    set(&value, "location", &location_to_js(&accepted.location)?)?;
    Ok(value.into())
}

fn context_to_js(context: &ConversationNodeContext) -> Result<JsValue, JsValue> {
    let value = Object::new();
    set(&value, "key", &JsValue::from_str(&context.key))?;
    set(&value, "kind", &JsValue::from_str(&context.kind))?;
    set(&value, "id", &JsValue::from_str(&context.id))?;
    let matches = Array::new();
    for accepted in context.matches.borrow().iter() {
        matches.push(&match_to_js(accepted)?);
    }
    set(&value, "matches", &matches)?;
    set(
        &value,
        "start",
        &context
            .start
            .as_ref()
            .map(|accepted| match_to_js(accepted))
            .transpose()?
            .unwrap_or(JsValue::UNDEFINED),
    )?;
    set(
        &value,
        "state",
        &context
            .state
            .as_ref()
            .map(|state| json_to_js(state))
            .transpose()?
            .unwrap_or(JsValue::UNDEFINED),
    )?;
    let current = Map::new();
    for (target, node) in context.current.borrow().iter() {
        current.set(
            &JsValue::from_str(target),
            &node
                .as_ref()
                .map(|node| view_node_to_js(node))
                .transpose()?
                .unwrap_or(JsValue::NULL),
        );
    }
    set(&value, "current", &current)?;
    Ok(value.into())
}

fn previous_context_to_js(context: &ConversationPreviousContext) -> Result<JsValue, JsValue> {
    let value = Object::new();
    set(&value, "key", &JsValue::from_str(&context.key))?;
    set(&value, "kind", &JsValue::from_str(&context.kind))?;
    set(&value, "id", &JsValue::from_str(&context.id))?;
    set(&value, "startSeq", &js_number(context.start_seq))?;
    set(&value, "state", &json_to_js(&context.state)?)?;
    let matches = Array::new();
    for accepted in context.matches.borrow().iter() {
        matches.push(&match_to_js(accepted)?);
    }
    set(&value, "matches", &matches)?;
    Ok(value.into())
}

pub(crate) fn location_to_js(location: &ConversationLocation) -> Result<JsValue, JsValue> {
    let value = Object::new();
    match location {
        ConversationLocation::Session => set(&value, "kind", &JsValue::from_str("session"))?,
        ConversationLocation::Turn { turn } => {
            set(&value, "kind", &JsValue::from_str("turn"))?;
            set(&value, "turn", &turn_to_js(turn)?)?;
        }
        ConversationLocation::Step { turn, step } => {
            set(&value, "kind", &JsValue::from_str("step"))?;
            set(&value, "turn", &turn_to_js(turn)?)?;
            set(&value, "step", &step_to_js(step)?)?;
        }
        ConversationLocation::Unresolved => {
            set(&value, "kind", &JsValue::from_str("unresolved"))?;
        }
    }
    Ok(value.into())
}

fn turn_to_js(turn: &TurnLocation) -> Result<JsValue, JsValue> {
    let value = Object::new();
    set(&value, "turn", &js_number(turn.turn))?;
    set_optional_event(&value, "start", turn.start.as_deref())?;
    set_optional_event(&value, "end", turn.end.as_deref())?;
    set(
        &value,
        "status",
        &JsValue::from_str(status_name(turn.status)),
    )?;
    let steps = Array::new();
    for step in turn.steps.iter() {
        steps.push(&step_to_js(step)?);
    }
    set(&value, "steps", &steps)?;
    set(&value, "data", &data_store_to_js(turn.data.clone())?)?;
    Ok(value.into())
}

fn step_to_js(step: &StepLocation) -> Result<JsValue, JsValue> {
    let value = Object::new();
    set(&value, "turn", &js_number(step.turn))?;
    set(&value, "step", &js_number(step.step))?;
    set_optional_event(&value, "start", step.start.as_deref())?;
    set_optional_event(&value, "end", step.end.as_deref())?;
    set(
        &value,
        "status",
        &JsValue::from_str(status_name(step.status)),
    )?;
    set(&value, "data", &data_store_to_js(step.data.clone())?)?;
    Ok(value.into())
}

fn data_store_to_js(store: Rc<ConversationLocationDataStore>) -> Result<JsValue, JsValue> {
    let value = Object::new();
    let get = Closure::wrap(Box::new(move |key: String| -> Result<JsValue, JsValue> {
        store
            .get(&key)
            .map(|value| json_to_js(&value))
            .transpose()
            .map(|value| value.unwrap_or(JsValue::UNDEFINED))
    }) as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>);
    set(&value, "get", &get.into_js_value())?;
    Ok(value.into())
}

pub(crate) fn timeline_to_js(timeline: &ConversationTimelineSnapshot) -> Result<JsValue, JsValue> {
    let value = Object::new();
    let order = Array::new();
    for turn in timeline.turn_order.iter() {
        order.push(&js_number(*turn));
    }
    set(&value, "turnOrder", &order)?;
    let turns = Map::new();
    for (number, turn) in timeline.turns.iter() {
        turns.set(&js_number(*number), &turn_to_js(turn)?);
    }
    set(&value, "turns", &turns)?;
    Ok(value.into())
}

fn view_node_to_js(node: &ConversationViewNode) -> Result<JsValue, JsValue> {
    let value = Object::new();
    set(&value, "key", &JsValue::from_str(&node.key))?;
    set(&value, "kind", &JsValue::from_str(&node.kind))?;
    set(&value, "id", &JsValue::from_str(&node.id))?;
    set(&value, "target", &JsValue::from_str(&node.target))?;
    if let Some(chat) = &node.chat {
        set(&value, "anchorSeq", &JsValue::from_f64(chat.anchor_seq))?;
        set(&value, "location", &location_to_js(&chat.location)?)?;
        set(
            &value,
            "visibility",
            &JsValue::from_str(match chat.visibility {
                crate::ConversationVisibility::Visible => "visible",
                crate::ConversationVisibility::Hidden => "hidden",
            }),
        )?;
    }
    set(&value, "data", &json_to_js(&node.data)?)?;
    Ok(value.into())
}

fn view_node_from_js(
    value: &JsValue,
    context: &ConversationNodeContext,
) -> Result<ConversationViewNode, JsValue> {
    let target = required_string(value, "target", "Conversation view Node")?;
    let chat = if target == "chat" {
        let anchor_seq = required(value, "anchorSeq", "Conversation Chat view Node")?
            .as_f64()
            .ok_or_else(|| {
                js_sys::Error::new("Conversation Chat view Node anchorSeq must be a number")
            })?;
        if !anchor_seq.is_finite() {
            return Err(
                js_sys::Error::new("Conversation Chat view Node anchorSeq must be finite").into(),
            );
        }
        let location = required(value, "location", "Conversation Chat view Node")?;
        let visibility =
            match required_string(value, "visibility", "Conversation Chat view Node")?.as_str() {
                "visible" => crate::ConversationVisibility::Visible,
                "hidden" => crate::ConversationVisibility::Hidden,
                visibility => {
                    return Err(js_sys::Error::new(&format!(
                        "Conversation Chat view Node visibility {visibility:?} is invalid"
                    ))
                    .into());
                }
            };
        Some(crate::ChatConversationViewMetadata {
            anchor_seq,
            location: context_location_from_js(&location, context)?,
            visibility,
        })
    } else {
        None
    };
    Ok(ConversationViewNode {
        key: required_string(value, "key", "Conversation view Node")?,
        kind: required_string(value, "kind", "Conversation view Node")?,
        id: required_string(value, "id", "Conversation view Node")?,
        target,
        data: Rc::new(js_to_json(&required(
            value,
            "data",
            "Conversation view Node",
        )?)?),
        chat,
    })
}

fn context_location_from_js(
    value: &JsValue,
    context: &ConversationNodeContext,
) -> Result<ConversationLocation, JsValue> {
    let kind = required_string(value, "kind", "Conversation Chat view Node location")?;
    match kind.as_str() {
        "session" => Ok(ConversationLocation::Session),
        "unresolved" => Ok(ConversationLocation::Unresolved),
        "turn" => {
            let turn = required_u64(
                &required(value, "turn", "Conversation Chat view Node location")?,
                "turn",
                "Conversation Chat view Node location turn",
            )?;
            context
                .matches
                .borrow()
                .iter()
                .find_map(|accepted| match &accepted.location {
                    ConversationLocation::Turn { turn: known }
                    | ConversationLocation::Step { turn: known, .. }
                        if known.turn == turn =>
                    {
                        Some(ConversationLocation::Turn {
                            turn: known.clone(),
                        })
                    }
                    _ => None,
                })
                .ok_or_else(|| {
                    js_sys::Error::new(&format!(
                        "Conversation Chat view Node location references unknown Turn {turn}"
                    ))
                    .into()
                })
        }
        "step" => {
            let turn = required_u64(
                &required(value, "turn", "Conversation Chat view Node location")?,
                "turn",
                "Conversation Chat view Node location turn",
            )?;
            let step = required_u64(
                &required(value, "step", "Conversation Chat view Node location")?,
                "step",
                "Conversation Chat view Node location step",
            )?;
            context
                .matches
                .borrow()
                .iter()
                .find_map(|accepted| match &accepted.location {
                    ConversationLocation::Step {
                        turn: known_turn,
                        step: known_step,
                    } if known_turn.turn == turn && known_step.step == step => {
                        Some(ConversationLocation::Step {
                            turn: known_turn.clone(),
                            step: known_step.clone(),
                        })
                    }
                    _ => None,
                })
                .ok_or_else(|| {
                    js_sys::Error::new(&format!(
                        "Conversation Chat view Node location references unknown Step {turn}:{step}"
                    ))
                    .into()
                })
        }
        kind => Err(js_sys::Error::new(&format!(
            "Conversation Chat view Node location kind {kind:?} is invalid"
        ))
        .into()),
    }
}

pub(crate) fn location_data_from_js(
    value: &JsValue,
) -> Result<Rc<ConversationLocationData>, JsValue> {
    let kind = required_string(value, "kind", "Conversation Location data")?;
    let turn = required_u64(value, "turn", "Conversation Location data")?;
    let key = required_string(value, "key", "Conversation Location data")?;
    let carried = Rc::new(js_to_json(&required(
        value,
        "value",
        "Conversation Location data",
    )?)?);
    match kind.as_str() {
        "turn" => Ok(Rc::new(ConversationLocationData::Turn {
            turn,
            key,
            value: carried,
        })),
        "step" => Ok(Rc::new(ConversationLocationData::Step {
            turn,
            step: Some(required_u64(value, "step", "Conversation Location data")?),
            key,
            value: carried,
        })),
        _ => Err(js_sys::Error::new("Conversation Location data kind must be turn or step").into()),
    }
}

fn optional_json(
    value: &JsValue,
) -> Result<Option<Rc<serde_json::Value>>, ConversationAssemblerError> {
    if value.is_undefined() {
        return Ok(None);
    }
    js_to_json(value)
        .map(Rc::new)
        .map(Some)
        .map_err(adapter_error)
}

fn set_optional_event(
    object: &Object,
    key: &str,
    event: Option<&ConversationLocationEvent>,
) -> Result<(), JsValue> {
    set(
        object,
        key,
        &event
            .map(event_to_js)
            .transpose()?
            .unwrap_or(JsValue::UNDEFINED),
    )
}

fn status_name(status: ConversationBoundaryStatus) -> &'static str {
    match status {
        ConversationBoundaryStatus::Open => "open",
        ConversationBoundaryStatus::Closed => "closed",
        ConversationBoundaryStatus::Unknown => "unknown",
    }
}

fn optional_function(value: &JsValue, key: &str) -> Option<Function> {
    let member = Reflect::get(value, &JsValue::from_str(key)).ok()?;
    if member.is_undefined() {
        return None;
    }
    Some(member.dyn_into::<Function>().unwrap_or_else(|_| {
        Function::new_no_args(&format!(
            "throw new TypeError('Conversation Definition {key} must be a function')"
        ))
    }))
}

fn call_method(value: &JsValue, method: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let function = required(value, method, "Conversation Definition")?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    function.apply(value, &args)
}

fn required(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let member = Reflect::get(value, &JsValue::from_str(key))?;
    if member.is_undefined() || member.is_null() {
        Err(js_sys::Error::new(&format!("{owner} requires {key:?}")).into())
    } else {
        Ok(member)
    }
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::Error::new(&format!("{owner} {key} must be a string")).into())
}

fn required_u64(value: &JsValue, key: &str, owner: &str) -> Result<u64, JsValue> {
    let number = required(value, key, owner)?
        .as_f64()
        .filter(|number| number.is_finite() && number.fract() == 0.0 && *number >= 0.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    number.map(|number| number as u64).ok_or_else(|| {
        js_sys::Error::new(&format!("{owner} {key} must be a non-negative integer")).into()
    })
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(object, &JsValue::from_str(key), value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new(&format!("failed to set Conversation member {key:?}")).into())
    }
}

#[allow(clippy::needless_pass_by_value)]
fn adapter_error(error: JsValue) -> ConversationAssemblerError {
    ConversationAssemblerError::new(render_js(&error))
}

fn js_number(value: u64) -> JsValue {
    #[allow(clippy::cast_precision_loss)]
    {
        JsValue::from_f64(value as f64)
    }
}
