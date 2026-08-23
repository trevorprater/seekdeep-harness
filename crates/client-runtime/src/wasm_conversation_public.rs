//! Public browser constructors for Conversation assembly and Location indexing.

use js_sys::{Array, Reflect, Set};
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};

use crate::{
    ConversationEventInput, ConversationLocationDataChange, ConversationLocationIndex,
    ConversationNodeAssembler, ConversationOwnedLocationData, ConversationPublication,
    WasmConversationEventRegistry, WasmConversationViewRegistry,
    wasm_conversation_adapter::{
        browser_event_definitions, browser_view_definitions, location_data_from_js, location_to_js,
        timeline_to_js,
    },
    wasm_session::parse_event,
};

/// JavaScript-facing Rust Conversation assembler.
#[wasm_bindgen(js_name = ConversationNodeAssembler)]
pub struct WasmConversationNodeAssembler {
    assembler: ConversationNodeAssembler,
}

#[wasm_bindgen(js_class = ConversationNodeAssembler)]
#[allow(clippy::needless_pass_by_value)]
impl WasmConversationNodeAssembler {
    /// Creates one Session-local assembler over live registries.
    #[wasm_bindgen(constructor)]
    pub fn new(
        events: &WasmConversationEventRegistry,
        views: &WasmConversationViewRegistry,
    ) -> Self {
        Self {
            assembler: ConversationNodeAssembler::new(
                browser_event_definitions(events.core_registry()),
                browser_view_definitions(views.core_registry()),
            ),
        }
    }

    /// Replaces the complete loaded window.
    ///
    /// # Errors
    ///
    /// Returns malformed input or assembler failures.
    #[wasm_bindgen(js_name = replaceWindow)]
    pub fn replace_window(&mut self, entries: Array, has_more: bool) -> Result<String, JsValue> {
        let entries = event_inputs(&entries)?;
        self.assembler
            .replace_window(&entries, has_more)
            .map(publication_name)
            .map(str::to_owned)
            .map_err(js_error)
    }

    /// Appends one live event.
    ///
    /// # Errors
    ///
    /// Returns malformed input or assembler failures.
    pub fn append(&mut self, input: JsValue) -> Result<String, JsValue> {
        self.assembler
            .append(&event_input(&input)?)
            .map(publication_name)
            .map(str::to_owned)
            .map_err(js_error)
    }

    /// Prepends older history.
    ///
    /// # Errors
    ///
    /// Returns malformed input or assembler failures.
    pub fn prepend(&mut self, entries: Array, has_more: bool) -> Result<String, JsValue> {
        let entries = event_inputs(&entries)?;
        self.assembler
            .prepend(&entries, has_more)
            .map(publication_name)
            .map(str::to_owned)
            .map_err(js_error)
    }

    /// Rebuilds against the current low-frequency registries.
    ///
    /// # Errors
    ///
    /// Returns assembler replay failures.
    #[wasm_bindgen(js_name = rebuildRegistry)]
    pub fn rebuild_registry(&mut self) -> Result<String, JsValue> {
        self.assembler
            .rebuild_registry()
            .map(publication_name)
            .map(str::to_owned)
            .map_err(js_error)
    }

    /// Materializes dirty Contexts and target snapshots.
    ///
    /// # Errors
    ///
    /// Returns Definition, Location-data, Node, or builder failures.
    pub fn flush(&mut self) -> Result<bool, JsValue> {
        self.assembler.flush().map_err(js_error)
    }

    /// Reads one registered target snapshot.
    ///
    /// # Errors
    ///
    /// Returns JSON-to-JavaScript conversion failures.
    pub fn get(&self, target: &str) -> Result<JsValue, JsValue> {
        self.assembler
            .snapshot(target)
            .map_or(Ok(JsValue::UNDEFINED), |value| {
                crate::wasm_session::json_to_js(&value)
            })
    }
}

/// JavaScript-facing Rust Turn/Step Location index.
#[wasm_bindgen(js_name = ConversationLocationIndex)]
pub struct WasmConversationLocationIndex {
    index: ConversationLocationIndex,
}

