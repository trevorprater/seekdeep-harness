//! Browser registry façades backed by native Rust Conversation Definitions.

use std::{cell::RefCell, rc::Rc};

use indexmap::IndexMap;
use js_sys::{Array, Function, Map, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};

use crate::{
    AssemblerNodeDefinition, AssemblerViewBuilder, AssemblerViewDefinition,
    ChatConversationViewMetadata, ConversationAssemblerError, ConversationBoundaryStatus,
    ConversationContextReader, ConversationLocation, ConversationLocationData,
    ConversationLocationDataScope, ConversationLocationDataStore, ConversationMatch,
    ConversationMatchResult, ConversationMatchRole, ConversationNodeContext,
    ConversationPreviousContext, ConversationPublication, ConversationTimelineSnapshot,
    ConversationViewNode, ConversationViewPlacement, ConversationVisibility, StepLocation,
    TurnLocation,
    wasm_conversation_adapter::view_node_to_js,
    wasm_session::{js_to_json, json_to_js, parse_event},
};

/// Wraps one native Rust Event Definition in the browser registry object contract.
///
/// # Errors
///
/// Returns JavaScript object-construction failures.
#[allow(clippy::too_many_lines)]
pub fn native_conversation_node_definition_to_js(
    definition: AssemblerNodeDefinition,
) -> Result<JsValue, JsValue> {
    let definition = Rc::new(definition);
    let value = Object::new();
    set(&value, "kind", &JsValue::from_str(&definition.kind))?;
    set(
        &value,
        "target",
        &definition
            .target
            .as_ref()
            .map_or(JsValue::UNDEFINED, |target| JsValue::from_str(target)),
    )?;

    let matcher = definition.clone();
    let match_event = Closure::wrap(Box::new(move |event: JsValue| -> Result<JsValue, JsValue> {
        let event = parse_event(&event)?;
        (matcher.match_event)(&event)
            .map_err(assembler_error)?
            .map_or(Ok(JsValue::NULL), |result| match_result_to_js(&result))
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    set(&value, "match", &match_event.into_js_value())?;

    let starter = definition.clone();
    let start = Closure::wrap(Box::new(
        move |context: JsValue, accepted: JsValue, reader: JsValue| -> Result<JsValue, JsValue> {
            let context = context_from_js(&context)?;
            let accepted = Rc::new(match_from_js(&accepted)?);
            let mut reader = BrowserContextReader::new(reader);
            let state =
                (starter.start)(&context, &accepted, &mut reader).map_err(assembler_error)?;
            if let Some(error) = reader.error.take() {
                return Err(assembler_error(error));
            }
            optional_json_to_js(state.as_deref())
        },
    )
        as Box<dyn FnMut(JsValue, JsValue, JsValue) -> Result<JsValue, JsValue>>);
    set(&value, "start", &start.into_js_value())?;

    let updater = definition.clone();
    let update = Closure::wrap(Box::new(
        move |context: JsValue, accepted: JsValue| -> Result<JsValue, JsValue> {
            let context = context_from_js(&context)?;
            let accepted = Rc::new(match_from_js(&accepted)?);
            (updater.update)(&context, &accepted)
                .map_err(assembler_error)
                .and_then(|state| optional_json_to_js(state.as_deref()))
        },
    )
        as Box<dyn FnMut(JsValue, JsValue) -> Result<JsValue, JsValue>>);
    set(&value, "update", &update.into_js_value())?;

    if let Some(publication) = &definition.publication {
        let publication = publication.clone();
        let callback = Closure::wrap(
            Box::new(move |accepted: JsValue| -> Result<String, JsValue> {
                publication(&match_from_js(&accepted)?)
                    .map(publication_name)
                    .map(str::to_owned)
                    .map_err(assembler_error)
            }) as Box<dyn FnMut(JsValue) -> Result<String, JsValue>>,
        );
        set(&value, "publication", &callback.into_js_value())?;
    }

    if let Some(builder) = &definition.build_location_data {
        let builder = builder.clone();
        let callback = Closure::wrap(Box::new(
            move |context: JsValue, scope: String| -> Result<JsValue, JsValue> {
                let context = context_from_js(&context)?;
                let scope = match scope.as_str() {
                    "step" => ConversationLocationDataScope::Step,
                    "turn" => ConversationLocationDataScope::Turn,
                    _ => {
                        return Err(js_sys::Error::new(&format!(
                            "Conversation Location data scope {scope:?} is invalid"
                        ))
                        .into());
                    }
                };
                builder(&context, scope)
                    .map_err(assembler_error)?
                    .map_or(Ok(JsValue::NULL), |data| location_data_to_js(&data))
            },
        )
            as Box<dyn FnMut(JsValue, String) -> Result<JsValue, JsValue>>);
        set(&value, "buildLocationData", &callback.into_js_value())?;
    }

    if let Some(builder) = &definition.build_view_node {
        let builder = builder.clone();
        let callback = Closure::wrap(
            Box::new(move |context: JsValue| -> Result<JsValue, JsValue> {
                let context = context_from_js(&context)?;
                builder(&context)
                    .map_err(assembler_error)?
                    .map_or(Ok(JsValue::NULL), |node| view_node_to_js(&node))
            }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        );
        set(&value, "buildViewNode", &callback.into_js_value())?;
    }
    Ok(value.into())
}

fn location_data_to_js(data: &ConversationLocationData) -> Result<JsValue, JsValue> {
    match data {
        ConversationLocationData::Turn { turn, key, value } => object(&[
            ("kind", JsValue::from_str("turn")),
            ("turn", JsValue::from_f64(u64_as_f64(*turn))),
            ("key", JsValue::from_str(key)),
            ("value", json_to_js(value)?),
        ]),
        ConversationLocationData::Step {
            turn,
            step,
            key,
            value,
        } => object(&[
            ("kind", JsValue::from_str("step")),
            ("turn", JsValue::from_f64(u64_as_f64(*turn))),
            (
                "step",
                step.map_or(JsValue::UNDEFINED, |step| {
                    JsValue::from_f64(u64_as_f64(step))
                }),
            ),
            ("key", JsValue::from_str(key)),
            ("value", json_to_js(value)?),
        ]),
    }
    .map(Into::into)
}

/// Wraps one native Rust View Definition in the browser registry object contract.
///
/// # Errors
///
/// Returns JavaScript object-construction failures.
pub fn native_conversation_view_definition_to_js(
    definition: AssemblerViewDefinition,
) -> Result<JsValue, JsValue> {
    let value = Object::new();
    set(&value, "target", &JsValue::from_str(&definition.target))?;
    let create_native = definition.create;
    let create = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        browser_view_builder((create_native)())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&value, "create", &create.into_js_value())?;
    Ok(value.into())
}

fn browser_view_builder(builder: Box<dyn AssemblerViewBuilder>) -> Result<JsValue, JsValue> {
    let builder = Rc::new(RefCell::new(builder));
    let value = Object::new();
    let empty = builder.borrow().empty();
    set(&value, "empty", &json_to_js(&empty)?)?;

    let replace_builder = builder.clone();
    let replace = Closure::wrap(Box::new(move |input: JsValue| -> Result<JsValue, JsValue> {
        let nodes = view_nodes_from_input(&input, "nodes")?;
        let timeline = timeline_from_input(&input)?;
        replace_builder
            .borrow_mut()
            .replace(&nodes, timeline)
            .map_err(assembler_error)
            .and_then(|snapshot| json_to_js(&snapshot))
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    set(&value, "replace", &replace.into_js_value())?;

    let apply_builder = builder;
    let apply = Closure::wrap(Box::new(move |input: JsValue| -> Result<JsValue, JsValue> {
        let nodes = view_nodes_from_input(&input, "upserts")?;
        let timeline = timeline_from_input(&input)?;
        apply_builder
            .borrow_mut()
            .apply(&nodes, timeline)
            .map_err(assembler_error)
            .and_then(|snapshot| json_to_js(&snapshot))
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    set(&value, "apply", &apply.into_js_value())?;
    Ok(value.into())
}

fn view_nodes_from_input(
    input: &JsValue,
    key: &str,
) -> Result<Vec<Rc<ConversationViewNode>>, JsValue> {
    Array::from(&required(input, key, "Conversation view builder input")?)
        .iter()
        .map(|node| view_node_from_js(&node).map(Rc::new))
        .collect()
}

fn timeline_from_input(input: &JsValue) -> Result<Rc<ConversationTimelineSnapshot>, JsValue> {
    let value = required(input, "timeline", "Conversation view builder input")?;
    let order = Array::from(&required(&value, "turnOrder", "Conversation timeline")?)
        .iter()
        .map(|turn| js_safe_u64(&turn, "Conversation timeline turn"))
        .collect::<Result<Vec<_>, _>>()?;
    let turns_value = required(&value, "turns", "Conversation timeline")?;
    let turns_map = turns_value.dyn_into::<Map>()?;
    let mut turns = IndexMap::new();
    for turn in &order {
        let value = turns_map.get(&JsValue::from_f64(u64_as_f64(*turn)));
        if value.is_undefined() {
            return Err(
                js_sys::Error::new(&format!("Conversation timeline omitted Turn {turn}")).into(),
            );
        }
        turns.insert(*turn, turn_from_js(&value)?);
    }
    Ok(Rc::new(ConversationTimelineSnapshot {
        turn_order: Rc::new(order),
        turns: Rc::new(turns),
    }))
}

fn match_result_to_js(result: &ConversationMatchResult) -> Result<JsValue, JsValue> {
    object(&[
        ("id", JsValue::from_str(&result.id)),
        (
            "role",
            JsValue::from_str(match result.role {
                ConversationMatchRole::Start => "start",
                ConversationMatchRole::Update => "update",
            }),
        ),
    ])
    .map(Into::into)
}

fn context_from_js(value: &JsValue) -> Result<ConversationNodeContext, JsValue> {
    let matches = Array::from(&required(value, "matches", "Conversation Context")?)
        .iter()
        .map(|accepted| match_from_js(&accepted).map(Rc::new))
        .collect::<Result<Vec<_>, _>>()?;
    let start = optional(value, "start")?
        .filter(|accepted| !accepted.is_null())
        .map(|accepted| match_from_js(&accepted).map(Rc::new))
        .transpose()?;
    let state = optional(value, "state")?
        .filter(|state| !state.is_null())
        .map(|state| js_to_json(&state).map(Rc::new))
        .transpose()?;
    let current_value = required(value, "current", "Conversation Context")?;
    let mut current = IndexMap::new();
    let iterator = js_sys::try_iter(&current_value)?
        .ok_or_else(|| js_sys::Error::new("Conversation Context current must be iterable"))?;
    for entry in iterator {
        let pair = Array::from(&entry?);
        let target = pair
            .get(0)
            .as_string()
            .ok_or_else(|| js_sys::Error::new("Conversation Context target must be a string"))?;
        let node = pair.get(1);
        current.insert(
            target,
            (!node.is_null())
                .then(|| view_node_from_js(&node).map(Rc::new))
                .transpose()?,
        );
    }
    Ok(ConversationNodeContext {
        key: required_string(value, "key", "Conversation Context")?,
        kind: required_string(value, "kind", "Conversation Context")?,
        id: required_string(value, "id", "Conversation Context")?,
        matches: Rc::new(RefCell::new(matches)),
        start,
        state,
        current: Rc::new(RefCell::new(current)),
    })
}

fn match_from_js(value: &JsValue) -> Result<ConversationMatch, JsValue> {
    let role = match required_string(value, "role", "Conversation match")?.as_str() {
        "start" => ConversationMatchRole::Start,
        "update" => ConversationMatchRole::Update,
        role => {
            return Err(js_sys::Error::new(&format!(
                "Conversation match role {role:?} is invalid"
            ))
            .into());
        }
    };
    Ok(ConversationMatch {
        event: parse_event(&required(value, "event", "Conversation match")?)?,
        view: optional(value, "view")?
            .filter(|view| !view.is_null())
            .map(|view| js_to_json(&view).map(Rc::new))
            .transpose()?,
        role,
        location: location_from_js(&required(value, "location", "Conversation match")?)?,
    })
}

fn location_from_js(value: &JsValue) -> Result<ConversationLocation, JsValue> {
    match required_string(value, "kind", "Conversation location")?.as_str() {
        "session" => Ok(ConversationLocation::Session),
        "unresolved" => Ok(ConversationLocation::Unresolved),
        "turn" => Ok(ConversationLocation::Turn {
            turn: turn_from_js(&required(value, "turn", "Conversation location")?)?,
        }),
        "step" => Ok(ConversationLocation::Step {
            turn: turn_from_js(&required(value, "turn", "Conversation location")?)?,
            step: step_from_js(&required(value, "step", "Conversation location")?)?,
        }),
        kind => Err(
            js_sys::Error::new(&format!("Conversation location kind {kind:?} is invalid")).into(),
        ),
    }
}

fn turn_from_js(value: &JsValue) -> Result<Rc<TurnLocation>, JsValue> {
    let steps = optional(value, "steps")?
        .map(|steps| {
            Array::from(&steps)
                .iter()
                .map(|step| step_from_js(&step))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    Ok(Rc::new(TurnLocation {
        turn: required_u64(value, "turn", "Turn location")?,
        start: optional_event(value, "start")?,
        end: optional_event(value, "end")?,
        status: boundary_status(value)?,
        steps: Rc::new(steps),
        data: data_store_from_js(value)?,
    }))
}

fn step_from_js(value: &JsValue) -> Result<Rc<StepLocation>, JsValue> {
    Ok(Rc::new(StepLocation {
        turn: required_u64(value, "turn", "Step location")?,
        step: required_u64(value, "step", "Step location")?,
        start: optional_event(value, "start")?,
        end: optional_event(value, "end")?,
        status: boundary_status(value)?,
        data: data_store_from_js(value)?,
    }))
}

fn data_store_from_js(value: &JsValue) -> Result<Rc<ConversationLocationDataStore>, JsValue> {
    let Some(data) = optional(value, "data")? else {
        return Ok(Rc::new(ConversationLocationDataStore::default()));
    };
    let entries = Reflect::get(&data, &JsValue::from_str("entries"))?;
    if entries.is_undefined() || entries.is_null() {
        return Ok(Rc::new(ConversationLocationDataStore::default()));
    }
    let iterator = js_sys::try_iter(&entries)?
        .ok_or_else(|| js_sys::Error::new("Conversation Location data entries must be iterable"))?;
    let mut values = IndexMap::new();
    for entry in iterator {
        let pair = Array::from(&entry?);
        let key = pair
            .get(0)
            .as_string()
            .ok_or_else(|| js_sys::Error::new("Conversation Location data key must be a string"))?;
        values.insert(key, Rc::new(js_to_json(&pair.get(1))?));
    }
    Ok(Rc::new(ConversationLocationDataStore::from_values(values)))
}

fn boundary_status(value: &JsValue) -> Result<ConversationBoundaryStatus, JsValue> {
    match optional(value, "status")?
        .and_then(|status| status.as_string())
        .as_deref()
    {
        Some("open") => Ok(ConversationBoundaryStatus::Open),
        Some("closed") => Ok(ConversationBoundaryStatus::Closed),
        Some("unknown") | None => Ok(ConversationBoundaryStatus::Unknown),
        Some(status) => Err(js_sys::Error::new(&format!(
            "Conversation boundary status {status:?} is invalid"
        ))
        .into()),
    }
}

fn optional_event(
    value: &JsValue,
    key: &str,
) -> Result<Option<Rc<crate::ConversationLocationEvent>>, JsValue> {
    optional(value, key)?
        .filter(|event| !event.is_null())
        .map(|event| parse_event(&event))
        .transpose()
}

fn view_node_from_js(value: &JsValue) -> Result<ConversationViewNode, JsValue> {
    let target = required_string(value, "target", "Conversation view Node")?;
    let anchor = optional(value, "anchorSeq")?.and_then(|anchor| anchor.as_f64());
    let location = optional(value, "location")?;
    let placement = if target == "chat" {
        None
    } else {
        anchor
            .zip(location.as_ref())
            .map(|(anchor_seq, location)| {
                Ok::<_, JsValue>(ConversationViewPlacement {
                    anchor_seq,
                    location: location_from_js(location)?,
                })
            })
            .transpose()?
    };
    let chat = if target == "chat" {
        Some(ChatConversationViewMetadata {
            anchor_seq: anchor.ok_or_else(|| {
                js_sys::Error::new("Conversation Chat view Node omitted anchorSeq")
            })?,
            location: location_from_js(&location.ok_or_else(|| {
                js_sys::Error::new("Conversation Chat view Node omitted location")
            })?)?,
            visibility: match required_string(value, "visibility", "Conversation Chat view Node")?
                .as_str()
            {
                "visible" => ConversationVisibility::Visible,
                "hidden" => ConversationVisibility::Hidden,
                visibility => {
                    return Err(js_sys::Error::new(&format!(
                        "Conversation visibility {visibility:?} is invalid"
                    ))
                    .into());
                }
            },
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
        placement,
        chat,
    })
}

struct BrowserContextReader {
    reader: JsValue,
    error: Option<ConversationAssemblerError>,
}

impl BrowserContextReader {
    fn new(reader: JsValue) -> Self {
        Self {
            reader,
            error: None,
        }
    }

    fn read(&mut self, kind: &str) -> Option<ConversationPreviousContext> {
        match call_method(&self.reader, "previous", &[JsValue::from_str(kind)]).and_then(|value| {
            if value.is_undefined() || value.is_null() {
                Ok(None)
            } else {
                previous_context_from_js(&value).map(Some)
            }
        }) {
            Ok(previous) => previous,
            Err(error) => {
                self.error = Some(ConversationAssemblerError::new(js_error_text(&error)));
                None
            }
        }
    }
}

impl ConversationContextReader for BrowserContextReader {
    fn peek_previous(&mut self, kind: &str) -> Option<ConversationPreviousContext> {
        self.read(kind)
    }

    fn previous(&mut self, kind: &str) -> Option<ConversationPreviousContext> {
        self.read(kind)
    }
}

fn previous_context_from_js(value: &JsValue) -> Result<ConversationPreviousContext, JsValue> {
    let matches = Array::from(&required(
        value,
        "matches",
        "previous Conversation Context",
    )?)
    .iter()
    .map(|accepted| match_from_js(&accepted).map(Rc::new))
    .collect::<Result<Vec<_>, _>>()?;
    Ok(ConversationPreviousContext {
        key: required_string(value, "key", "previous Conversation Context")?,
        kind: required_string(value, "kind", "previous Conversation Context")?,
        id: required_string(value, "id", "previous Conversation Context")?,
        start_seq: required_u64(value, "startSeq", "previous Conversation Context")?,
        state: Rc::new(js_to_json(&required(
            value,
            "state",
            "previous Conversation Context",
        )?)?),
        matches: Rc::new(RefCell::new(matches)),
    })
}

fn optional_json_to_js(value: Option<&serde_json::Value>) -> Result<JsValue, JsValue> {
    value.map_or(Ok(JsValue::UNDEFINED), json_to_js)
}

fn publication_name(publication: ConversationPublication) -> &'static str {
    match publication {
        ConversationPublication::None => "none",
        ConversationPublication::AnimationFrame => "animation-frame",
        ConversationPublication::Immediate => "immediate",
    }
}

fn required_u64(value: &JsValue, key: &str, owner: &str) -> Result<u64, JsValue> {
    let value = required(value, key, owner)?;
    js_safe_u64(&value, &format!("{owner} {key:?}"))
}

fn js_safe_u64(value: &JsValue, owner: &str) -> Result<u64, JsValue> {
    let number = value
        .as_f64()
        .filter(|number| number.is_finite() && *number >= 0.0 && number.fract() == 0.0)
        .ok_or_else(|| js_sys::Error::new(&format!("{owner} must be a u64")))?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(number as u64)
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::Error::new(&format!("{owner} {key:?} must be a string")).into())
}

fn required(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let value = Reflect::get(value, &JsValue::from_str(key))?;
    if value.is_undefined() {
        Err(js_sys::Error::new(&format!("{owner} omitted {key:?}")).into())
    } else {
        Ok(value)
    }
}

fn optional(value: &JsValue, key: &str) -> Result<Option<JsValue>, JsValue> {
    let value = Reflect::get(value, &JsValue::from_str(key))?;
    Ok((!value.is_undefined()).then_some(value))
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let value = Object::new();
    for (key, entry) in entries {
        set(&value, key, entry)?;
    }
    Ok(value)
}

fn set(value: &Object, key: &str, entry: &JsValue) -> Result<(), JsValue> {
    Reflect::set(value, &JsValue::from_str(key), entry).map(|_| ())
}

fn u64_as_f64(value: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}

#[allow(clippy::needless_pass_by_value)] // `Result::map_err` owns the error at this ABI seam.
fn assembler_error(error: ConversationAssemblerError) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}

fn js_error_text(value: &JsValue) -> String {
    Reflect::get(value, &JsValue::from_str("message"))
        .ok()
        .and_then(|message| message.as_string())
        .or_else(|| value.as_string())
        .unwrap_or_else(|| format!("{value:?}"))
}
