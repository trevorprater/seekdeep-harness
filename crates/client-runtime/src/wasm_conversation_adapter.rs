//! JavaScript Definition adapters for the Rust-owned Conversation assembler.

use std::{
    cell::RefCell,
    collections::HashMap,
    rc::{Rc, Weak},
};

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
    ConversationTimelineSnapshot, ConversationViewNode, ConversationViewRegistry, PrematchTable,
    StepLocation, TurnLocation,
    wasm_session::json_to_js,
    wasm_session::render_js,
    wasm_value_bridge::{js_to_value, js_to_value_reusing, value_to_js, value_to_js_reusing},
};

type BrowserNode = ConversationNodeDefinition<JsValue>;
type BrowserView = crate::ConversationViewDefinition<JsValue>;

/// Identity-keyed JavaScript faces for the engine's shared `Rc` values.
///
/// The source hands Definitions the live Location, Match, and Node objects, so a streaming Turn
/// costs one object per value for its whole life. The Rust engine shares those values by `Rc`
/// identity (an unchanged Turn or Step keeps its `Rc` across appends), so each value converts to
/// JavaScript once and every later callback receives the same face. A `Weak` pins the allocation,
/// which keeps the pointer key unambiguous until the entry is pruned.
struct FaceCache<T> {
    entries: HashMap<usize, (Weak<T>, JsValue)>,
    /// Face object back to its entry key, so a native Definition recovers the engine value
    /// instead of re-parsing the face it was handed.
    faces: js_sys::WeakMap,
    /// Entry count at which the next liveness prune runs; doubles after a prune that kept
    /// most entries alive, so a long-lived list is not rescanned on every insert.
    prune_at: usize,
}

impl<T> FaceCache<T> {
    const PRUNE_ABOVE: usize = 4096;

    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            faces: js_sys::WeakMap::new(),
            prune_at: Self::PRUNE_ABOVE,
        }
    }

    fn get_or_build(
        &mut self,
        value: &Rc<T>,
        build: impl FnOnce() -> Result<JsValue, JsValue>,
    ) -> Result<JsValue, JsValue> {
        let key = Rc::as_ptr(value) as usize;
        if let Some((weak, face)) = self.entries.get(&key)
            && weak.upgrade().is_some_and(|live| Rc::ptr_eq(&live, value))
        {
            return Ok(face.clone());
        }
        let face = build()?;
        if self.entries.len() >= self.prune_at {
            self.entries.retain(|_, (weak, _)| weak.strong_count() > 0);
            self.prune_at = (self.entries.len() * 2).max(Self::PRUNE_ABOVE);
        }
        self.entries
            .insert(key, (Rc::downgrade(value), face.clone()));
        if face.is_object() {
            self.faces
                .set(&face.clone().unchecked_into(), &face_key_to_js(key)?);
        }
        Ok(face)
    }

    fn recover(&self, face: &JsValue) -> Option<Rc<T>> {
        if !face.is_object() {
            return None;
        }
        let key = face_key_from_js(self.faces.get(&face.clone().unchecked_into()).as_f64()?)?;
        self.entries.get(&key).and_then(|(weak, _)| weak.upgrade())
    }
}

/// Encodes one pointer key as a JavaScript number (wasm32 addresses fit a `u32`).
fn face_key_to_js(key: usize) -> Result<JsValue, JsValue> {
    let key = u32::try_from(key)
        .map_err(|_| js_sys::Error::new("conversation face key exceeds the JavaScript range"))?;
    Ok(JsValue::from(key))
}

