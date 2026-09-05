//! Browser bindings for the Client Runtime public helper barrel.

use std::collections::BTreeMap;

use js_sys::{Array, Function, Map, Object, Reflect};
use seekdeep_identity::SessionId;
use wasm_bindgen::{JsCast, JsValue, prelude::wasm_bindgen};

use crate::{
    SubagentSessionSummary, conversation_context_key, index_subagent_descendants,
    wasm_session::{empty_chat_snapshot, js_to_json},
    workspace_title_of,
};

const PUBLIC_WASM_GLOBAL: &str = "__seekdeep_client_runtime_wasm";

pub(crate) fn construct_public_error(class_name: &str, arguments: &[JsValue]) -> Option<JsValue> {
    let exports = Reflect::get(&js_sys::global(), &JsValue::from_str(PUBLIC_WASM_GLOBAL)).ok()?;
    let constructor = Reflect::get(&exports, &JsValue::from_str(class_name))
        .ok()?
        .dyn_into::<Function>()
        .ok()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    Reflect::construct(&constructor, &args).ok()
}

/// Whether an event is one of the three append-origin model-surface records.
///
/// # Errors
///
/// Returns JavaScript property-access failures.
#[wasm_bindgen(js_name = isAppendSurfaceEvent)]
#[allow(clippy::needless_pass_by_value)]
pub fn is_append_surface_event_js(event: JsValue) -> Result<bool, JsValue> {
    Ok(is_surface_event(&event)?
        && Reflect::get(&event, &JsValue::from_str("surfaceOp"))?
            .as_string()
            .as_deref()
            == Some("append"))
}

/// Whether an event is one of the three model-surface replacement records.
///
/// # Errors
///
/// Returns JavaScript property-access failures.
#[wasm_bindgen(js_name = isReplacementSurfaceEvent)]
#[allow(clippy::needless_pass_by_value)]
pub fn is_replacement_surface_event_js(event: JsValue) -> Result<bool, JsValue> {
    if !is_surface_event(&event)? {
        return Ok(false);
    }
    Ok(Reflect::get(&event, &JsValue::from_str("surfaceOp"))?
        .as_string()
        .as_deref()
        != Some("append"))
}

/// Builds one collision-free Definition Context key using JavaScript UTF-16 length.
#[wasm_bindgen(js_name = conversationContextKey)]
pub fn conversation_context_key_js(kind: &str, id: &str) -> String {
    conversation_context_key(kind, id)
}

/// Returns the cross-platform display basename of one Workspace path.
#[wasm_bindgen(js_name = workspaceTitleOf)]
pub fn workspace_title_of_js(cwd: &str) -> String {
    workspace_title_of(cwd)
}

/// Resolves a Workspace-relative Host path lexically across POSIX and Windows spellings.
#[wasm_bindgen(js_name = resolveWorkspacePath)]
pub fn resolve_workspace_path_js(cwd: Option<String>, path: &str) -> String {
    if is_host_absolute(path) {
        return path.to_owned();
    }
    let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty()) else {
        return path.to_owned();
    };
    let base = cwd.trim_end_matches(['/', '\\']);
    let relative = path.trim_start_matches(['/', '\\']);
    format!("{base}/{relative}")
}

/// Returns the empty projection for one streamed Assistant block kind.
///
/// # Errors
///
/// Returns JavaScript object-construction failures.
#[wasm_bindgen(js_name = emptyAssistantBlock)]
pub fn empty_assistant_block_js(block_type: &str) -> Result<JsValue, JsValue> {
    let value = Object::new();
    match block_type {
        "text" | "reasoning" => {
            set(&value, "kind", &JsValue::from_str(block_type))?;
            set(&value, "text", &JsValue::from_str(""))?;
        }
        "tool-call" => {
            set(&value, "kind", &JsValue::from_str("tool-call"))?;
            set(&value, "callId", &JsValue::from_str(""))?;
            set(&value, "name", &JsValue::from_str(""))?;
            set(&value, "argsRaw", &JsValue::from_str(""))?;
        }
        _ => {
            set(&value, "kind", &JsValue::from_str("other"))?;
            set(&value, "block", &JsValue::NULL)?;
        }
    }
    Ok(value.into())
}

/// Whether one stream chunk carries non-empty visible model output.
///
/// # Errors
///
/// Returns JavaScript property-access failures.
#[wasm_bindgen(js_name = isTokenDelta)]
#[allow(clippy::needless_pass_by_value)]
pub fn is_token_delta_js(chunk: JsValue) -> Result<bool, JsValue> {
    let kind = Reflect::get(&chunk, &JsValue::from_str("type"))?.as_string();
    match kind.as_deref() {
        Some("text-delta" | "reasoning-delta") => {
            Ok(Reflect::get(&chunk, &JsValue::from_str("text"))?
                .as_string()
                .is_some_and(|text| !text.is_empty()))
        }
        Some("tool-call-delta") => {
            let arguments = Reflect::get(&chunk, &JsValue::from_str("argumentsDelta"))?
                .as_string()
                .is_some_and(|arguments| !arguments.is_empty());
            let name = !Reflect::get(&chunk, &JsValue::from_str("name"))?.is_undefined();
            Ok(arguments || name)
        }
        _ => Ok(false),
    }
}

