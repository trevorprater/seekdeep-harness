//! Typed trajectory serialization retaining JavaScript maps, undefined, and raw JSON.

use js_sys::{Array, JSON, Map, Object, Reflect};
use seekdeep_lossless_json::JsonString;
use wasm_bindgen::{JsCast as _, JsValue};

const RAW_VALUE: &str = "$serde_json::private::RawValue";

pub(crate) fn from_value<T: serde::de::DeserializeOwned>(value: &JsValue) -> Result<T, String> {
    seekdeep_client_runtime::js_to_lossless_value(value)
        .map_err(|error| error_message(&error))?
        .deserialize()
        .map_err(|error| error.to_string())
}

pub(crate) fn to_value<T: serde::Serialize + ?Sized>(value: &T) -> Result<JsValue, String> {
    let value = serde_wasm_bindgen::to_value(value).map_err(|error| error.to_string())?;
    restore_serialized(value).map_err(|error| error_message(&error))
}

pub(crate) trait BrowserText {
    fn to_js_text(&self) -> JsValue;
}

impl BrowserText for str {
    fn to_js_text(&self) -> JsValue {
        JsValue::from_str(self)
    }
}

impl BrowserText for String {
    fn to_js_text(&self) -> JsValue {
        JsValue::from_str(self)
    }
}

impl BrowserText for JsonString {
    fn to_js_text(&self) -> JsValue {
        JSON::parse(self.as_raw()).expect("serialized JSON string is valid")
    }
}

impl<T: BrowserText + ?Sized> BrowserText for &T {
    fn to_js_text(&self) -> JsValue {
        (*self).to_js_text()
    }
}

pub(crate) fn text<T: BrowserText + ?Sized>(value: &T) -> JsValue {
    value.to_js_text()
}

fn restore_serialized(value: JsValue) -> Result<JsValue, JsValue> {
    if let Some(array) = value.dyn_ref::<Array>() {
        for index in 0..array.length() {
            array.set(index, restore_serialized(array.get(index))?);
        }
    } else if let Some(map) = value.dyn_ref::<Map>() {
        let entries = Array::from(&JsValue::from(map.entries()));
        map.clear();
        for entry in entries {
            let pair = Array::from(&entry);
            map.set(
                &restore_serialized(pair.get(0))?,
                &restore_serialized(pair.get(1))?,
            );
        }
    } else if value.is_object() && !value.is_null() {
        let object: &Object = value.unchecked_ref();
        let keys = Object::keys(object);
        if keys.length() == 1 && keys.get(0).as_string().as_deref() == Some(RAW_VALUE) {
            let raw = Reflect::get(&value, &JsValue::from_str(RAW_VALUE))?;
            if let Some(raw) = raw.as_string() {
                // Only serializer-created wrappers reach this walk. Parsed model
                // payloads retain their own keys, including this private spelling.
                return JSON::parse(&raw);
            }
        }
        for key in keys {
            let restored = restore_serialized(Reflect::get(&value, &key)?)?;
            Reflect::set(&value, &key, &restored)?;
        }
    }
    Ok(value)
}

fn error_message(value: &JsValue) -> String {
    value
        .as_string()
        .or_else(|| {
            Reflect::get(value, &JsValue::from_str("message"))
                .ok()?
                .as_string()
        })
        .unwrap_or_else(|| format!("{value:?}"))
}