/// Decodes a pointer key written by [`face_key_to_js`]; anything else is not a face key.
fn face_key_from_js(value: f64) -> Option<usize> {
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 || value > f64::from(u32::MAX) {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let key = value as u32;
    usize::try_from(key).ok()
}

/// Recovers the engine Turn behind one face the adapter handed out.
pub(crate) fn recover_turn(face: &JsValue) -> Option<Rc<TurnLocation>> {
    TURN_FACES.with(|cache| cache.borrow().recover(face))
}

/// Recovers the engine Step behind one face the adapter handed out.
pub(crate) fn recover_step(face: &JsValue) -> Option<Rc<StepLocation>> {
    STEP_FACES.with(|cache| cache.borrow().recover(face))
}

/// Recovers the engine Location data store behind one face the adapter handed out.
pub(crate) fn recover_store(face: &JsValue) -> Option<Rc<ConversationLocationDataStore>> {
    STORE_FACES.with(|cache| cache.borrow().recover(face))
}

/// Recovers the engine Location event behind one face the adapter handed out.
pub(crate) fn recover_event(face: &JsValue) -> Option<Rc<ConversationLocationEvent>> {
    EVENT_FACES.with(|cache| cache.borrow().recover(face))
}

/// Recovers the engine Match behind one face the adapter handed out.
pub(crate) fn recover_match(face: &JsValue) -> Option<Rc<ConversationMatch>> {
    MATCH_FACES.with(|cache| cache.borrow().recover(face))
}

/// Recovers the engine view Node behind one face the adapter handed out.
pub(crate) fn recover_node(face: &JsValue) -> Option<Rc<ConversationViewNode>> {
    NODE_FACES.with(|cache| cache.borrow().recover(face))
}

/// Recovers the immutable JSON value behind one face the adapter handed out.
pub(crate) fn recover_value(face: &JsValue) -> Option<Rc<serde_json::Value>> {
    VALUE_FACES.with(|cache| cache.borrow().recover(face))
}

thread_local! {
    static EVENT_FACES: RefCell<FaceCache<ConversationLocationEvent>> = RefCell::new(FaceCache::new());
    static MATCH_FACES: RefCell<FaceCache<ConversationMatch>> = RefCell::new(FaceCache::new());
    static TURN_FACES: RefCell<FaceCache<TurnLocation>> = RefCell::new(FaceCache::new());
    static STEP_FACES: RefCell<FaceCache<StepLocation>> = RefCell::new(FaceCache::new());
    static STORE_FACES: RefCell<FaceCache<ConversationLocationDataStore>> = RefCell::new(FaceCache::new());
    static NODE_FACES: RefCell<FaceCache<ConversationViewNode>> = RefCell::new(FaceCache::new());
    static VALUE_FACES: RefCell<FaceCache<serde_json::Value>> = RefCell::new(FaceCache::new());
    static MATCH_LISTS: RefCell<HashMap<usize, MatchListEntry>> = RefCell::new(HashMap::new());
    static NODE_DATA_FACES: RefCell<RetainedFaces> = RefCell::new(RetainedFaces::new());
}

/// One shared Match list pinned by `Weak` beside its append-only JavaScript Array face.
type MatchListEntry = (Weak<RefCell<Vec<Rc<ConversationMatch>>>>, Array);

fn event_face(event: &Rc<ConversationLocationEvent>) -> Result<JsValue, JsValue> {
    EVENT_FACES.with(|cache| {
        cache
            .borrow_mut()
            .get_or_build(event, || event_to_js(event))
    })
}

fn match_face(accepted: &Rc<ConversationMatch>) -> Result<JsValue, JsValue> {
    MATCH_FACES.with(|cache| {
        cache
            .borrow_mut()
            .get_or_build(accepted, || match_to_js(accepted))
    })
}

fn value_face(value: &Rc<serde_json::Value>) -> Result<JsValue, JsValue> {
    VALUE_FACES.with(|cache| {
        cache
            .borrow_mut()
            .get_or_build(value, || value_to_js(value))
    })
}

/// Faces retained by Node key, so the next data of a streaming Node reuses the previous face's
/// unchanged subtrees and grows its text by the appended suffix instead of re-marshalling.
struct RetainedFaces {
    entries: HashMap<String, (Rc<serde_json::Value>, JsValue)>,
    prune_at: usize,
}

impl RetainedFaces {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            prune_at: FaceCache::<()>::PRUNE_ABOVE,
        }
    }

    fn reuse(
        &mut self,
        key: &str,
        value: &Rc<serde_json::Value>,
        build: impl FnOnce(Option<(&serde_json::Value, &JsValue)>) -> Result<JsValue, JsValue>,
    ) -> Result<JsValue, JsValue> {
        let face = build(
            self.entries
                .get(key)
                .map(|(previous, face)| (previous.as_ref(), face)),
        )?;
        if self.entries.len() >= self.prune_at {
            // An entry whose value only this map still holds belongs to a replaced Node.
            self.entries
                .retain(|_, (value, _)| Rc::strong_count(value) > 1);
            self.prune_at = (self.entries.len() * 2).max(FaceCache::<()>::PRUNE_ABOVE);
        }
        self.entries
            .insert(key.to_owned(), (value.clone(), face.clone()));
        Ok(face)
    }
}

