//! Structural DOM snapshot normalization shared by Rust and browser tests.

/// Folds CSS-module tokens shaped as `_<local>_<lowercase-hash>` to `<local>`.
#[must_use]
pub fn normalize_snapshot_class_value(value: &str) -> String {
    value
        .split_whitespace()
        .map(normalize_class_token)
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_class_token(token: &str) -> &str {
    let Some(without_prefix) = token.strip_prefix('_') else {
        return token;
    };
    let Some((local, hash)) = without_prefix.rsplit_once('_') else {
        return token;
    };
    if local.is_empty()
        || hash.is_empty()
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        token
    } else {
        local
    }
}

/// Source-compatible FNV-1a fingerprint over JavaScript UTF-16 code units.
#[must_use]
pub fn snapshot_markup_fingerprint(markup: &str) -> String {
    let mut hash = 0x811c_9dc5_u32;
    for unit in markup.encode_utf16() {
        hash ^= u32::from(unit);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{hash:08x}")
}

#[cfg(target_arch = "wasm32")]
mod browser {
    use js_sys::{Array, Function, Reflect};
    use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};

    use super::{normalize_snapshot_class_value, snapshot_markup_fingerprint};

    /// Whether a DOM subtree carries a scoped class or nonempty SVG body.
    ///
    /// # Errors
    ///
    /// Returns malformed DOM method or property failures.
    #[wasm_bindgen(js_name = snapshotNeedsNormalization)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn snapshot_needs_normalization(root: JsValue) -> Result<bool, JsValue> {
        let mut class_elements = vec![root.clone()];
        class_elements.extend(query(&root, "[class]")?.iter());
        for element in class_elements {
            let class_name = call(&element, "getAttribute", &[JsValue::from_str("class")])?;
            if let Some(class_name) = class_name.as_string()
                && class_name
                    .split_whitespace()
                    .any(|token| normalize_snapshot_class_value(token) != token)
            {
                return Ok(true);
            }
        }
        Ok(svgs_of(&root)?.iter().any(|svg| has_children(&svg)))
    }

    /// Clones one DOM subtree, folds scoped classes, and fingerprints nonempty SVG bodies.
    ///
    /// The input tree is never mutated.
    ///
    /// # Errors
    ///
    /// Returns malformed DOM method or property failures.
    #[wasm_bindgen(js_name = normalizeDomSnapshot)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn normalize_dom_snapshot(root: JsValue) -> Result<JsValue, JsValue> {
        let clone = call(&root, "cloneNode", &[JsValue::TRUE])?;
        let mut class_elements = vec![clone.clone()];
        class_elements.extend(query(&clone, "[class]")?.iter());
        for element in class_elements {
            let class_name = call(&element, "getAttribute", &[JsValue::from_str("class")])?;
            if let Some(class_name) = class_name.as_string() {
                call(
                    &element,
                    "setAttribute",
                    &[
                        JsValue::from_str("class"),
                        JsValue::from_str(&normalize_snapshot_class_value(&class_name)),
                    ],
                )?;
            }
        }
        for svg in svgs_of(&clone)? {
            if !has_children(&svg) {
                continue;
            }
            let markup = Reflect::get(&svg, &JsValue::from_str("innerHTML"))?
                .as_string()
                .unwrap_or_default();
            call(
                &svg,
                "setAttribute",
                &[
                    JsValue::from_str("data-content"),
                    JsValue::from_str(&snapshot_markup_fingerprint(&markup)),
                ],
            )?;
            call(&svg, "replaceChildren", &[])?;
        }
        Ok(clone)
    }

    fn svgs_of(root: &JsValue) -> Result<Array, JsValue> {
        let output = query(root, "svg")?;
        let tag = Reflect::get(root, &JsValue::from_str("tagName"))?
            .as_string()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if tag == "svg" {
            output.unshift(root);
        }
        Ok(output)
    }

    fn has_children(value: &JsValue) -> bool {
        Reflect::get(value, &JsValue::from_str("childNodes"))
            .and_then(|children| Reflect::get(&children, &JsValue::from_str("length")))
            .ok()
            .and_then(|length| length.as_f64())
            .is_some_and(|length| length > 0.0)
    }

    fn query(value: &JsValue, selector: &str) -> Result<Array, JsValue> {
        Ok(Array::from(&call(
            value,
            "querySelectorAll",
            &[JsValue::from_str(selector)],
        )?))
    }

    fn call(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
        let function = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
        let arguments: Array = arguments.iter().cloned().collect();
        function.apply(value, &arguments)
    }
}

#[cfg(target_arch = "wasm32")]
pub use browser::*;
