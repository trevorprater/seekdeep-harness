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
    wasm_conversation_adapter::{
        recover_event, recover_match, recover_node, recover_step, recover_store, recover_turn,
        recover_value, view_node_to_js,
    },
    wasm_session::{js_to_json, json_to_js, parse_event},
};

/// Parsed engine values keyed by the identity of the face they were parsed from.
///
/// A Definition compiled into another module cannot share the engine's `Rc` values, so it
/// parses the faces it is handed. The adapter hands out one stable face per engine value, so
/// each face parses once here and later callbacks reuse the parse; a bounded FIFO keeps the
/// parsed copies from outliving the conversation they mirror.
struct ParsedFaces<T> {
    ids: js_sys::WeakMap,
    values: IndexMap<u32, Rc<T>>,
    next: u32,
}

impl<T> ParsedFaces<T> {
    const CAPACITY: usize = 4096;

    fn new() -> Self {
        Self {
            ids: js_sys::WeakMap::new(),
            values: IndexMap::new(),
            next: 0,
        }
    }

    fn get_or_parse(
        &mut self,
        face: &JsValue,
        parse: impl FnOnce() -> Result<Rc<T>, JsValue>,
    ) -> Result<Rc<T>, JsValue> {
        if !face.is_object() {
            return parse();
        }
        let key: &Object = face.unchecked_ref();
        if let Some(id) = self.ids.get(key).as_f64().and_then(parsed_face_id)
            && let Some(value) = self.values.get(&id)
        {
            return Ok(value.clone());
        }
        let value = parse()?;
        while self.values.len() >= Self::CAPACITY {
            self.values.shift_remove_index(0);
        }
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        self.values.insert(id, value.clone());
        self.ids.set(key, &JsValue::from_f64(f64::from(id)));
        Ok(value)
    }
}

thread_local! {
    static PARSED_TURNS: RefCell<ParsedFaces<TurnLocation>> = RefCell::new(ParsedFaces::new());
    static PARSED_STEPS: RefCell<ParsedFaces<StepLocation>> = RefCell::new(ParsedFaces::new());
    static PARSED_STORES: RefCell<ParsedFaces<ConversationLocationDataStore>> = RefCell::new(ParsedFaces::new());
    static PARSED_EVENTS: RefCell<ParsedFaces<crate::ConversationLocationEvent>> = RefCell::new(ParsedFaces::new());
    static PARSED_MATCHES: RefCell<ParsedFaces<ConversationMatch>> = RefCell::new(ParsedFaces::new());
    static PARSED_NODES: RefCell<ParsedFaces<ConversationViewNode>> = RefCell::new(ParsedFaces::new());
    static PARSED_VALUES: RefCell<ParsedFaces<serde_json::Value>> = RefCell::new(ParsedFaces::new());
}

/// The immutable JSON behind a face, or a fresh parse of a foreign object.
fn json_value_from_js(value: &JsValue) -> Result<Rc<serde_json::Value>, JsValue> {
    if let Some(recovered) = recover_value(value) {
        return Ok(recovered);
    }
    PARSED_VALUES.with(|cache| {
        cache
            .borrow_mut()
            .get_or_parse(value, || js_to_json(value).map(Rc::new))
    })
}

/// The engine event behind a face, or a fresh parse of a foreign object.
fn event_from_js(value: &JsValue) -> Result<Rc<crate::ConversationLocationEvent>, JsValue> {
    if let Some(recovered) = recover_event(value) {
        return Ok(recovered);
    }
    PARSED_EVENTS.with(|cache| {
        cache
            .borrow_mut()
            .get_or_parse(value, || parse_event(value))
    })
}

// Native codec metadata must not reserve string properties on arbitrary Client View Builders.
pub(crate) const VIEW_SNAPSHOT_CODEC: &str =
    "@seekdeep-ai/seekdeep-client-runtime/native-view-snapshot-codec";

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
            let accepted = match_from_js(&accepted)?;
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
            let accepted = match_from_js(&accepted)?;
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
                publication(&*match_from_js(&accepted)?)
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
    native_view_definition(definition, None)
}

