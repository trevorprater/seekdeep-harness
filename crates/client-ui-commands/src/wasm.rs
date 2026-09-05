//! Browser command directory, runtime service, popup controller, and popup view.

mod browser_directory;
mod browser_popup;
mod plugin;
mod popup_view;
mod service;

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};

use crate::POPUP_VIEW_STYLES;

pub use browser_directory::*;
pub use browser_popup::*;
pub use service::*;

pub(crate) const INJECT: &[&str] = &[
    "inputTriggers",
    "sessions",
    "remote",
    "remote.commands",
    "locale",
];

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
pub(crate) struct BrowserModules {
    pub(crate) react: JsValue,
    pub(crate) anchored_max_height: Function,
    pub(crate) check: JsValue,
    pub(crate) risk_confirmation: JsValue,
}

/// Configures React, UI primitives, and the compiled stylesheet.
///
/// # Errors
///
/// Returns missing primitive or DOM stylesheet-injection failures.
#[wasm_bindgen(js_name = configureClientUiCommands)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_commands(react: JsValue, primitives: JsValue) -> Result<(), JsValue> {
    MODULES.with(|modules| {
        *modules.borrow_mut() = Some(BrowserModules {
            react,
            anchored_max_height: required_function(
                &primitives,
                "useAnchoredMaxHeight",
                "UI primitives",
            )?,
            check: required(&primitives, "IconCheckOutline16", "UI primitives")?,
            risk_confirmation: required(&primitives, "RiskConfirmation", "UI primitives")?,
        });
        Ok::<_, JsValue>(())
    })?;
    inject_styles()
}

/// Applies the command UI browser plugin.
///
/// # Errors
///
/// Returns missing service, remote, locale, scope, Slot, or component failures.
#[wasm_bindgen(js_name = applyClientUiCommands)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_client_ui_commands(ctx: JsValue) -> Result<(), JsValue> {
    plugin::apply(&configured_modules()?, &ctx)
}

/// Returns the exact browser dependency order.
#[wasm_bindgen(js_name = commandsInject)]
pub fn commands_inject() -> Array {
    let values = Array::new();
    for dependency in INJECT {
        values.push(&JsValue::from_str(dependency));
    }
    values
}

/// Returns the compiled `PopupSelectView` component.
///
/// # Errors
///
/// Returns before browser modules are configured.
#[wasm_bindgen(js_name = popupSelectViewComponent)]
pub fn exported_popup_select_view_component() -> Result<JsValue, JsValue> {
    Ok(popup_view::component(&configured_modules()?))
}

/// Filters popup options with source-compatible identity semantics.
///
/// # Errors
///
/// Returns malformed option rows.
#[wasm_bindgen(js_name = filterOptions)]
#[allow(clippy::needless_pass_by_value)]
pub fn exported_filter_options(options: JsValue, search: String) -> Result<JsValue, JsValue> {
    if search.trim().is_empty() {
        return Ok(options);
    }
    let parsed: Vec<crate::SelectOption> = serde_wasm_bindgen::from_value(options.clone())
        .map_err(|error| js_sys::Error::new(&error.to_string()))?;
    let source = Array::from(&options);
    let output = Array::new();
    for index in crate::filtered_option_indices(&parsed, &search) {
        output.push(&source.get(usize_as_u32(index)?));
    }
    Ok(output.into())
}

pub(crate) fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules
            .borrow()
            .clone()
            .ok_or_else(|| js_sys::Error::new("client-ui-commands is not configured").into())
    })
}

fn usize_as_u32(value: usize) -> Result<u32, JsValue> {
    u32::try_from(value).map_err(|_| {
        js_sys::Error::new("popup option index exceeds JavaScript array limits").into()
    })
}

fn inject_styles() -> Result<(), JsValue> {
    const PACKAGE: &str = "@seekdeep-ai/seekdeep-client-ui-commands";
    let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))?;
    if document.is_null() || document.is_undefined() {
        return Ok(());
    }
    let selector = format!("style[data-plugin=\"{PACKAGE}\"]");
    if !call_method(&document, "querySelector", &[JsValue::from_str(&selector)])?.is_null() {
        return Ok(());
    }
    let style = call_method(&document, "createElement", &[JsValue::from_str("style")])?;
    call_method(
        &style,
        "setAttribute",
        &[JsValue::from_str("data-plugin"), JsValue::from_str(PACKAGE)],
    )?;
    Reflect::set(
        &style,
        &JsValue::from_str("textContent"),
        &JsValue::from_str(POPUP_VIEW_STYLES),
    )?;
    let head = required(&document, "head", "document")?;
    call_method(&head, "appendChild", &[style])?;
    Ok(())
}

