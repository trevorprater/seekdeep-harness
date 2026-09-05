//! Compiled readers over the internal keyed Tool chat-node store.

use js_sys::{Array, Function, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};

/// Reads one root Tool lifecycle through its collision-free Context key.
///
/// # Errors
///
/// Returns when the snapshot's keyed chat-node surface is malformed.
#[wasm_bindgen(js_name = rootToolCall)]
#[allow(clippy::needless_pass_by_value)]
pub fn root_tool_call_browser(snapshot: JsValue, root_call_id: String) -> Result<JsValue, JsValue> {
    let nodes = chat_nodes(&snapshot)?;
    let key = conversation_context_key("tool-call", &root_call_id);
    let node = call_method(&nodes, "get", &[JsValue::from_str(&key)])?;
    let Some(node) = tool_node(node)? else {
        return Ok(JsValue::UNDEFINED);
    };
    let data = required_property(&node, "data", "tool-call chat node")?;
    Reflect::get(&data, &JsValue::from_str("root"))
}

/// Finds a root or nested Tool lifecycle in keyed-node insertion order.
///
/// # Errors
///
/// Returns when the snapshot or a materialized Tool tree is malformed.
#[wasm_bindgen(js_name = findToolCall)]
#[allow(clippy::needless_pass_by_value)]
pub fn find_tool_call_browser(snapshot: JsValue, call_id: String) -> Result<JsValue, JsValue> {
    let nodes = chat_nodes(&snapshot)?;
    let iterator = call_method(&nodes, "values", &[])?;
    loop {
        let step = call_method(&iterator, "next", &[])?;
        if required_property(&step, "done", "chat-node iterator step")?
            .as_bool()
            .ok_or_else(|| js_sys::TypeError::new("iterator done must be a boolean"))?
        {
            return Ok(JsValue::UNDEFINED);
        }
        let node = Reflect::get(&step, &JsValue::from_str("value"))?;
        let Some(node) = tool_node(node)? else {
            continue;
        };
        let data = required_property(&node, "data", "tool-call chat node")?;
        let root = Reflect::get(&data, &JsValue::from_str("root"))?;
        if root.is_undefined() {
            continue;
        }
        if let Some(found) = visit_tool_call(&root, &call_id)? {
            return Ok(found);
        }
    }
}

fn chat_nodes(snapshot: &JsValue) -> Result<JsValue, JsValue> {
    let chat = required_property(snapshot, "chat", "conversation snapshot")?;
    required_property(&chat, "nodes", "chat snapshot")
}

fn tool_node(node: JsValue) -> Result<Option<JsValue>, JsValue> {
    if node.is_null() || node.is_undefined() {
        return Ok(None);
    }
    let kind = Reflect::get(&node, &JsValue::from_str("kind"))?;
    Ok((kind.as_string().as_deref() == Some("tool-call")).then_some(node))
}

fn visit_tool_call(block: &JsValue, call_id: &str) -> Result<Option<JsValue>, JsValue> {
    if Reflect::get(block, &JsValue::from_str("callId"))?
        .as_string()
        .as_deref()
        == Some(call_id)
    {
        return Ok(Some(block.clone()));
    }
    let children = required_property(block, "subCalls", "Tool call block")?.dyn_into::<Array>()?;
    for index in 0..children.length() {
        if let Some(found) = visit_tool_call(&children.get(index), call_id)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

fn conversation_context_key(kind: &str, id: &str) -> String {
    format!("{}:{kind}{id}", kind.encode_utf16().count())
}

fn required_property(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Err(js_sys::Error::new(&format!("{owner} omitted {key}")).into())
    } else {
        Ok(property)
    }
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let arguments: Array = arguments.iter().collect();
    method.apply(value, &arguments)
}