fn node_data_face(node: &ConversationViewNode) -> Result<JsValue, JsValue> {
    VALUE_FACES.with(|cache| {
        cache.borrow_mut().get_or_build(&node.data, || {
            NODE_DATA_FACES.with(|faces| {
                faces
                    .borrow_mut()
                    .reuse(&node.key, &node.data, |previous| match previous {
                        Some((previous, previous_js)) => {
                            value_to_js_reusing(previous, previous_js, &node.data)
                        }
                        None => value_to_js(&node.data),
                    })
            })
        })
    })
}

/// The append-only Match list of one Context as one live JavaScript array, extended in place.
fn match_list_face(matches: &Rc<RefCell<Vec<Rc<ConversationMatch>>>>) -> Result<Array, JsValue> {
    let key = Rc::as_ptr(matches) as usize;
    let list = matches.borrow();
    let array = MATCH_LISTS.with(|lists| {
        let mut lists = lists.borrow_mut();
        let existing = lists.get(&key).and_then(|(weak, array)| {
            (weak
                .upgrade()
                .is_some_and(|live| Rc::ptr_eq(&live, matches))
                && array.length() as usize <= list.len())
            .then(|| array.clone())
        });
        // A merge that inserted an earlier Match (an older history page) breaks the
        // append-only prefix: the face keeps its identity but is rebuilt from the start.
        if let Some(array) = &existing {
            let rendered = array.length() as usize;
            if rendered > 0
                && !recover_match(&array.get(u32::try_from(rendered - 1).unwrap_or(u32::MAX)))
                    .is_some_and(|last| Rc::ptr_eq(&last, &list[rendered - 1]))
            {
                array.set_length(0);
            }
        }
        existing.unwrap_or_else(|| {
            if lists.len() >= FaceCache::<()>::PRUNE_ABOVE {
                lists.retain(|_, (weak, _)| weak.strong_count() > 0);
            }
            let array = Array::new();
            lists.insert(key, (Rc::downgrade(matches), array.clone()));
            array
        })
    });
    for accepted in list.iter().skip(array.length() as usize) {
        array.push(&match_face(accepted)?);
    }
    Ok(array)
}

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

    /// Asks each Definition that exposes `matchMany` about the whole window in one call: the
    /// event faces cross once as an array and the answers come back as one JSON text, instead
    /// of a boundary crossing per event and Definition. Definitions without it (a JavaScript
    /// Definition, for one) keep the per-event matcher.
    fn prematch(
        &self,
        events: &[Rc<ConversationLocationEvent>],
    ) -> Result<Option<PrematchTable>, ConversationAssemblerError> {
        if events.len() < PREMATCH_MIN_EVENTS {
            return Ok(None);
        }
        let faces = Array::new();
        for event in events {
            faces.push(&event_face(event).map_err(adapter_error)?);
        }
        let mut table = PrematchTable::default();
        let mut covered = false;
        for node in self
            .registry
            .entries()
            .iter()
            .cloned()
            .chain(self.registry.fallback())
        {
            let batch = Reflect::get(&node.payload, &JsValue::from_str("matchMany"))
                .map_err(adapter_error)?;
            let Some(batch) = batch.dyn_ref::<Function>() else {
                continue;
            };
            let rows = batch.call1(&node.payload, &faces).map_err(adapter_error)?;
            let rows = rows.as_string().ok_or_else(|| {
                ConversationAssemblerError::new(format!(
                    "Conversation Definition {} matchMany must return JSON text",
                    node.kind
                ))
            })?;
            let rows: Vec<Option<PrematchRow>> = serde_json::from_str(&rows).map_err(|error| {
                ConversationAssemblerError::new(format!(
                    "Conversation Definition {} matchMany returned invalid rows: {error}",
                    node.kind
                ))
            })?;
            if rows.len() != events.len() {
                return Err(ConversationAssemblerError::new(format!(
                    "Conversation Definition {} matchMany answered {} of {} events",
                    node.kind,
                    rows.len(),
                    events.len()
                )));
            }
            let adapted = self.adapt(&node);
            for (event, row) in events.iter().zip(rows) {
                let answer = row.map(|row| row.into_result(&node.kind)).transpose()?;
                table.insert(&adapted, event.seq, answer);
            }
            covered = true;
        }
        Ok(covered.then_some(table))
    }
}