pub(crate) fn translated(translate: &Function, key: &str) -> Result<JsValue, JsValue> {
    translate.call1(&JsValue::UNDEFINED, &JsValue::from_str(key))
}

pub(crate) fn translated_values(
    translate: &Function,
    key: &str,
    values: &[(&str, JsValue)],
) -> Result<JsValue, JsValue> {
    translate.call2(
        &JsValue::UNDEFINED,
        &JsValue::from_str(key),
        &object(values)?.into(),
    )
}

pub(crate) fn fragment(react: &JsValue, children: &[JsValue]) -> Result<JsValue, JsValue> {
    let fragment = required(react, "Fragment", "React")?;
    element(react, &fragment, None, children)
}

pub(crate) fn tag(
    react: &JsValue,
    name: &str,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    element(react, &JsValue::from_str(name), props, children)
}

pub(crate) fn component(
    react: &JsValue,
    component: &JsValue,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    element(react, component, props, children)
}

fn element(
    react: &JsValue,
    kind: &JsValue,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    let arguments = Array::new();
    arguments.push(kind);
    arguments.push(props.map_or(&JsValue::NULL, AsRef::as_ref));
    for child in children {
        arguments.push(child);
    }
    required_function(react, "createElement", "React")?.apply(react, &arguments)
}

pub(crate) fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    required_function(react, "useRef", "React")?.call1(react, initial)
}

pub(crate) fn use_effect(
    react: &JsValue,
    effect: &JsValue,
    dependencies: &Array,
) -> Result<(), JsValue> {
    required_function(react, "useEffect", "React")?
        .call2(react, effect, dependencies)
        .map(|_| ())
}

pub(crate) fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let value = Object::new();
    for (key, entry) in entries {
        set(&value, key, entry)?;
    }
    Ok(value)
}

pub(crate) fn set(value: &Object, key: &str, entry: &JsValue) -> Result<(), JsValue> {
    Reflect::set(value, &JsValue::from_str(key), entry).map(|_| ())
}

pub(crate) fn required(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let entry = Reflect::get(value, &JsValue::from_str(key))?;
    if entry.is_null() || entry.is_undefined() {
        Err(js_sys::Error::new(&format!("{owner} omitted required property {key:?}")).into())
    } else {
        Ok(entry)
    }
}

pub(crate) fn optional(value: &JsValue, key: &str) -> Result<Option<JsValue>, JsValue> {
    let entry = Reflect::get(value, &JsValue::from_str(key))?;
    Ok((!entry.is_null() && !entry.is_undefined()).then_some(entry))
}

pub(crate) fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::Error::new(&format!("{owner} {key:?} must be a string")).into())
}

pub(crate) fn required_function(
    value: &JsValue,
    key: &str,
    owner: &str,
) -> Result<Function, JsValue> {
    required(value, key, owner)?.dyn_into()
}

pub(crate) fn call_method(
    value: &JsValue,
    name: &str,
    arguments: &[JsValue],
) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let values = Array::new();
    for argument in arguments {
        values.push(argument);
    }
    method.apply(value, &values)
}

pub(crate) fn js_error_string(value: &JsValue) -> String {
    Reflect::get(&js_sys::global(), &JsValue::from_str("String"))
        .ok()
        .and_then(|value| value.dyn_into::<Function>().ok())
        .and_then(|string| string.call1(&JsValue::UNDEFINED, value).ok())
        .and_then(|value| value.as_string())
        .unwrap_or_else(|| "unknown JavaScript error".to_owned())
}

pub(crate) fn js_error_text(value: &JsValue) -> String {
    if value.is_instance_of::<js_sys::Error>() {
        Reflect::get(value, &JsValue::from_str("message"))
            .ok()
            .and_then(|message| message.as_string())
            .unwrap_or_else(|| js_error_string(value))
    } else {
        js_error_string(value)
    }
}

pub(crate) fn to_js<T: serde::Serialize>(value: &T) -> Result<JsValue, JsValue> {
    value
        .serialize(
            &serde_wasm_bindgen::Serializer::new().serialize_large_number_types_as_bigints(false),
        )
        .map_err(|error| js_sys::Error::new(&error.to_string()).into())
}
