//! Rust/WASM React components for the generated SVG glyph catalog.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{ICON_DEFINITIONS, IconDefinition};

const BRAND_NAME: &str = "BrandWordmark";

thread_local! {
    static REACT: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

/// Configures React for compiled SVG components.
#[wasm_bindgen(js_name = configureClientUiPrimitiveIcons)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_primitive_icons(react: JsValue) {
    REACT.with(|slot| *slot.borrow_mut() = Some(react));
}

/// Returns every icon and product glyph keyed by its source export name.
///
/// # Errors
///
/// Returns missing React configuration or JavaScript object failures.
#[wasm_bindgen(js_name = iconComponents)]
pub fn icon_components() -> Result<Object, JsValue> {
    let react = configured_react()?;
    let output = Object::new();
    for definition in ICON_DEFINITIONS {
        let definition = *definition;
        let component_react = react.clone();
        let component = Closure::wrap(Box::new(move |props: JsValue| {
            render_icon(&component_react, definition, &props)
        })
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
        Reflect::set(
            &output,
            &JsValue::from_str(definition.name),
            &component.into_js_value(),
        )?;
    }
    Ok(output)
}

pub(crate) fn render_icon(
    react: &JsValue,
    definition: IconDefinition,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let size = optional_number(props, "size")?.unwrap_or(definition.default_size);
    let class_name = Reflect::get(props, &JsValue::from_str("className"))?;
    let renamed_brand;
    let inner_html = if definition.name == BRAND_NAME {
        renamed_brand = seekdeep_brand_inner()?;
        renamed_brand.as_str()
    } else {
        definition.inner_html
    };
    let inner = object(&[("__html", JsValue::from_str(inner_html))])?;
    let svg = object(&[
        ("width", JsValue::from_f64(size * definition.width_factor)),
        ("height", JsValue::from_f64(size * definition.height_factor)),
        ("className", class_name),
        ("viewBox", JsValue::from_str(definition.view_box)),
        ("fill", JsValue::from_str("none")),
        ("xmlns", JsValue::from_str("http://www.w3.org/2000/svg")),
        (
            "aria-hidden",
            if definition.aria_hidden {
                JsValue::TRUE
            } else {
                JsValue::UNDEFINED
            },
        ),
        ("dangerouslySetInnerHTML", inner.into()),
    ])?;
    let arguments = Array::of2(&JsValue::from_str("svg"), &svg);
    function(react, "createElement")?.apply(react, &arguments)
}

fn seekdeep_brand_inner() -> Result<String, JsValue> {
    let fish = ICON_DEFINITIONS
        .iter()
        .find(|definition| definition.name == "FishLogo")
        .ok_or_else(|| js_sys::Error::new("FishLogo is missing from the glyph catalog"))?;
    Ok(format!(
        r#"<g transform="translate(0.15 3.5)">{}</g><text x="28" y="17.2" fill="currentColor" font-family="ui-sans-serif,system-ui,-apple-system,sans-serif" font-size="15" font-weight="650" letter-spacing="-0.4">seekdeep</text><rect x="112" y="5" width="69" height="14" rx="2" fill="currentColor"></rect><text x="117" y="15" fill="var(--dsw-alias-label-primary-inverted)" font-family="ui-sans-serif,system-ui,-apple-system,sans-serif" font-size="7.5" font-weight="700" letter-spacing="0.55">HARNESS</text>"#,
        fish.inner_html
    ))
}

fn configured_react() -> Result<JsValue, JsValue> {
    REACT.with(|slot| {
        slot.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-primitives icon module was not configured").into()
        })
    })
}

fn optional_number(value: &JsValue, key: &str) -> Result<Option<f64>, JsValue> {
    let value = Reflect::get(value, &JsValue::from_str(key))?;
    if value.is_null() || value.is_undefined() {
        Ok(None)
    } else {
        value
            .as_f64()
            .map(Some)
            .ok_or_else(|| js_sys::TypeError::new("icon size must be a number").into())
    }
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        Reflect::set(&object, &JsValue::from_str(key), value)?;
    }
    Ok(object)
}

fn function(value: &JsValue, key: &str) -> Result<Function, JsValue> {
    Reflect::get(value, &JsValue::from_str(key))?.dyn_into::<Function>()
}
