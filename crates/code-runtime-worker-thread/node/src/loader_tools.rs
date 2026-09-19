//! Lossless tool callbacks owned by one file-plugin activation.

use std::{cell::RefCell, rc::Rc};

use serde_json::{Value, json};
use wasm_bindgen::prelude::*;

use crate::{
    bridge,
    loader::{Realm, settled, wire},
    loader_context::{Activation, call, callback, command_disposer, drain, next_effect},
    snapshot::{Intrinsics, snapshot},
    worker_json::decode_code_json,
};

fn required_function(object: &JsValue, name: &str) -> Result<JsValue, JsValue> {
    let value = bridge::get(object, name)?;
    if !value.is_function() {
        return Err(bridge::error(&format!("Tool {name} must be a function")));
    }
    Ok(value)
}

fn register(
    realm: &Rc<RefCell<Realm>>,
    activation: &Rc<RefCell<Activation>>,
    tool: &JsValue,
) -> Result<JsValue, JsValue> {
    required_function(tool, "execute")?;
    let output = bridge::get(tool, "output")?;
    required_function(&output, "render")?;
    let name = bridge::get(tool, "name")?
        .as_string()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| bridge::error("Tool name must be a non-empty string"))?;
    let description = bridge::get(tool, "description")?
        .as_string()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| bridge::error("Tool description must be a non-empty string"))?;
    let presentation = bridge::get(&output, "presentationMeta")?;
    if !presentation.is_undefined() && !presentation.is_function() {
        return Err(bridge::error(
            "Tool output.presentationMeta must be a function",
        ));
    }
    let classifier = bridge::get(tool, "isConcurrencySafe")?;
    let effect = next_effect(activation);
    activation.borrow_mut().tools.insert(effect, tool.clone());
    let timeout = bridge::get(tool, "timeoutMs")?;
    call(
        realm,
        activation,
        json!({"type":"registerTool","effect":effect,"tool":effect,"name":name,"description":description,"parameters":wire(&bridge::get(tool,"parameters")?)?,"outputSchema":wire(&bridge::get(&output,"schema")?)?,"timeoutMs":if timeout.is_undefined(){Value::Null}else{wire(&timeout)?},"presentationMeta":presentation.is_function(),"concurrency":classifier.is_function()}),
    )?;
    Ok(command_disposer(realm, activation, effect))
}

pub(crate) fn install(
    realm: &Rc<RefCell<Realm>>,
    activation: &Rc<RefCell<Activation>>,
    context: &JsValue,
    schemas: &Value,
) -> Result<(), JsValue> {
    let owner = realm.clone();
    let active = activation.clone();
    let register = callback(move |args| register(&owner, &active, &args.get(0)));
    let schema_snapshot = schemas.clone();
    let schema_function = callback(move |_| bridge::from_json(&schema_snapshot));
    let active = activation.clone();
    let listed = bridge::from_json(schemas)?;
    let get = callback(move |args| {
        let name = bridge::string(&args.get(0))?;
        let values = active.borrow().tools.values().cloned().collect::<Vec<_>>();
        for tool in values {
            if bridge::get(&tool, "name")?.as_string().as_deref() == Some(&name) {
                return Ok(tool);
            }
        }
        for schema in js_sys::Array::from(&listed).iter() {
            if bridge::get(&schema, "name")?.as_string().as_deref() == Some(&name) {
                return Ok(schema);
            }
        }
        Ok(JsValue::UNDEFINED)
    });
    let tools = bridge::object(&[
        ("register", register),
        ("schemas", schema_function),
        ("get", get),
    ])?;
    bridge::set_key(context, &JsValue::from_str("tools"), &tools)?;
    Ok(())
}

fn argument(request: &Value, key: &str) -> Result<JsValue, JsValue> {
    bridge::parse(
        request[key]
            .as_str()
            .ok_or_else(|| bridge::error("Tool JSON argument is missing"))?,
    )
}

async fn run(realm: &Rc<RefCell<Realm>>, request: &Value) -> Result<Value, JsValue> {
    let activation = realm
        .borrow()
        .activations
        .get(&request["activation"].as_u64().unwrap_or_default())
        .cloned()
        .ok_or_else(|| bridge::error("Tool activation is disposed"))?;
    let tool = activation
        .borrow()
        .tools
        .get(&request["tool"].as_u64().unwrap_or_default())
        .cloned()
        .ok_or_else(|| bridge::error("Tool is no longer registered"))?;
    let args = argument(request, "argsRaw")?;
    let phase = request["phase"].as_str().unwrap_or_default();
    let (receiver, function, arguments) = match phase {
        "execute" => (
            tool.clone(),
            required_function(&tool, "execute")?,
            vec![args],
        ),
        "render" | "presentationMeta" => {
            let output = bridge::get(&tool, "output")?;
            let function = required_function(&output, phase)?;
            (output, function, vec![args, argument(request, "valueRaw")?])
        }
        "concurrency" => (
            tool.clone(),
            required_function(&tool, "isConcurrencySafe")?,
            vec![args],
        ),
        _ => return Err(bridge::error("unknown Tool callback phase")),
    };
    let value = settled(bridge::apply(&function, &receiver, &arguments)?).await?;
    drain(&activation).await?;
    if phase == "concurrency" {
        return Ok(json!({"raw":if value.as_bool()==Some(true){"true"}else{"false"}}));
    }
    let intrinsics: Intrinsics = realm.borrow().intrinsics.clone();
    let snapshot = snapshot(&value, &intrinsics)
        .ok_or_else(|| bridge::error(&format!("Tool {phase} result must be lossless JSON")))?;
    let value = decode_code_json(&snapshot.wire)
        .ok_or_else(|| bridge::error("Tool result JSON is invalid"))?;
    Ok(json!({"raw":value.as_raw()}))
}

pub(crate) async fn invoke(realm: &Rc<RefCell<Realm>>, request: &Value) -> Value {
    match run(realm, request).await {
        Ok(value) => value,
        Err(error) => json!({"rawError":bridge::message_text(&error).as_raw()}),
    }
}
