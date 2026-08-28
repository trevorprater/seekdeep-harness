//! Browser model directory, resolver, command/Slot plugin, and composer selector.

mod browser_directory;
mod model_select;
mod plugin;
mod resolver;

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::MODEL_SELECT_STYLES;

pub use browser_directory::*;

pub(crate) const INJECT: &[&str] = &[
    "commandUi",
    "connection",
    "locale",
    "sessions",
    "slots",
    "remote",
];

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
pub(crate) struct BrowserModules {
    pub(crate) react: JsValue,
    pub(crate) check: JsValue,
    pub(crate) chevron_down: JsValue,
    pub(crate) chevron_right: JsValue,
    pub(crate) warning: JsValue,
    pub(crate) toast: JsValue,
}

/// Configures React, UI primitives, and the compiled stylesheet.
///
/// # Errors
///
/// Returns missing primitive or DOM stylesheet-injection failures.
#[wasm_bindgen(js_name = configureClientUiModelSelection)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_model_selection(
    react: JsValue,
    primitives: JsValue,
) -> Result<(), JsValue> {
    MODULES.with(|modules| {
        *modules.borrow_mut() = Some(BrowserModules {
            react,
            check: required(&primitives, "IconCheckOutline16", "UI primitives")?,
            chevron_down: required(&primitives, "IconChevronDownOutline14", "UI primitives")?,
            chevron_right: required(&primitives, "IconChevronRightOutline14", "UI primitives")?,
            warning: required(&primitives, "IconWarningOutline16", "UI primitives")?,
            toast: required(&primitives, "Toast", "UI primitives")?,
        });
        Ok::<_, JsValue>(())
    })?;
    inject_styles()
}

/// Applies the model-selection browser plugin.
///
/// # Errors
///
/// Returns missing service, scope, resolver, registration, or component failures.
#[wasm_bindgen(js_name = applyClientUiModelSelection)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_client_ui_model_selection(ctx: JsValue) -> Result<(), JsValue> {
    plugin::apply(&configured_modules()?, &ctx)
}

/// Returns the exact browser dependency order.
#[wasm_bindgen(js_name = modelSelectionInject)]
pub fn model_selection_inject() -> Array {
    let values = Array::new();
    for dependency in INJECT {
        values.push(&JsValue::from_str(dependency));
    }
    values
}

/// Returns the compiled `ModelSelect` component.
///
/// # Errors
///
/// Returns before browser modules are configured.
#[wasm_bindgen(js_name = modelSelectComponent)]
pub fn exported_model_select_component() -> Result<JsValue, JsValue> {
    Ok(model_select::component(&configured_modules()?))
}

/// Creates and provides a source-compatible model directory resolver.
///
/// # Errors
///
/// Returns missing service, event, scope, or provision failures.
#[wasm_bindgen(js_name = createModelDirectoryResolver)]
#[allow(clippy::needless_pass_by_value)]
pub fn create_model_directory_resolver(
    ctx: JsValue,
    block_reason: Function,
) -> Result<JsValue, JsValue> {
    let connection = required(&ctx, "connection", "Client Context")?;
    let sessions = required(&ctx, "sessions", "Client Context")?;
    let remote = required(&ctx, "remote", "Client Context")?;
    let api = required(&connection, "api", "connection")?;
    let sessions_api = required(&api, "sessions", "generated API")?;
    let translate =
        Closure::wrap(
            Box::new(move |_key: JsValue| block_reason.call0(&JsValue::UNDEFINED))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        )
        .into_js_value()
        .dyn_into::<Function>()?;
    let resolver = resolver::BrowserModelDirectoryResolver::new(
        ctx.clone(),
        sessions,
        sessions_api,
        translate,
    );
    plugin::own_resolver(&ctx, &remote, &resolver)?;
    resolver.face()
}

pub(crate) fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules
            .borrow()
            .clone()
            .ok_or_else(|| js_sys::Error::new("client-ui-model-selection is not configured").into())
    })
}

fn inject_styles() -> Result<(), JsValue> {
    const PACKAGE: &str = "@seekdeep-ai/seekdeep-client-ui-model-selection";
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
        &JsValue::from_str(MODEL_SELECT_STYLES),
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

pub(crate) fn use_state(
    react: &JsValue,
    initial: &JsValue,
) -> Result<(JsValue, Function), JsValue> {
    let pair = Array::from(&required_function(react, "useState", "React")?.call1(react, initial)?);
    Ok((pair.get(0), pair.get(1).dyn_into()?))
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

pub(crate) fn required_bool(value: &JsValue, key: &str, owner: &str) -> Result<bool, JsValue> {
    required(value, key, owner)?
        .as_bool()
        .ok_or_else(|| js_sys::Error::new(&format!("{owner} {key:?} must be a boolean")).into())
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