/// Target-owned conversion between native JSON storage and the public browser snapshot.
#[derive(Clone, Copy)]
pub struct NativeConversationViewSnapshotCodec {
    /// Restores browser-only types without mutating the encoded input.
    pub to_browser: fn(&JsValue) -> Result<JsValue, JsValue>,
    /// Encodes browser-only types without mutating the public snapshot.
    pub to_native: fn(&JsValue) -> Result<JsValue, JsValue>,
}

/// Wraps a native View Definition with its target-owned browser snapshot codec.
///
/// Builder outputs and public Session reads expose browser types such as Maps; the
/// native assembler stores the codec's JSON representation without knowing target fields.
///
/// # Errors
///
/// Returns object-construction failures. Codec errors propagate through the affected operation.
pub fn native_conversation_view_definition_to_js_with_codec(
    definition: AssemblerViewDefinition,
    codec: NativeConversationViewSnapshotCodec,
) -> Result<JsValue, JsValue> {
    native_view_definition(definition, Some(codec))
}

fn native_view_definition(
    definition: AssemblerViewDefinition,
    codec: Option<NativeConversationViewSnapshotCodec>,
) -> Result<JsValue, JsValue> {
    let value = Object::new();
    set(&value, "target", &JsValue::from_str(&definition.target))?;
    let create_native = definition.create;
    let create = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        browser_view_builder((create_native)(), codec)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&value, "create", &create.into_js_value())?;
    Ok(value.into())
}