#[wasm_bindgen(js_class = ConversationLocationIndex)]
#[allow(clippy::needless_pass_by_value)]
impl WasmConversationLocationIndex {
    /// Creates an empty Location index.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            index: ConversationLocationIndex::default(),
        }
    }

    /// Returns the current timeline snapshot.
    ///
    /// # Errors
    ///
    /// Returns JavaScript construction failures.
    pub fn snapshot(&self) -> Result<JsValue, JsValue> {
        timeline_to_js(&self.index.snapshot())
    }

    /// Replaces every Definition-owned Location value.
    ///
    /// # Errors
    ///
    /// Returns malformed data or ownership diagnostics.
    #[wasm_bindgen(js_name = replaceData)]
    pub fn replace_data(&self, entries: Array) -> Result<bool, JsValue> {
        let entries = entries
            .iter()
            .map(|entry| {
                Ok(ConversationOwnedLocationData {
                    owner: required_string(&entry, "owner")?,
                    data: location_data_from_js(&required(&entry, "data")?)?
                        .as_ref()
                        .clone(),
                })
            })
            .collect::<Result<Vec<_>, JsValue>>()?;
        self.index.replace_data(&entries).map_err(js_error)
    }

    /// Applies incremental Location-data changes.
    ///
    /// # Errors
    ///
    /// Returns malformed data or ownership diagnostics.
    #[wasm_bindgen(js_name = applyData)]
    pub fn apply_data(&self, changes: Array) -> Result<bool, JsValue> {
        let changes = changes
            .iter()
            .map(|change| {
                Ok(ConversationLocationDataChange {
                    owner: required_string(&change, "owner")?,
                    previous: optional_data(&change, "previous")?,
                    next: optional_data(&change, "next")?,
                })
            })
            .collect::<Result<Vec<_>, JsValue>>()?;
        self.index.apply_data(&changes).map_err(js_error)
    }

    /// Resolves one ingested event Location.
    ///
    /// # Errors
    ///
    /// Returns malformed event or JavaScript construction failures.
    #[wasm_bindgen(js_name = locationOf)]
    pub fn location_of(&self, event: JsValue) -> Result<JsValue, JsValue> {
        let event = parse_event(&event)?;
        location_to_js(&self.index.location_of(event.as_ref()))
    }

    /// Rebuilds complete timeline facts.
    ///
    /// # Errors
    ///
    /// Returns malformed input or boundary diagnostics.
    pub fn rebuild(&mut self, entries: Array) -> Result<Set, JsValue> {
        let entries = event_inputs(&entries)?;
        self.index
            .rebuild(&entries)
            .map(index_set_to_js)
            .map_err(js_error)
    }

    /// Appends one Turn/Step boundary.
    ///
    /// # Errors
    ///
    /// Returns malformed event or boundary diagnostics.
    #[wasm_bindgen(js_name = appendBoundary)]
    pub fn append_boundary(&mut self, event: JsValue) -> Result<Set, JsValue> {
        let event = parse_event(&event)?;
        self.index
            .append_boundary(&event)
            .map(index_set_to_js)
            .map_err(js_error)
    }

    /// Appends one non-boundary event.
    ///
    /// # Errors
    ///
    /// Returns malformed event diagnostics.
    #[wasm_bindgen(js_name = appendNonBoundary)]
    pub fn append_non_boundary(&mut self, event: JsValue) -> Result<(), JsValue> {
        let event = parse_event(&event)?;
        self.index.append_non_boundary(event.as_ref());
        Ok(())
    }
}

impl Default for WasmConversationLocationIndex {
    fn default() -> Self {
        Self::new()
    }
}

fn event_inputs(entries: &Array) -> Result<Vec<ConversationEventInput>, JsValue> {
    entries.iter().map(|entry| event_input(&entry)).collect()
}

fn event_input(value: &JsValue) -> Result<ConversationEventInput, JsValue> {
    let event = parse_event(&required(value, "event")?)?;
    let view = Reflect::get(value, &JsValue::from_str("view"))?;
    Ok(ConversationEventInput {
        event,
        view: if view.is_undefined() {
            None
        } else {
            Some(std::rc::Rc::new(crate::wasm_session::js_to_json(&view)?))
        },
    })
}

fn optional_data(
    value: &JsValue,
    key: &str,
) -> Result<Option<crate::ConversationLocationData>, JsValue> {
    let data = Reflect::get(value, &JsValue::from_str(key))?;
    if data.is_undefined() || data.is_null() {
        return Ok(None);
    }
    Ok(Some(location_data_from_js(&data)?.as_ref().clone()))
}

fn publication_name(publication: ConversationPublication) -> &'static str {
    match publication {
        ConversationPublication::None => "none",
        ConversationPublication::AnimationFrame => "animation-frame",
        ConversationPublication::Immediate => "immediate",
    }
}

fn index_set_to_js(values: indexmap::IndexSet<u64>) -> Set {
    let result = Set::new(&JsValue::UNDEFINED);
    for value in values {
        #[allow(clippy::cast_precision_loss)]
        result.add(&JsValue::from_f64(value as f64));
    }
    result
}

fn required(value: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    let member = Reflect::get(value, &JsValue::from_str(key))?;
    if member.is_undefined() || member.is_null() {
        Err(js_sys::Error::new(&format!("Conversation value requires {key:?}")).into())
    } else {
        Ok(member)
    }
}

fn required_string(value: &JsValue, key: &str) -> Result<String, JsValue> {
    required(value, key)?.as_string().ok_or_else(|| {
        js_sys::Error::new(&format!("Conversation value {key} must be a string")).into()
    })
}

fn js_error(error: impl std::fmt::Display) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}
