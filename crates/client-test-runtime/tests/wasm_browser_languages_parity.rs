//! Browser-language pin and restoration parity.

#![cfg(target_arch = "wasm32")]

use js_sys::Array;
use seekdeep_client_test_runtime::WasmBrowserLanguagePin;
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function installNavigator() {
  const prototype = { get languages() { return ['en-US'] }, get language() { return 'en-US' } }
  Object.defineProperty(globalThis, 'navigator', { configurable: true, value: Object.create(prototype) })
}
export function languages() { return navigator.languages }
export function language() { return navigator.language }
export function owns(name) { return Object.hasOwn(navigator, name) }
export function removeNavigator() { delete globalThis.navigator }
"#)]
extern "C" {
    fn installNavigator();
    fn languages() -> Array;
    fn language() -> String;
    fn owns(name: &str) -> bool;
    fn removeNavigator();
}

#[wasm_bindgen_test]
fn pins_preference_order_and_restores_inherited_accessors() {
    installNavigator();
    let rest = Array::of2(&JsValue::from_str("zh"), &JsValue::from_str("en-US"));
    let pin = WasmBrowserLanguagePin::new("zh-CN".to_owned(), rest).unwrap();
    assert_eq!(
        languages()
            .iter()
            .filter_map(|value| value.as_string())
            .collect::<Vec<_>>(),
        ["zh-CN", "zh", "en-US"]
    );
    assert_eq!(language(), "zh-CN");
    assert!(owns("languages"));
    assert!(owns("language"));
    pin.dispose();
    pin.dispose();
    assert!(!owns("languages"));
    assert!(!owns("language"));
    assert_eq!(language(), "en-US");
    removeNavigator();
}