fn browser_view_builder(
    builder: Box<dyn AssemblerViewBuilder>,
    codec: Option<NativeConversationViewSnapshotCodec>,
) -> Result<JsValue, JsValue> {
    let builder = Rc::new(RefCell::new(builder));
    let value = Object::new();
    if let Some(codec) = codec {
        let metadata = Object::new();
        for (name, convert) in [
            ("toBrowserSnapshot", codec.to_browser),
            ("toNativeSnapshot", codec.to_native),
        ] {
            let callback = Closure::wrap(Box::new(move |snapshot: JsValue| convert(&snapshot))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
            set(&metadata, name, &callback.into_js_value())?;
        }
        Reflect::set(
            &value,
            &js_sys::Symbol::for_(VIEW_SNAPSHOT_CODEC),
            &metadata,
        )?;
    }
    let empty = builder.borrow().empty();
    let encode = move |snapshot: &serde_json::Value| {
        let encoded = json_to_js(snapshot)?;
        codec.map_or(Ok(encoded.clone()), |codec| (codec.to_browser)(&encoded))
    };
    set(&value, "empty", &encode(&empty)?)?;

    let replace_builder = builder.clone();
    let replace = Closure::wrap(Box::new(move |input: JsValue| -> Result<JsValue, JsValue> {
        let nodes = view_nodes_from_input(&input, "nodes")?;
        let timeline = timeline_from_input(&input)?;
        replace_builder
            .borrow_mut()
            .replace(&nodes, timeline)
            .map_err(assembler_error)
            .and_then(|snapshot| encode(&snapshot))
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
            .and_then(|snapshot| encode(&snapshot))
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
        .map(|node| view_node_from_js(&node))
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
        .map(|accepted| match_from_js(&accepted))
        .collect::<Result<Vec<_>, _>>()?;
    let start = optional(value, "start")?
        .filter(|accepted| !accepted.is_null())
        .map(|accepted| match_from_js(&accepted))
        .transpose()?;
    let state = optional(value, "state")?
        .filter(|state| !state.is_null())
        .map(|state| json_value_from_js(&state))
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
                .then(|| view_node_from_js(&node))
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

fn match_from_js(value: &JsValue) -> Result<Rc<ConversationMatch>, JsValue> {
    if let Some(accepted) = recover_match(value) {
        return Ok(accepted);
    }
    PARSED_MATCHES.with(|cache| {
        cache
            .borrow_mut()
            .get_or_parse(value, || parse_match(value))
    })
}

fn parse_match(value: &JsValue) -> Result<Rc<ConversationMatch>, JsValue> {
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
    Ok(Rc::new(ConversationMatch {
        event: event_from_js(&required(value, "event", "Conversation match")?)?,
        view: optional(value, "view")?
            .filter(|view| !view.is_null())
            .map(|view| json_value_from_js(&view))
            .transpose()?,
        role,
        location: location_from_js(&required(value, "location", "Conversation match")?)?,
    }))
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
    if let Some(turn) = recover_turn(value) {
        return Ok(turn);
    }
    PARSED_TURNS.with(|cache| cache.borrow_mut().get_or_parse(value, || parse_turn(value)))
}

fn parse_turn(value: &JsValue) -> Result<Rc<TurnLocation>, JsValue> {
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
    if let Some(step) = recover_step(value) {
        return Ok(step);
    }
    PARSED_STEPS.with(|cache| cache.borrow_mut().get_or_parse(value, || parse_step(value)))
}

fn parse_step(value: &JsValue) -> Result<Rc<StepLocation>, JsValue> {
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
    if let Some(store) = recover_store(&data) {
        return Ok(store);
    }
    PARSED_STORES.with(|cache| {
        cache
            .borrow_mut()
            .get_or_parse(&data, || parse_data_store(&data))
    })
}

fn parse_data_store(data: &JsValue) -> Result<Rc<ConversationLocationDataStore>, JsValue> {
    // The source contract is the `get(key)` reader: read the live store behind the face on
    // demand rather than copying whatever it holds at parse time.
    if let Ok(get) = Reflect::get(data, &JsValue::from_str("get"))
        && let Ok(get) = get.dyn_into::<Function>()
    {
        let owner = data.clone();
        let reader: crate::conversation_location::RemoteLocationDataReader =
            Rc::new(move |key: &str| {
                let value = get.call1(&owner, &JsValue::from_str(key)).ok()?;
                if value.is_undefined() || value.is_null() {
                    return None;
                }
                json_value_from_js(&value).ok()
            });
        return Ok(Rc::new(ConversationLocationDataStore::from_reader(reader)));
    }
    let entries = Reflect::get(data, &JsValue::from_str("entries"))?;
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
        .map(|event| event_from_js(&event))
        .transpose()
}

fn view_node_from_js(value: &JsValue) -> Result<Rc<ConversationViewNode>, JsValue> {
    if let Some(node) = recover_node(value) {
        return Ok(node);
    }
    PARSED_NODES.with(|cache| {
        cache
            .borrow_mut()
            .get_or_parse(value, || parse_view_node(value))
    })
}

fn parse_view_node(value: &JsValue) -> Result<Rc<ConversationViewNode>, JsValue> {
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
    Ok(Rc::new(ConversationViewNode {
        key: required_string(value, "key", "Conversation view Node")?,
        kind: required_string(value, "kind", "Conversation view Node")?,
        id: required_string(value, "id", "Conversation view Node")?,
        target,
        data: json_value_from_js(&required(value, "data", "Conversation view Node")?)?,
        placement,
        chat,
    }))
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
    .map(|accepted| match_from_js(&accepted))
    .collect::<Result<Vec<_>, _>>()?;
    Ok(ConversationPreviousContext {
        key: required_string(value, "key", "previous Conversation Context")?,
        kind: required_string(value, "kind", "previous Conversation Context")?,
        id: required_string(value, "id", "previous Conversation Context")?,
        start_seq: required_u64(value, "startSeq", "previous Conversation Context")?,
        state: json_value_from_js(&required(value, "state", "previous Conversation Context")?)?,
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

/// Decodes a parsed-face id written by the cache; anything else is not one of its ids.
fn parsed_face_id(value: f64) -> Option<u32> {
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 || value > f64::from(u32::MAX) {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(value as u32)
}
