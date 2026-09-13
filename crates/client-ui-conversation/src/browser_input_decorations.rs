//! Browser compatibility exports for the portable draft-decoration core.

use std::collections::BTreeMap;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};

use crate::{
    DecorationClaim, DecorationOccurrence, DecorationPhase, InputDecorationState, OccurrenceId,
    ReferenceLexicon, ReferenceTrigger, derive_decorations, scan_text_refs,
};

/// Scans browser draft text for lexicon-backed references.
///
/// # Errors
///
/// Returns for malformed lexicon maps or non-string lexicon entries.
#[wasm_bindgen(js_name = scanTextRefs)]
#[allow(clippy::needless_pass_by_value)]
pub fn scan_text_refs_browser(draft: String, lexicon: JsValue) -> Result<JsValue, JsValue> {
    let lexicon = parse_lexicon(&lexicon)?;
    Ok(text_refs_value(&scan_text_refs(&draft, &lexicon))?.into())
}

/// Derives browser claim/chip/reference/hint decorations.
///
/// # Errors
///
/// Returns for malformed published input state or lexicon values.
#[wasm_bindgen(js_name = deriveDecorations)]
#[allow(clippy::needless_pass_by_value)]
pub fn derive_decorations_browser(state: JsValue, lexicon: JsValue) -> Result<JsValue, JsValue> {
    let state = parse_state(&state)?;
    let lexicon = parse_lexicon(&lexicon)?;
    let decorations = derive_decorations(&state, &lexicon);
    let token = if let Some(range) = decorations.token {
        object(&[
            ("start", JsValue::from_f64(f64::from(range.start))),
            ("end", JsValue::from_f64(f64::from(range.end))),
        ])?
        .into()
    } else {
        JsValue::NULL
    };
    let chips = Array::new();
    for chip in decorations.chips {
        chips.push(
            object(&[
                (
                    "occurrenceId",
                    JsValue::from_f64(u64_to_f64(chip.occurrence_id.get())?),
                ),
                ("offset", JsValue::from_f64(f64::from(chip.offset))),
                ("label", JsValue::from_str(&chip.label)),
                ("invalid", JsValue::from_bool(chip.invalid)),
            ])?
            .as_ref(),
        );
    }
    Ok(object(&[
        ("token", token),
        ("chips", chips.into()),
        ("textRefs", text_refs_value(&decorations.text_refs)?.into()),
        (
            "hint",
            decorations
                .hint
                .map_or(JsValue::NULL, |hint| JsValue::from_str(&hint)),
        ),
    ])?
    .into())
}

fn parse_state(value: &JsValue) -> Result<InputDecorationState, JsValue> {
    let draft = required_string(value, "draft", "input state")?;
    let phase = match required_string(value, "phase", "input state")?.as_str() {
        "claimed" => DecorationPhase::Claimed,
        "submitting" => DecorationPhase::Submitting,
        _ => DecorationPhase::Other,
    };
    let claim_value = Reflect::get(value, &JsValue::from_str("claim"))?;
    let claim = if claim_value.is_undefined() {
        None
    } else {
        Some(DecorationClaim {
            token: required_string(&claim_value, "token", "input claim")?,
            hint: Reflect::get(&claim_value, &JsValue::from_str("hint"))?.as_string(),
        })
    };
    let occurrences_value = required_property(value, "occurrences", "input state")?;
    if !Array::is_array(&occurrences_value) {
        return Err(js_sys::TypeError::new("input state occurrences must be an array").into());
    }
    let occurrences_value = occurrences_value.dyn_into::<Array>()?;
    let mut occurrences = Vec::new();
    for index in 0..occurrences_value.length() {
        let occurrence = occurrences_value.get(index);
        occurrences.push(DecorationOccurrence {
            occurrence_id: OccurrenceId::new(number_to_u64(
                numeric_property(&occurrence, "occurrenceId", "input occurrence")?,
                "input occurrence id",
            )?),
            offset: number_to_u32(
                numeric_property(&occurrence, "offset", "input occurrence")?,
                "input occurrence offset",
            )?,
            label: required_string(&occurrence, "label", "input occurrence")?,
            invalid: Reflect::get(&occurrence, &JsValue::from_str("invalid"))?.as_bool()
                == Some(true),
        });
    }
    Ok(InputDecorationState {
        draft,
        phase,
        claim,
        occurrences,
    })
}

fn parse_lexicon(value: &JsValue) -> Result<ReferenceLexicon, JsValue> {
    if value.is_null() || value.is_undefined() {
        return Ok(BTreeMap::new());
    }
    let get = required_function(value, "get", "reference lexicon")?;
    let mut lexicon = BTreeMap::new();
    for (trigger, key) in [(ReferenceTrigger::Slash, "/"), (ReferenceTrigger::At, "@")] {
        let names = get.call1(value, &JsValue::from_str(key))?;
        if names.is_undefined() {
            continue;
        }
        if !Array::is_array(&names) {
            return Err(js_sys::TypeError::new("reference lexicon values must be arrays").into());
        }
        let names = names.dyn_into::<Array>()?;
        let mut parsed = Vec::new();
        for index in 0..names.length() {
            parsed.push(names.get(index).as_string().ok_or_else(|| {
                js_sys::TypeError::new("reference lexicon names must be strings")
            })?);
        }
        lexicon.insert(trigger, parsed);
    }
    Ok(lexicon)
}

fn text_refs_value(ranges: &[crate::TextRefRange]) -> Result<Array, JsValue> {
    let values = Array::new();
    for range in ranges {
        values.push(
            object(&[
                ("start", JsValue::from_f64(f64::from(range.start))),
                ("end", JsValue::from_f64(f64::from(range.end))),
                (
                    "trigger",
                    JsValue::from_str(&range.trigger.as_char().to_string()),
                ),
            ])?
            .as_ref(),
        );
    }
    Ok(values)
}

fn number_to_u32(value: f64, owner: &str) -> Result<u32, JsValue> {
    number_string(value)?
        .parse::<u32>()
        .map_err(|_| js_sys::RangeError::new(&format!("{owner} must be a u32")).into())
}

fn number_to_u64(value: f64, owner: &str) -> Result<u64, JsValue> {
    number_string(value)?
        .parse::<u64>()
        .map_err(|_| js_sys::RangeError::new(&format!("{owner} must be a u64")).into())
}

fn u64_to_f64(value: u64) -> Result<f64, JsValue> {
    value.to_string().parse::<f64>().map_err(|_| {
        js_sys::RangeError::new("occurrence id cannot be represented as number").into()
    })
}

fn number_string(value: f64) -> Result<String, JsValue> {
    js_sys::Number::from(value)
        .to_string_with_radix(10)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("Number.toString() returned non-string").into())
}

fn numeric_property(value: &JsValue, key: &str, owner: &str) -> Result<f64, JsValue> {
    required_property(value, key, owner)?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be number")).into())
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required_property(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be string")).into())
}

fn required_function(value: &JsValue, key: &str, owner: &str) -> Result<Function, JsValue> {
    required_property(value, key, owner)?.dyn_into()
}

fn required_property(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Err(js_sys::Error::new(&format!("{owner} omitted {key}")).into())
    } else {
        Ok(property)
    }
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        Reflect::set(&object, &JsValue::from_str(key), value)?;
    }
    Ok(object)
}
