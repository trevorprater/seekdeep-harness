//! Browser bindings for deterministic Client runtime projection helpers.

use js_sys::{Array, Function, Map, Object, Reflect, Set};
use wasm_bindgen::{JsCast, JsValue, prelude::wasm_bindgen};

use crate::{ContextRole, KnownContextForm, context_form, context_provenance};

/// Classifies one provider-neutral content block into the Client render shape.
///
/// # Errors
///
/// Returns JavaScript property access or result construction failures.
#[wasm_bindgen(js_name = toAssistantBlock)]
#[allow(clippy::needless_pass_by_value)]
pub fn to_assistant_block_js(block: JsValue) -> Result<JsValue, JsValue> {
    let block_type = Reflect::get(&block, &JsValue::from_str("type"))?.as_string();
    let result = Object::new();
    match block_type.as_deref() {
        Some("text" | "reasoning") => {
            set(
                &result,
                "kind",
                &JsValue::from_str(block_type.as_deref().unwrap_or_default()),
            )?;
            set(
                &result,
                "text",
                &Reflect::get(&block, &JsValue::from_str("text"))?,
            )?;
        }
        Some("image") => {
            set(&result, "kind", &JsValue::from_str("image"))?;
            set(
                &result,
                "attachment",
                &Reflect::get(&block, &JsValue::from_str("attachment"))?,
            )?;
        }
        Some("tool-call") => {
            set(&result, "kind", &JsValue::from_str("tool-call"))?;
            let id = Reflect::get(&block, &JsValue::from_str("id"))?;
            set(
                &result,
                "callId",
                &id.as_string().map_or(id, |id| JsValue::from_str(&id)),
            )?;
            set(
                &result,
                "name",
                &Reflect::get(&block, &JsValue::from_str("name"))?,
            )?;
            set(
                &result,
                "argsRaw",
                &Reflect::get(&block, &JsValue::from_str("arguments"))?,
            )?;
        }
        Some(_) | None => {
            set(&result, "kind", &JsValue::from_str("other"))?;
            set(&result, "block", &block)?;
        }
    }
    Ok(result.into())
}

/// Classifies complete content blocks in source order.
///
/// # Errors
///
/// Returns the first block classification failure.
#[wasm_bindgen(js_name = toAssistantBlocks)]
#[allow(clippy::needless_pass_by_value)]
pub fn to_assistant_blocks_js(content: Array) -> Result<Array, JsValue> {
    let result = Array::new();
    for block in content.iter() {
        result.push(&to_assistant_block_js(block)?);
    }
    Ok(result)
}

/// Resolves the browser's current IANA time zone.
///
/// # Errors
///
/// Returns the source-compatible unavailable diagnostic when Intl supplies no zone.
#[wasm_bindgen(js_name = resolvedClientTimeZone)]
pub fn resolved_client_time_zone_js() -> Result<String, JsValue> {
    let intl = required(&js_sys::global(), "Intl")?;
    let constructor = required(&intl, "DateTimeFormat")?.dyn_into::<Function>()?;
    let formatter = Reflect::construct(&constructor, &Array::new())?;
    let options = call_method(&formatter, "resolvedOptions", &[])?;
    Reflect::get(&options, &JsValue::from_str("timeZone"))?
        .as_string()
        .filter(|zone| !zone.is_empty())
        .ok_or_else(|| js_sys::Error::new("browser time zone is unavailable").into())
}

/// Projects one durable source into transcript role and producer label.
///
/// # Errors
///
/// Returns JSON conversion or JavaScript object-construction failures.
#[wasm_bindgen(js_name = contextProvenance)]
#[allow(clippy::needless_pass_by_value)]
pub fn context_provenance_js(source: JsValue) -> Result<JsValue, JsValue> {
    let source = serde_wasm_bindgen::from_value(source)
        .map_err(|error| js_sys::Error::new(&error.to_string()))?;
    let view = context_provenance(&source);
    let output = Object::new();
    set(
        &output,
        "role",
        &JsValue::from_str(match view.role {
            ContextRole::Inject => "inject",
            ContextRole::Recall => "recall",
        }),
    )?;
    set(
        &output,
        "label",
        &view
            .label
            .map_or(JsValue::NULL, |label| JsValue::from_str(&label)),
    )?;
    Ok(output.into())
}

/// Reads a producer-declared form known to this Client version.
///
/// # Errors
///
/// Returns JSON conversion failures.
#[wasm_bindgen(js_name = contextForm)]
#[allow(clippy::needless_pass_by_value)]
pub fn context_form_js(source: JsValue) -> Result<JsValue, JsValue> {
    let source = serde_wasm_bindgen::from_value(source)
        .map_err(|error| js_sys::Error::new(&error.to_string()))?;
    Ok(context_form(&source).map_or(JsValue::NULL, |form| {
        JsValue::from_str(match form {
            KnownContextForm::Instructions => "instructions",
            KnownContextForm::Catalog => "catalog",
            KnownContextForm::Snapshot => "snapshot",
            KnownContextForm::Notice => "notice",
            KnownContextForm::Relay => "relay",
            KnownContextForm::Recall => "recall",
        })
    }))
}

/// Reconciles authoritative rows while preserving established visible identity order.
///
/// # Errors
///
/// Returns selector callback failures.
#[wasm_bindgen(js_name = mergeOrderedBaseline)]
#[allow(clippy::needless_pass_by_value)]
pub fn merge_ordered_baseline_js(
    current: Array,
    baseline: Array,
    key_of: Function,
) -> Result<Array, JsValue> {
    let by_key = Map::new();
    for value in baseline.iter() {
        by_key.set(&key_of.call1(&JsValue::UNDEFINED, &value)?, &value);
    }
    let merged = Array::new();
    for value in current.iter() {
        let key = key_of.call1(&JsValue::UNDEFINED, &value)?;
        if by_key.has(&key) {
            merged.push(&by_key.get(&key));
        }
    }
    let merged_keys = Set::new(&JsValue::UNDEFINED);
    for value in merged.iter() {
        merged_keys.add(&key_of.call1(&JsValue::UNDEFINED, &value)?);
    }
    for index in 0..baseline.length() {
        let value = baseline.get(index);
        let key = key_of.call1(&JsValue::UNDEFINED, &value)?;
        if merged_keys.has(&key) {
            continue;
        }
        let mut insertion = merged.length();
        for following in index + 1..baseline.length() {
            let candidate = key_of.call1(&JsValue::UNDEFINED, &baseline.get(following))?;
            for known in 0..merged.length() {
                if Object::is(
                    &key_of.call1(&JsValue::UNDEFINED, &merged.get(known))?,
                    &candidate,
                ) {
                    insertion = known;
                    break;
                }
            }
            if insertion != merged.length() {
                break;
            }
        }
        merged.splice(insertion, 0, &value);
        merged_keys.add(&key);
    }
    Ok(merged)
}

fn required(value: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    let value = Reflect::get(value, &JsValue::from_str(key))?;
    if value.is_undefined() || value.is_null() {
        Err(js_sys::Error::new(&format!("Client runtime requires {key:?}")).into())
    } else {
        Ok(value)
    }
}

fn call_method(value: &JsValue, method: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = required(value, method)?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(object, &JsValue::from_str(key), value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new(&format!("failed to set {key:?}")).into())
    }
}