/// Aggregates uninterrupted subagent-origin descendants by ancestor Session.
///
/// # Errors
///
/// Returns malformed summary or JavaScript result-construction failures.
#[wasm_bindgen(js_name = indexSubagentDescendants)]
#[allow(clippy::needless_pass_by_value)]
pub fn index_subagent_descendants_js(summaries: JsValue) -> Result<Map, JsValue> {
    let object = Object::from(summaries);
    let mut parsed = BTreeMap::new();
    for id in Object::keys(&object).iter() {
        let id = id
            .as_string()
            .ok_or_else(|| js_sys::Error::new("Session summary key must be a string"))?;
        let summary = Reflect::get(&object, &JsValue::from_str(&id))?;
        parsed.insert(
            SessionId::new(&id),
            SubagentSessionSummary {
                id: SessionId::new(required_string(&summary, "id")?),
                parent_id: optional_string(&summary, "parentId")?.map(SessionId::new),
                subagent_origin: optional_string(&summary, "origin")?.as_deref()
                    == Some("subagent"),
                running: Reflect::get(&summary, &JsValue::from_str("running"))?
                    .as_bool()
                    .ok_or_else(|| js_sys::Error::new("Session summary running must be boolean"))?,
            },
        );
    }
    let result = Map::new();
    for (id, summary) in index_subagent_descendants(&parsed) {
        let value = Object::new();
        set(&value, "count", &js_usize(summary.count))?;
        set(&value, "runningCount", &js_usize(summary.running_count))?;
        result.set(&JsValue::from_str(id.as_str()), &value);
    }
    Ok(result)
}

/// Produces copy-safe display text for one durable failure value.
///
/// # Errors
///
/// Returns values that cannot cross the JSON-compatible browser boundary.
#[wasm_bindgen(js_name = displayFailureMessage)]
#[allow(clippy::needless_pass_by_value)]
pub fn display_failure_message_js(failure: JsValue) -> Result<String, JsValue> {
    Ok(seekdeep_failure_display::display_failure_message(
        &js_to_json(&failure)?,
    ))
}

/// Creates the reference shape used for `EMPTY_CHAT_SNAPSHOT` compatibility.
///
/// # Errors
///
/// Returns JavaScript object-construction failures.
#[wasm_bindgen(js_name = emptyChatSnapshot)]
pub fn empty_chat_snapshot_js() -> Result<JsValue, JsValue> {
    empty_chat_snapshot()
}

/// Creates the empty registered-view snapshot reader.
///
/// # Errors
///
/// Returns JavaScript object-construction failures.
#[wasm_bindgen(js_name = emptyConversationViews)]
pub fn empty_conversation_views_js() -> Result<JsValue, JsValue> {
    let value = Object::new();
    set(
        &value,
        "get",
        &js_sys::Function::new_with_args("target", "return undefined"),
    )?;
    Ok(value.into())
}

fn is_surface_event(event: &JsValue) -> Result<bool, JsValue> {
    let event_type = Reflect::get(event, &JsValue::from_str("type"))?.as_string();
    if !matches!(
        event_type.as_deref(),
        Some("user/message" | "assistant/message" | "tool/result")
    ) {
        return Ok(false);
    }
    Ok(!Reflect::get(event, &JsValue::from_str("surfaceOp"))?.is_undefined())
}

fn is_host_absolute(path: &str) -> bool {
    path.starts_with('/')
        || path.starts_with(r"\\")
        || matches!(
            path.as_bytes(),
            [drive, b':', separator, ..]
                if drive.is_ascii_alphabetic() && matches!(separator, b'/' | b'\\')
        )
}

fn required_string(value: &JsValue, key: &str) -> Result<String, JsValue> {
    Reflect::get(value, &JsValue::from_str(key))?
        .as_string()
        .ok_or_else(|| {
            js_sys::Error::new(&format!("Session summary {key} must be a string")).into()
        })
}

fn optional_string(value: &JsValue, key: &str) -> Result<Option<String>, JsValue> {
    let value = Reflect::get(value, &JsValue::from_str(key))?;
    if value.is_undefined() {
        return Ok(None);
    }
    value.as_string().map(Some).ok_or_else(|| {
        js_sys::Error::new(&format!("Session summary {key} must be a string")).into()
    })
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(object, &JsValue::from_str(key), value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new(&format!("failed to set public helper member {key:?}")).into())
    }
}

fn js_usize(value: usize) -> JsValue {
    #[allow(clippy::cast_precision_loss)]
    {
        JsValue::from_f64(value as f64)
    }
}
