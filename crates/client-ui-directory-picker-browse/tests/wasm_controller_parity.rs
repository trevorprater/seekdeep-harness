//! Live JavaScript boundary coverage for the persistent Rust controller face.

#![cfg(target_arch = "wasm32")]

use js_sys::{Function, Object, Reflect};
use seekdeep_client_ui_directory_picker_browse::create_directory_browser_state_controller;
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_test::wasm_bindgen_test;

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn object(entries: &[(&str, JsValue)]) -> JsValue {
    let value = Object::new();
    for (key, entry) in entries {
        Reflect::set(&value, &JsValue::from_str(key), entry).unwrap();
    }
    value.into()
}

#[allow(clippy::needless_pass_by_value)] // Test payloads are one-shot JS objects.
fn dispatch(controller: &JsValue, action: &str, payload: JsValue) -> JsValue {
    property(controller, "dispatch")
        .dyn_into::<Function>()
        .unwrap()
        .call2(controller, &JsValue::from_str(action), &payload)
        .unwrap()
}

fn snapshot(controller: &JsValue) -> JsValue {
    property(controller, "snapshot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(controller)
        .unwrap()
}

fn json(source: &str) -> JsValue {
    js_sys::JSON::parse(source).unwrap()
}

fn submitted() -> JsValue {
    json(r#"{"closeEditor":true,"announce":true}"#)
}

fn home() -> JsValue {
    json(
        r#"{"path":"/home/u","home":"/home/u","crumbs":[{"name":"/","path":"/","hidden":false},{"name":"home","path":"/home","hidden":false},{"name":"u","path":"/home/u","hidden":false}],"entries":[{"name":"Documents","path":"/home/u/Documents","hidden":false}],"truncated":false}"#,
    )
}

fn docs() -> JsValue {
    json(
        r#"{"path":"/home/u/Documents","home":"/home/u","crumbs":[{"name":"/","path":"/","hidden":false},{"name":"home","path":"/home","hidden":false},{"name":"u","path":"/home/u","hidden":false},{"name":"Documents","path":"/home/u/Documents","hidden":false}],"entries":[{"name":"harness","path":"/home/u/Documents/harness","hidden":false}],"truncated":false}"#,
    )
}

#[wasm_bindgen_test]
fn target_parent_timeout_and_late_upgrade_cross_the_js_boundary() {
    let controller = create_directory_browser_state_controller().unwrap();
    let home_launch = dispatch(&controller, "open", JsValue::UNDEFINED);
    dispatch(
        &controller,
        "targetLanded",
        object(&[
            ("launch", home_launch),
            ("target", home()),
            ("options", submitted()),
        ]),
    );
    let target_launch = dispatch(
        &controller,
        "beginLanding",
        object(&[
            ("path", JsValue::from_str("/home/u/Documents")),
            ("options", submitted()),
        ]),
    );
    let outcome = dispatch(
        &controller,
        "targetLanded",
        object(&[
            ("launch", target_launch),
            ("target", docs()),
            ("options", submitted()),
        ]),
    );
    assert_eq!(
        property(&outcome, "kind").as_string().as_deref(),
        Some("parent")
    );
    let parent = property(&outcome, "parent");
    let seq = property(&parent, "seq");
    dispatch(
        &controller,
        "parentWaitElapsed",
        object(&[("seq", seq.clone())]),
    );
    assert_eq!(
        property(&property(&snapshot(&controller), "parent"), "path")
            .as_string()
            .as_deref(),
        Some("/home/u/Documents")
    );
    dispatch(
        &controller,
        "parentLanded",
        object(&[("seq", seq), ("parent", home())]),
    );
    let state = snapshot(&controller);
    assert_eq!(
        property(&property(&state, "parent"), "path")
            .as_string()
            .as_deref(),
        Some("/home/u")
    );
    assert_eq!(
        property(&property(&state, "selected"), "path")
            .as_string()
            .as_deref(),
        Some("/home/u/Documents")
    );
}

#[wasm_bindgen_test]
fn draft_selection_creation_and_generation_fences_are_live() {
    let controller = create_directory_browser_state_controller().unwrap();
    let home_launch = dispatch(&controller, "open", JsValue::UNDEFINED);
    dispatch(
        &controller,
        "targetLanded",
        object(&[
            ("launch", home_launch),
            ("target", home()),
            ("options", submitted()),
        ]),
    );
    dispatch(&controller, "openPathEditor", JsValue::UNDEFINED);
    let token = dispatch(
        &controller,
        "editPath",
        object(&[("draft", JsValue::from_str("/home/u/Documents/har"))]),
    );
    let preview = dispatch(&controller, "previewElapsed", token);
    assert_eq!(
        property(&preview, "path").as_string().as_deref(),
        Some("/home/u/Documents/")
    );
    dispatch(&controller, "openCreateDialog", JsValue::UNDEFINED);
    dispatch(
        &controller,
        "editFolderName",
        object(&[("draft", JsValue::from_str(" fresh "))]),
    );
    let create = dispatch(&controller, "confirmCreate", JsValue::UNDEFINED);
    assert_eq!(
        property(&create, "name").as_string().as_deref(),
        Some(" fresh ")
    );
    dispatch(&controller, "close", JsValue::UNDEFINED);
    dispatch(&controller, "open", JsValue::UNDEFINED);
    let stale = dispatch(
        &controller,
        "creationSucceeded",
        object(&[
            ("launch", create),
            ("createdPath", JsValue::from_str("/home/u/ fresh ")),
        ]),
    );
    assert!(stale.is_undefined() || stale.is_null());
    let reopened = snapshot(&controller);
    assert_eq!(property(&reopened, "creatingFolder"), JsValue::FALSE);
    assert_eq!(property(&reopened, "showHidden"), JsValue::FALSE);
}
