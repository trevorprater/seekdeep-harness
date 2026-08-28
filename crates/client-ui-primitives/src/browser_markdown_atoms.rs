//! Compiled literal-message and collapsible JSON primitives.

use std::cell::RefCell;

use js_sys::{Array, Function, JsString, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::browser_code_block::inject_namespaced_style;

const JSON_BLOCK_CSS: &str =
    include_str!("../../../packages/client/ui-primitives/src/markdown/JsonBlock.module.css");
const MESSAGE_TEXT_CSS: &str =
    include_str!("../../../packages/client/ui-primitives/src/markdown/MessageText.module.css");
const MAX_JSON_CHARS: u32 = 20_000;

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    default_truncated_label: Function,
}

/// Configures React and the stable default footer formatter for the two primitives.
///
/// # Errors
///
/// Returns on missing React methods or stylesheet injection failures.
#[wasm_bindgen(js_name = configureClientUiPrimitiveMarkdownAtoms)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_primitive_markdown_atoms(react: JsValue) -> Result<(), JsValue> {
    for method in ["createElement", "useMemo", "useState"] {
        required_function(&react, method, "React")?;
    }
    inject_json_style()?;
    inject_message_style()?;
    let default_truncated_label =
        Closure::wrap(Box::new(default_truncated_label) as Box<dyn FnMut(f64) -> JsValue>)
            .into_js_value()
            .dyn_into()?;
    MODULES.with(|modules| {
        *modules.borrow_mut() = Some(BrowserModules {
            react,
            default_truncated_label,
        });
    });
    Ok(())
}

/// Returns the compiled `JsonBlock` component.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = jsonBlockComponent)]
pub fn json_block_component() -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    Ok(Closure::wrap(
        Box::new(move |props: JsValue| render_json_block(&modules, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value())
}

/// Returns the compiled `MessageText` component.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = messageTextComponent)]
pub fn message_text_component() -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    Ok(Closure::wrap(
        Box::new(move |props: JsValue| render_message_text(&modules.react, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value())
}

fn render_message_text(react: &JsValue, props: &JsValue) -> Result<JsValue, JsValue> {
    let text = required_string(props, "text", "MessageText props")?;
    create_element(
        react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-primitive-message-text-text")?),
        &[JsValue::from_str(&text)],
    )
}

fn render_json_block(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let react = &modules.react;
    let label = required_string(props, "label", "JsonBlock props")?;
    let payload = Reflect::get(props, &JsValue::from_str("payload"))?;
    let default_open = Reflect::get(props, &JsValue::from_str("defaultOpen"))?;
    let initial_open = if default_open.is_undefined() {
        JsValue::FALSE
    } else {
        default_open
    };
    let truncated_label = Reflect::get(props, &JsValue::from_str("truncatedLabel"))?;
    let truncated_label = if truncated_label.is_undefined() {
        modules.default_truncated_label.clone().into()
    } else {
        truncated_label
    };

    let (open_value, set_open) = use_state(react, &initial_open)?;
    let open = open_value.is_truthy();
    let body_payload = payload.clone();
    let body_label = truncated_label.clone();
    let body_factory = Closure::wrap(Box::new(move || {
        if !open {
            return Ok(JsValue::from_str(""));
        }
        stringify_payload(&body_payload, &body_label)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let dependencies = Array::new();
    dependencies.push(&open_value);
    dependencies.push(&payload);
    dependencies.push(&truncated_label);
    let body = required_function(react, "useMemo", "React")?.call2(
        react,
        &body_factory.into_js_value(),
        &dependencies,
    )?;

    let toggle_setter = set_open;
    let on_toggle = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let updater =
            Closure::wrap(
                Box::new(move |value: JsValue| JsValue::from_bool(value.is_falsy()))
                    as Box<dyn FnMut(JsValue) -> JsValue>,
            );
        toggle_setter
            .call1(&JsValue::UNDEFINED, &updater.into_js_value())
            .map(|_| ())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let toggle = create_element(
        react,
        &JsValue::from_str("button"),
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str("seekdeep-primitive-json-block-toggle"),
            ),
            ("onClick", on_toggle.into_js_value()),
        ])?),
        &[
            JsValue::from_str(if open { "▾" } else { "▸" }),
            JsValue::from_str(" "),
            JsValue::from_str(&label),
        ],
    )?;
    let mut children = vec![toggle];
    if open {
        children.push(create_element(
            react,
            &JsValue::from_str("pre"),
            Some(&class_props("seekdeep-primitive-json-block-body")?),
            &[body],
        )?);
    }
    create_element(
        react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-primitive-json-block-root")?),
        &children,
    )
}

fn stringify_payload(payload: &JsValue, truncated_label: &JsValue) -> Result<JsValue, JsValue> {
    let global = js_sys::global();
    let json = required_property(&global, "JSON", "global")?;
    let serialized = call_method(
        &json,
        "stringify",
        &[payload.clone(), JsValue::NULL, JsValue::from_f64(2.0)],
    );
    let value = match serialized {
        Ok(value) if !value.is_undefined() => value,
        Ok(_) | Err(_) => {
            required_function(&global, "String", "global")?.call1(&JsValue::UNDEFINED, payload)?
        }
    };
    let value: JsString = value.unchecked_into();
    let length = value.length();
    if length <= MAX_JSON_CHARS {
        return Ok(value.into());
    }
    let footer = truncated_label
        .clone()
        .dyn_into::<Function>()?
        .call1(&JsValue::UNDEFINED, &JsValue::from_f64(f64::from(length)))?;
    if !footer.is_string() {
        return Err(js_sys::TypeError::new("truncatedLabel must return a string").into());
    }
    let parts = Array::of3(
        value.slice(0, MAX_JSON_CHARS).as_ref(),
        &JsValue::from_str("\n"),
        &footer,
    );
    Ok(parts.join("").into())
}

fn default_truncated_label(total: f64) -> JsValue {
    JsValue::from_str(&format!("… 已截断，共 {total} 字符"))
}

fn inject_json_style() -> Result<(), JsValue> {
    inject_namespaced_style(
        "JsonBlock",
        JSON_BLOCK_CSS,
        &[
            ("toggle", "seekdeep-primitive-json-block-toggle"),
            ("root", "seekdeep-primitive-json-block-root"),
            ("body", "seekdeep-primitive-json-block-body"),
        ],
    )
}

fn inject_message_style() -> Result<(), JsValue> {
    inject_namespaced_style(
        "MessageText",
        MESSAGE_TEXT_CSS,
        &[("text", "seekdeep-primitive-message-text-text")],
    )
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-primitives Markdown atoms were not configured").into()
        })
    })
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let pair = Array::from(&required_function(react, "useState", "React")?.call1(react, initial)?);
    Ok((pair.get(0), pair.get(1).dyn_into()?))
}

fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required_property(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a string")).into())
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

fn create_element(
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

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let arguments: Array = arguments.iter().collect();
    method.apply(value, &arguments)
}
