//! Browser-language pin for WASM feature tests.

use std::cell::Cell;

use js_sys::{Array, Object, Reflect};
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};

/// Per-test browser-language override with idempotent restoration.
#[wasm_bindgen]
pub struct WasmBrowserLanguagePin {
    navigator: JsValue,
    active: Cell<bool>,
}

#[wasm_bindgen]
impl WasmBrowserLanguagePin {
    /// Pins `navigator.languages` and `navigator.language` as configurable own properties.
    ///
    /// # Errors
    ///
    /// Returns missing navigator or property-definition failures.
    #[wasm_bindgen(constructor)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(primary: String, rest: Array) -> Result<Self, JsValue> {
        let navigator = Reflect::get(&js_sys::global(), &JsValue::from_str("navigator"))?;
        if navigator.is_undefined() || navigator.is_null() {
            return Err(js_sys::Error::new("browser language pin requires navigator").into());
        }
        let languages = Array::new();
        languages.push(&JsValue::from_str(&primary));
        for value in rest.iter() {
            languages.push(&value);
        }
        define_value(&navigator, "languages", languages.as_ref())?;
        if let Err(error) = define_value(&navigator, "language", &JsValue::from_str(&primary)) {
            let _ = Reflect::delete_property(
                &Object::from(navigator.clone()),
                &JsValue::from_str("languages"),
            );
            return Err(error);
        }
        Ok(Self {
            navigator,
            active: Cell::new(true),
        })
    }

    /// Deletes both own overrides so the environment's accessors are visible again.
    pub fn dispose(&self) {
        restore(&self.navigator, &self.active);
    }
}

impl Drop for WasmBrowserLanguagePin {
    fn drop(&mut self) {
        restore(&self.navigator, &self.active);
    }
}

fn define_value(target: &JsValue, key: &str, value: &JsValue) -> Result<(), JsValue> {
    let descriptor = Object::new();
    Reflect::set(&descriptor, &JsValue::from_str("value"), value)?;
    Reflect::set(
        &descriptor,
        &JsValue::from_str("configurable"),
        &JsValue::TRUE,
    )?;
    Object::define_property(
        &Object::from(target.clone()),
        &JsValue::from_str(key),
        &descriptor,
    );
    Ok(())
}

fn restore(navigator: &JsValue, active: &Cell<bool>) {
    if !active.replace(false) {
        return;
    }
    let navigator = Object::from(navigator.clone());
    let _ = Reflect::delete_property(&navigator, &JsValue::from_str("languages"));
    let _ = Reflect::delete_property(&navigator, &JsValue::from_str("language"));
}
