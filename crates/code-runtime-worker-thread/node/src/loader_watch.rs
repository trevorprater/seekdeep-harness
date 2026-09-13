//! Rust ownership of source-compatible Chokidar watcher options and event lifetimes.

use std::{cell::RefCell, rc::Rc};

use serde_json::{Value, json};
use wasm_bindgen::prelude::*;

use crate::{
    bridge,
    loader::{Realm, emit, error_wire, settled},
};

#[path = "../../../hmr/src/ignore.rs"]
#[allow(
    unreachable_pub,
    reason = "the same matcher module is public in the native HMR crate"
)]
mod ignore;

pub(crate) struct Watcher {
    watcher: JsValue,
    callbacks: Vec<JsValue>,
}

pub(crate) fn open(realm: &Rc<RefCell<Realm>>, request: &Value) -> Result<Value, JsValue> {
    let id = request["watch"]
        .as_u64()
        .ok_or_else(|| bridge::error("watch identity is missing"))?;
    let options = bridge::from_json(&request["options"])?;
    let roots = bridge::from_json(&request["roots"])?;
    let mut callbacks = Vec::new();
    if let Some(ignored) = request["ignored"].as_object() {
        let base = JsValue::from_str(ignored["base"].as_str().unwrap_or_default());
        let patterns = ignored["patterns"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let matcher = ignore::IgnoreMatcher::new(&patterns)
            .map_err(|error| bridge::error(&error.to_string()))?;
        let relative = bridge::api("relative")?;
        let callback = Closure::<dyn FnMut(JsValue) -> Result<bool, JsValue>>::new(move |path| {
            let path = bridge::apply(&relative, &JsValue::UNDEFINED, &[base.clone(), path])?;
            Ok(matcher.is_match(&bridge::string(&path)?))
        })
        .into_js_value();
        bridge::set_key(&options, &JsValue::from_str("ignored"), &callback)?;
        callbacks.push(callback);
    }
    let watcher = bridge::apply(
        &bridge::api("watch")?,
        &JsValue::UNDEFINED,
        &[roots, options],
    )?;
    for event in ["add", "change", "unlink"] {
        let owner = realm.clone();
        let callback = Closure::<dyn FnMut(JsValue)>::new(move |path: JsValue| {
            if let Some(path) = path.as_string() {
                let _ = emit(&owner, &json!({"watch":id,"type":event,"path":path}));
            }
        })
        .into_js_value();
        bridge::method(
            &watcher,
            "on",
            &[JsValue::from_str(event), callback.clone()],
        )?;
        callbacks.push(callback);
    }
    let owner = realm.clone();
    let callback = Closure::<dyn FnMut()>::new(move || {
        let _ = emit(&owner, &json!({"watch":id,"type":"ready"}));
    })
    .into_js_value();
    bridge::method(
        &watcher,
        "on",
        &[JsValue::from_str("ready"), callback.clone()],
    )?;
    callbacks.push(callback);
    let owner = realm.clone();
    let callback = Closure::<dyn FnMut(JsValue)>::new(move |error: JsValue| {
        let _ = emit(
            &owner,
            &json!({"watch":id,"type":"error","error":error_wire(&error)}),
        );
    })
    .into_js_value();
    bridge::method(
        &watcher,
        "on",
        &[JsValue::from_str("error"), callback.clone()],
    )?;
    callbacks.push(callback);
    realm
        .borrow_mut()
        .watchers
        .insert(id, Watcher { watcher, callbacks });
    if request["roots"].as_array().is_some_and(Vec::is_empty) {
        emit(realm, &json!({"watch":id,"type":"ready"}))?;
    }
    Ok(Value::Null)
}

pub(crate) async fn close(realm: &Rc<RefCell<Realm>>, id: u64) -> Result<Value, JsValue> {
    let watcher = realm.borrow_mut().watchers.remove(&id);
    if let Some(mut watcher) = watcher {
        settled(bridge::method(&watcher.watcher, "close", &[])?).await?;
        watcher.callbacks.clear();
    }
    Ok(Value::Null)
}