/// Below this many events a window replacement keeps the per-event matcher: the batch call
/// pays one array of faces and one JSON crossing per Definition.
const PREMATCH_MIN_EVENTS: usize = 256;

/// One `matchMany` answer as it crosses back from a Definition module.
#[derive(serde::Deserialize)]
struct PrematchRow {
    id: String,
    role: String,
}

impl PrematchRow {
    fn into_result(
        self,
        kind: &str,
    ) -> Result<ConversationMatchResult, ConversationAssemblerError> {
        let role = match self.role.as_str() {
            "start" => ConversationMatchRole::Start,
            "update" => ConversationMatchRole::Update,
            role => {
                return Err(ConversationAssemblerError::new(format!(
                    "Conversation Definition {kind} matchMany returned match role {role:?}"
                )));
            }
        };
        Ok(ConversationMatchResult { id: self.id, role })
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
                    match_face(accepted).map_err(adapter_error)?,
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
                    match_face(accepted).map_err(adapter_error)?,
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
                .and_then(|value| native_view_snapshot(&builder, &value))
                .and_then(|encoded| js_to_value(&encoded))
                .map_or_else(|error| wasm_bindgen::throw_val(error), Rc::new);
            Box::new(BrowserViewBuilder {
                builder,
                empty,
                previous: RefCell::new(None),
            })
        }),
    })
}

struct BrowserViewBuilder {
    builder: JsValue,
    empty: Rc<serde_json::Value>,
    /// The last snapshot and its encoded face, so the next crossing in either direction reuses
    /// unchanged subtrees and grows streamed text by its suffix.
    previous: RefCell<Option<(Rc<serde_json::Value>, JsValue)>>,
}

impl AssemblerViewBuilder for BrowserViewBuilder {
    fn snapshot_to_browser(&self, snapshot: &serde_json::Value) -> Result<JsValue, JsValue> {
        let encoded = match &*self.previous.borrow() {
            Some((previous, previous_js)) => value_to_js_reusing(previous, previous_js, snapshot)?,
            None => value_to_js(snapshot)?,
        };
        let projection = snapshot_codec_method(&self.builder, "toBrowserSnapshot")?;
        if projection.is_undefined() {
            Ok(encoded)
        } else {
            projection
                .dyn_into::<Function>()?
                .call1(&self.builder, &encoded)
        }
    }

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
        self.decode(&result).map_err(adapter_error)
    }

    fn decode(&self, snapshot: &JsValue) -> Result<Rc<serde_json::Value>, JsValue> {
        let encoded = native_view_snapshot(&self.builder, snapshot)?;
        let value = match &*self.previous.borrow() {
            Some((previous, previous_js)) => js_to_value_reusing(previous, previous_js, &encoded)?,
            None => js_to_value(&encoded)?,
        };
        let value = Rc::new(value);
        *self.previous.borrow_mut() = Some((value.clone(), encoded));
        Ok(value)
    }
}

