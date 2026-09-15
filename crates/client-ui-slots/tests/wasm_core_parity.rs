//! Live JavaScript-observable Slot registry parity.

#![cfg(target_arch = "wasm32")]

use std::{cell::Cell, rc::Rc};

use js_sys::{Object, Promise, Reflect};
use seekdeep_client_ui_slots::{WasmSlotCore, resolve_slot_label};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::*;

fn set(object: &Object, key: &str, value: &JsValue) {
    assert!(Reflect::set(object, &JsValue::from_str(key), value).unwrap());
}

fn get(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn declaration(kind: &str, scope: &str) -> Object {
    let value = Object::new();
    set(&value, "kind", &JsValue::from_str(kind));
    set(&value, "scope", &JsValue::from_str(scope));
    value
}

fn frame_options() -> Object {
    let children = Object::new();
    set(&children, "test.single", &declaration("single", "root"));
    set(&children, "test.list", &declaration("list", "root"));
    set(&children, "test.keyed", &declaration("keyed", "session"));
    set(&children, "test.chain", &declaration("chain", "session"));
    let options = Object::new();
    set(&options, "name", &JsValue::from_str("root"));
    set(&options, "children", &children);
    options
}

fn entry_options(name: &str) -> Object {
    let options = Object::new();
    set(&options, "name", &JsValue::from_str(name));
    options
}

#[wasm_bindgen_test]
fn declaration_validation_entry_identity_and_cascade_match_source() {
    let core = WasmSlotCore::new();
    let root = core.spec_dynamic("root".to_owned()).unwrap();
    assert_eq!(get(&root, "kind").as_string().as_deref(), Some("single"));
    assert_eq!(get(&root, "scope").as_string().as_deref(), Some("root"));
    let error = core
        .register(
            entry_options("test.single").into(),
            JsValue::from_str("early"),
        )
        .unwrap_err();
    assert!(js_error(&error).contains("not declared"));

    let dispose_frame = core
        .register(frame_options().into(), JsValue::from_str("frame"))
        .unwrap();
    let dispose_entry = core
        .register(
            entry_options("test.single").into(),
            JsValue::from_str("entry"),
        )
        .unwrap();
    let first = core.entries("test.single".to_owned());
    let second = core.entries("test.single".to_owned());
    assert!(Object::is(&first, &second));
    let entry = first.get(0);
    assert!(core.is_live(entry.clone()));
    dispose_entry.call0(&JsValue::UNDEFINED).unwrap();
    assert!(!core.is_live(entry));
    dispose_entry.call0(&JsValue::UNDEFINED).unwrap();
    dispose_frame.call0(&JsValue::UNDEFINED).unwrap();
    assert!(
        core.spec_dynamic("test.single".to_owned())
            .unwrap()
            .is_undefined()
    );
}

#[wasm_bindgen_test]
fn kind_guards_parent_inject_and_late_label_resolution_are_exact() {
    let core = WasmSlotCore::new();
    let options = frame_options();
    let children = get(&options, "children");
    let injected = declaration("single", "root");
    let face = Object::new();
    set(&face, "token", &JsValue::from_str("shared"));
    set(&injected, "inject", &face);
    set(&Object::from(children), "surface.injected", &injected);
    core.register(options.into(), JsValue::from_str("frame"))
        .unwrap();
    assert!(Object::is(
        &get(
            &core.spec_dynamic("surface.injected".to_owned()).unwrap(),
            "inject"
        ),
        &face
    ));

    assert!(
        js_error(
            &core
                .register(
                    entry_options("test.keyed").into(),
                    JsValue::from_str("missing")
                )
                .unwrap_err()
        )
        .contains("requires options.key")
    );
    assert!(
        js_error(
            &core
                .register(
                    entry_options("test.list").into(),
                    JsValue::from_str("missing")
                )
                .unwrap_err()
        )
        .contains("requires options.id")
    );
    assert!(
        js_error(
            &core
                .register(
                    entry_options("test.chain").into(),
                    JsValue::from_str("missing")
                )
                .unwrap_err()
        )
        .contains("requires options.select")
    );

    let label = Closure::wrap(
        Box::new(|| -> String { "current label".to_owned() }) as Box<dyn FnMut() -> String>
    );
    assert_eq!(
        resolve_slot_label(label.into_js_value())
            .unwrap()
            .as_string()
            .as_deref(),
        Some("current label")
    );
}

#[wasm_bindgen_test(async)]
async fn versions_are_synchronous_and_notifications_batch_in_a_microtask() {
    let core = WasmSlotCore::new();
    core.register(frame_options().into(), JsValue::from_str("frame"))
        .unwrap();
    JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
    let calls = Rc::new(Cell::new(0));
    let observed = calls.clone();
    let listener = Closure::wrap(Box::new(move || {
        observed.set(observed.get() + 1);
    }) as Box<dyn FnMut()>);
    let _subscription = core.subscribe(
        "test.list".to_owned(),
        listener.into_js_value().unchecked_into(),
    );
    let before = core.version("test.list".to_owned());
    for id in ["a", "b"] {
        let options = entry_options("test.list");
        set(&options, "id", &JsValue::from_str(id));
        core.register(options.into(), JsValue::from_str(id))
            .unwrap();
    }
    assert!((core.version("test.list".to_owned()) - (before + 2.0)).abs() < f64::EPSILON);
    assert_eq!(calls.get(), 0);
    JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
    JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
    assert_eq!(calls.get(), 1);
}

#[wasm_bindgen_test]
fn shadowing_abdication_error_reporting_and_snapshot_projection_match_source() {
    let core = WasmSlotCore::new();
    core.register(frame_options().into(), JsValue::from_str("frame"))
        .unwrap();
    let low = entry_options("test.single");
    set(&low, "registrant", &JsValue::from_str("low"));
    core.register(low.into(), JsValue::from_str("low")).unwrap();
    let high = entry_options("test.single");
    set(&high, "priority", &JsValue::from_f64(5.0));
    set(&high, "registrant", &JsValue::from_str("high"));
    core.register(high.into(), JsValue::from_str("high"))
        .unwrap();
    let low = core.entries("test.single".to_owned()).get(0);
    let errors = Rc::new(Cell::new(0));
    let observed = errors.clone();
    let listener = Closure::wrap(Box::new(
        move |_key: JsValue, _entry: JsValue, error: JsValue, info: JsValue| {
            assert_eq!(error.as_string().as_deref(), Some("boom"));
            assert_eq!(get(&info, "abdicated").as_bool(), Some(true));
            observed.set(observed.get() + 1);
        },
    ) as Box<dyn FnMut(JsValue, JsValue, JsValue, JsValue)>);
    let _subscription = core.on_entry_error(listener.into_js_value().unchecked_into());
    let info = Object::new();
    set(&info, "abdicate", &JsValue::TRUE);
    core.report_entry_error(
        "test.single".to_owned(),
        low.clone(),
        JsValue::from_str("boom"),
        info.clone().into(),
    )
    .unwrap();
    core.report_entry_error(
        "test.single".to_owned(),
        low,
        JsValue::from_str("again"),
        info.into(),
    )
    .unwrap();
    assert_eq!(errors.get(), 1);
    let winner = core.entries_of_slot("test.single".to_owned()).get(0);
    assert_eq!(
        get(&winner, "component").as_string().as_deref(),
        Some("high")
    );

    let snapshot: serde_json::Value =
        serde_wasm_bindgen::from_value(core.snapshot(Some("root".to_owned())).unwrap()).unwrap();
    let single = snapshot[0]["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["name"] == "test.single")
        .unwrap();
    assert_eq!(single["occupants"].as_array().unwrap().len(), 2);
    assert_eq!(single["occupants"][0]["active"], false);
    assert_eq!(single["occupants"][1]["active"], true);
}

fn js_error(value: &JsValue) -> String {
    Reflect::get(value, &JsValue::from_str("message"))
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_else(|| format!("{value:?}"))
}
