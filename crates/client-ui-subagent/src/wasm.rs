//! Browser subagent catalog, read-only composer, reference source, and Client plugin.

mod catalog_action;
mod plugin;

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{SUBAGENT_CATALOG_STYLES, SUBAGENT_READ_ONLY_STYLES};

pub(crate) const INJECT: &[&str] = &["inputTriggers", "sessions", "slots", "locale"];

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
pub(crate) struct BrowserModules {
    pub(crate) react: JsValue,
    pub(crate) chevron_down: JsValue,
    pub(crate) chevron_right: JsValue,
    pub(crate) refresh: JsValue,
    pub(crate) state_dot: JsValue,
}

/// Configures React, UI primitives, and the compiled stylesheets.
///
/// # Errors
///
/// Returns missing primitive or DOM stylesheet-injection failures.
#[wasm_bindgen(js_name = configureClientUiSubagent)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_subagent(react: JsValue, primitives: JsValue) -> Result<(), JsValue> {
    MODULES.with(|modules| {
        *modules.borrow_mut() = Some(BrowserModules {
            react,
            chevron_down: required(&primitives, "IconChevronDownOutline14", "UI primitives")?,
            chevron_right: required(&primitives, "IconChevronRightOutline14", "UI primitives")?,
            refresh: required(&primitives, "IconRefreshOutline14", "UI primitives")?,
            state_dot: required(&primitives, "StateDot", "UI primitives")?,
        });
        Ok::<_, JsValue>(())
    })?;
    inject_styles()
}

/// Applies the subagent browser plugin.
///
/// # Errors
///
/// Returns missing service, registration, source, or component failures.
#[wasm_bindgen(js_name = applyClientUiSubagent)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_client_ui_subagent(ctx: JsValue) -> Result<(), JsValue> {
    plugin::apply(&configured_modules()?, &ctx)
}

/// Returns the exact browser dependency order.
#[wasm_bindgen(js_name = subagentInject)]
pub fn subagent_inject() -> Array {
    let values = Array::new();
    for dependency in INJECT {
        values.push(&JsValue::from_str(dependency));
    }
    values
}

/// Returns the compiled `SubagentCatalogAction` component.
///
/// # Errors
///
/// Returns before browser modules are configured.
#[wasm_bindgen(js_name = subagentCatalogActionComponent)]
pub fn exported_subagent_catalog_action_component() -> Result<JsValue, JsValue> {
    Ok(catalog_action::component(&configured_modules()?))
}

/// Returns the compiled `SubagentReadOnlyComposer` component.
///
/// # Errors
///
/// Returns before browser modules are configured.
#[wasm_bindgen(js_name = subagentReadOnlyComposerComponent)]
pub fn exported_subagent_read_only_composer_component() -> Result<JsValue, JsValue> {
    Ok(read_only_component(&configured_modules()?))
}

pub(crate) fn read_only_component(modules: &BrowserModules) -> JsValue {
    let modules = modules.clone();
    Closure::wrap(
        Box::new(move |props: JsValue| render_read_only(&modules, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value()
}

fn render_read_only(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let matched = required(props, "matched", "SubagentReadOnlyComposer")?;
    let one_shot =
        required_string(&matched, "reason", "SubagentReadOnlyComposer match")? == "one-shot";
    let translate = required_function(props, "t", "SubagentReadOnlyComposer")?;
    let title = translated(
        &translate,
        if one_shot {
            "readonly.oneShot.title"
        } else {
            "readonly.title"
        },
    )?;
    let body = translated(
        &translate,
        if one_shot {
            "readonly.oneShot.body"
        } else {
            "readonly.body"
        },
    )?;
    tag(
        &modules.react,
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-subagent-readonly-frame"),
            ),
            ("role", JsValue::from_str("status")),
        ])?),
        &[
            tag(&modules.react, "strong", None, &[title])?,
            tag(&modules.react, "span", None, &[body])?,
        ],
    )
}

fn inject_styles() -> Result<(), JsValue> {
    const PACKAGE: &str = "@seekdeep-ai/seekdeep-client-ui-subagent";
    let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))?;
    if document.is_null() || document.is_undefined() {
        return Ok(());
    }
    let selector = format!(
        "style[data-plugin={}]",
        serde_json::to_string(PACKAGE).unwrap()
    );
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
        &JsValue::from_str(&format!(
            "{SUBAGENT_CATALOG_STYLES}\n{SUBAGENT_READ_ONLY_STYLES}"
        )),
    )?;
    let head = required(&document, "head", "document")?;
    call_method(&head, "appendChild", &[style])?;
    Ok(())
}

pub(crate) fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules
            .borrow()
            .clone()
            .ok_or_else(|| js_sys::Error::new("client-ui-subagent is not configured").into())
    })
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
    let args = Array::new();
    args.push(kind);
    args.push(props.map_or(&JsValue::NULL, AsRef::as_ref));
    for child in children {
        args.push(child);
    }
    required_function(react, "createElement", "React")?.apply(react, &args)
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

pub(crate) fn optional_string(value: &JsValue, key: &str) -> Result<Option<String>, JsValue> {
    optional(value, key)?
        .map(|entry| {
            entry
                .as_string()
                .ok_or_else(|| js_sys::Error::new(&format!("{key:?} must be a string")).into())
        })
        .transpose()
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
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}

pub(crate) fn usize_as_f64(value: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}

pub(crate) fn f64_as_i128(value: f64) -> i128 {
    #[allow(clippy::cast_possible_truncation)]
    {
        value as i128
    }
}
