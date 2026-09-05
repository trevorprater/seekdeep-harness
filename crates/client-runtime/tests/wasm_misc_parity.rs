//! Live browser projection helper parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_runtime::{
    context_form_js, context_provenance_js, merge_ordered_baseline_js, resolved_client_time_zone_js,
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_test::*;

fn set(object: &Object, key: &str, value: &JsValue) {
    assert!(Reflect::set(object, &JsValue::from_str(key), value).unwrap());
}

fn get(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn row(id: &str, value: f64) -> JsValue {
    let row = Object::new();
    set(&row, "id", &JsValue::from_str(id));
    set(&row, "value", &JsValue::from_f64(value));
    row.into()
}

#[wasm_bindgen_test]
fn browser_time_zone_matches_runtime_intl() {
    let zone = resolved_client_time_zone_js().unwrap();
    assert!(!zone.is_empty());
    assert!(zone.contains('/') || zone == "UTC");
}

#[wasm_bindgen_test]
fn durable_context_projection_and_known_forms_keep_js_shapes() {
    let source = Object::new();
    set(&source, "kind", &JsValue::from_str("session-reference"));
    set(&source, "form", &JsValue::from_str("recall"));
    let references = Array::new();
    for label in ["Loader", "CI"] {
        let reference = Object::new();
        set(&reference, "label", &JsValue::from_str(label));
        references.push(&reference);
    }
    set(&source, "references", &references);
    let view = context_provenance_js(source.clone().into()).unwrap();
    assert_eq!(get(&view, "role").as_string().as_deref(), Some("recall"));
    assert_eq!(
        get(&view, "label").as_string().as_deref(),
        Some("Loader, CI")
    );
    assert_eq!(
        context_form_js(source.into())
            .unwrap()
            .as_string()
            .as_deref(),
        Some("recall")
    );
    assert!(context_form_js(Object::new().into()).unwrap().is_null());
}

#[wasm_bindgen_test]
fn ordered_baseline_uses_js_key_identity_and_returns_baseline_values() {
    let current = Array::new();
    current.push(&row("b", 1.0));
    current.push(&row("d", 1.0));
    current.push(&row("gone", 1.0));
    let baseline = Array::new();
    for (id, value) in [("a", 2.0), ("b", 2.0), ("c", 2.0), ("d", 2.0)] {
        baseline.push(&row(id, value));
    }
    let key = Closure::wrap(
        Box::new(|row: JsValue| get(&row, "id")) as Box<dyn FnMut(JsValue) -> JsValue>
    );
    let merged = merge_ordered_baseline_js(
        current,
        baseline,
        key.into_js_value().unchecked_into::<Function>(),
    )
    .unwrap();
    assert_eq!(
        merged
            .iter()
            .map(|row| get(&row, "id").as_string().unwrap())
            .collect::<Vec<_>>(),
        ["a", "b", "c", "d"]
    );
    assert!(
        merged
            .iter()
            .all(|row| get(&row, "value").as_f64() == Some(2.0))
    );
}
