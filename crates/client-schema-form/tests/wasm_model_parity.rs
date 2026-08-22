//! Browser `undefined`, hostile-validator, and immutable identity parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_schema_form::*;
use seekdeep_schemastery::Schema;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

fn strings(values: &[&str]) -> Array {
    values
        .iter()
        .map(|value| JsValue::from_str(value))
        .collect()
}

#[wasm_bindgen_test]
fn validates_hostile_throws_and_rehydrated_nodes() {
    let schema = Schema::object([("name", Schema::string().required())]);
    let wire = serde_wasm_bindgen::to_value(&schema.to_json()).unwrap();
    let root = rehydrate_schema_js(wire).unwrap();
    let root: JsValue = root.into();
    assert_eq!(
        validate_draft_js(
            root.clone(),
            Function::new_no_args("return { name: 'ok' }")
                .call0(&JsValue::UNDEFINED)
                .unwrap()
        ),
        None
    );
    assert!(
        validate_draft_js(
            root.clone(),
            Function::new_no_args("return { name: 42 }")
                .call0(&JsValue::UNDEFINED)
                .unwrap()
        )
        .unwrap()
        .contains("name")
    );
    let hostile = Function::new_no_args("throw 'plain-string failure'");
    assert_eq!(
        validate_draft_js(hostile.into(), Object::new().into()).as_deref(),
        Some("plain-string failure")
    );
    let name = node_at_path_js(root, strings(&["name"]));
    assert_eq!(
        Reflect::get(&name, &JsValue::from_str("type"))
            .unwrap()
            .as_string()
            .as_deref(),
        Some("string")
    );
}

#[wasm_bindgen_test]
fn path_helpers_preserve_undefined_presence_and_immutable_spines() {
    let root = Function::new_no_args(
        "return { nested: { key: undefined }, models: [{ id: 'a' }, { id: 'b' }] };",
    )
    .call0(&JsValue::UNDEFINED)
    .unwrap();
    assert!(has_path_js(root.clone(), strings(&["nested", "key"])));
    assert!(get_path_js(root.clone(), strings(&["nested", "key"])).is_undefined());
    assert!(!has_path_js(root.clone(), strings(&["nested", "missing"])));
    let next = set_path_js(
        root.clone(),
        strings(&["providers", "openai", "baseURL"]),
        JsValue::from_str("https://x"),
    )
    .unwrap();
    assert!(
        Reflect::get(&root, &JsValue::from_str("providers"))
            .unwrap()
            .is_undefined()
    );
    assert_eq!(
        get_path_js(next.clone(), strings(&["providers", "openai", "baseURL"]))
            .as_string()
            .as_deref(),
        Some("https://x")
    );
    let direct = Array::new();
    direct.push(&JsValue::from_str("a"));
    direct.push(&JsValue::from_str("b"));
    let direct_deleted = delete_path_js(direct.into(), strings(&["0"]))
        .unwrap()
        .dyn_into::<Array>()
        .unwrap();
    assert_eq!(direct_deleted.length(), 1, "direct array delete failed");
    let deleted = delete_path_js(next.clone(), strings(&["models", "0"])).unwrap();
    let models = get_path_js(deleted, strings(&["models"]))
        .dyn_into::<Array>()
        .unwrap();
    let models_debug = Function::new_with_args("models", "return JSON.stringify(models)")
        .call1(&JsValue::UNDEFINED, &models)
        .unwrap()
        .as_string()
        .unwrap();
    assert_eq!(models.length(), 1, "models after delete: {models_debug}");
    assert_eq!(
        Reflect::get(&models.get(0), &JsValue::from_str("id"))
            .unwrap()
            .as_string()
            .as_deref(),
        Some("b")
    );
    let missing = delete_path_js(next.clone(), strings(&["missing", "x"])).unwrap();
    assert!(Object::is(&missing, &next));
}