fn native_view_snapshot(builder: &JsValue, snapshot: &JsValue) -> Result<JsValue, JsValue> {
    let codec = snapshot_codec_method(builder, "toNativeSnapshot")?;
    if codec.is_undefined() {
        Ok(snapshot.clone())
    } else {
        codec.dyn_into::<Function>()?.call1(builder, snapshot)
    }
}

fn snapshot_codec_method(builder: &JsValue, name: &str) -> Result<JsValue, JsValue> {
    let codec = Reflect::get(
        builder,
        &js_sys::Symbol::for_(crate::wasm_native_definition::VIEW_SNAPSHOT_CODEC),
    )?;
    if codec.is_undefined() {
        Ok(JsValue::UNDEFINED)
    } else {
        Reflect::get(&codec, &JsValue::from_str(name))
    }
}

fn event_to_js(event: &ConversationLocationEvent) -> Result<JsValue, JsValue> {
    json_to_js(&event.wire_value())
}

fn match_to_js(accepted: &ConversationMatch) -> Result<JsValue, JsValue> {
    let value = Object::new();
    set(&value, "event", &event_face(&accepted.event)?)?;
    set(
        &value,
        "view",
        &accepted
            .view
            .as_ref()
            .map(value_face)
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
    set(
        &value,
        "matches",
        &match_list_face(&context.matches)?.into(),
    )?;
    set(
        &value,
        "start",
        &context
            .start
            .as_ref()
            .map(match_face)
            .transpose()?
            .unwrap_or(JsValue::UNDEFINED),
    )?;
    set(
        &value,
        "state",
        &context
            .state
            .as_ref()
            .map(value_face)
            .transpose()?
            .unwrap_or(JsValue::UNDEFINED),
    )?;
    let current = Map::new();
    for (target, node) in context.current.borrow().iter() {
        current.set(
            &JsValue::from_str(target),
            &node
                .as_ref()
                .map(view_node_to_js)
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
    set(&value, "state", &value_face(&context.state)?)?;
    set(
        &value,
        "matches",
        &match_list_face(&context.matches)?.into(),
    )?;
    Ok(value.into())
}

pub(crate) fn location_to_js(location: &ConversationLocation) -> Result<JsValue, JsValue> {
    let value = Object::new();
    match location {
        ConversationLocation::Session => set(&value, "kind", &JsValue::from_str("session"))?,
        ConversationLocation::Turn { turn } => {
            set(&value, "kind", &JsValue::from_str("turn"))?;
            set(&value, "turn", &turn_face(turn)?)?;
        }
        ConversationLocation::Step { turn, step } => {
            set(&value, "kind", &JsValue::from_str("step"))?;
            set(&value, "turn", &turn_face(turn)?)?;
            set(&value, "step", &step_face(step)?)?;
        }
        ConversationLocation::Unresolved => {
            set(&value, "kind", &JsValue::from_str("unresolved"))?;
        }
    }
    Ok(value.into())
}

fn turn_face(turn: &Rc<TurnLocation>) -> Result<JsValue, JsValue> {
    TURN_FACES.with(|cache| cache.borrow_mut().get_or_build(turn, || turn_to_js(turn)))
}

fn step_face(step: &Rc<StepLocation>) -> Result<JsValue, JsValue> {
    STEP_FACES.with(|cache| cache.borrow_mut().get_or_build(step, || step_to_js(step)))
}

fn turn_to_js(turn: &TurnLocation) -> Result<JsValue, JsValue> {
    let value = Object::new();
    set(&value, "turn", &js_number(turn.turn))?;
    set_optional_event(&value, "start", turn.start.as_ref())?;
    set_optional_event(&value, "end", turn.end.as_ref())?;
    set(
        &value,
        "status",
        &JsValue::from_str(status_name(turn.status)),
    )?;
    let steps = Array::new();
    for step in turn.steps.iter() {
        steps.push(&step_face(step)?);
    }
    set(&value, "steps", &steps)?;
    set(&value, "data", &data_store_face(&turn.data)?)?;
    Ok(value.into())
}

fn step_to_js(step: &StepLocation) -> Result<JsValue, JsValue> {
    let value = Object::new();
    set(&value, "turn", &js_number(step.turn))?;
    set(&value, "step", &js_number(step.step))?;
    set_optional_event(&value, "start", step.start.as_ref())?;
    set_optional_event(&value, "end", step.end.as_ref())?;
    set(
        &value,
        "status",
        &JsValue::from_str(status_name(step.status)),
    )?;
    set(&value, "data", &data_store_face(&step.data)?)?;
    Ok(value.into())
}

/// The source contract is one stable keyed reader per Location: `get(key)` reads the latest
/// published value on demand, so the face converts nothing until a Definition asks for a key.
pub(crate) fn data_store_face(
    store: &Rc<ConversationLocationDataStore>,
) -> Result<JsValue, JsValue> {
    STORE_FACES.with(|cache| {
        cache.borrow_mut().get_or_build(store, || {
            let value = Object::new();
            let reader = store.clone();
            let get = Closure::wrap(Box::new(move |key: String| -> Result<JsValue, JsValue> {
                reader
                    .get(&key)
                    .map(|value| value_face(&value))
                    .transpose()
                    .map(|value| value.unwrap_or(JsValue::UNDEFINED))
            })
                as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>);
            set(&value, "get", &get.into_js_value())?;
            Ok(value.into())
        })
    })
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
        turns.set(&js_number(*number), &turn_face(turn)?);
    }
    set(&value, "turns", &turns)?;
    Ok(value.into())
}

pub(crate) fn view_node_to_js(node: &Rc<ConversationViewNode>) -> Result<JsValue, JsValue> {
    NODE_FACES.with(|cache| {
        cache
            .borrow_mut()
            .get_or_build(node, || view_node_value_to_js(node))
    })
}

fn view_node_value_to_js(node: &ConversationViewNode) -> Result<JsValue, JsValue> {
    let value = Object::new();
    set(&value, "key", &JsValue::from_str(&node.key))?;
    set(&value, "kind", &JsValue::from_str(&node.kind))?;
    set(&value, "id", &JsValue::from_str(&node.id))?;
    set(&value, "target", &JsValue::from_str(&node.target))?;
    if let Some(placement) = &node.placement {
        set(
            &value,
            "anchorSeq",
            &JsValue::from_f64(placement.anchor_seq),
        )?;
        set(&value, "location", &location_to_js(&placement.location)?)?;
    }
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
    set(&value, "data", &node_data_face(node)?)?;
    Ok(value.into())
}

fn view_node_from_js(
    value: &JsValue,
    context: &ConversationNodeContext,
) -> Result<ConversationViewNode, JsValue> {
    let target = required_string(value, "target", "Conversation view Node")?;
    let placement = if target == "chat" {
        None
    } else {
        let anchor = Reflect::get(value, &JsValue::from_str("anchorSeq"))?;
        let location = Reflect::get(value, &JsValue::from_str("location"))?;
        match (anchor.is_undefined(), location.is_undefined()) {
            (true, true) => None,
            (false, false) => {
                let anchor_seq = anchor.as_f64().ok_or_else(|| {
                    js_sys::Error::new("Conversation view Node anchorSeq must be a number")
                })?;
                if !anchor_seq.is_finite() {
                    return Err(js_sys::Error::new(
                        "Conversation view Node anchorSeq must be finite",
                    )
                    .into());
                }
                Some(crate::ConversationViewPlacement {
                    anchor_seq,
                    location: context_location_from_js(&location, context)?,
                })
            }
            _ => {
                return Err(js_sys::Error::new(
                    "Conversation view Node must provide anchorSeq and location together",
                )
                .into());
            }
        }
    };
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
        data: Rc::new(js_to_value(&required(
            value,
            "data",
            "Conversation view Node",
        )?)?),
        placement,
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
    let carried = Rc::new(js_to_value(&required(
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
    js_to_value(value)
        .map(Rc::new)
        .map(Some)
        .map_err(adapter_error)
}

fn set_optional_event(
    object: &Object,
    key: &str,
    event: Option<&Rc<ConversationLocationEvent>>,
) -> Result<(), JsValue> {
    set(
        object,
        key,
        &event
            .map(event_face)
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
