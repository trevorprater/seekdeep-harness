//! Compiled General Settings row for busy-state Enter behavior.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::browser_reasoning::inject_style;

const ENTER_CSS: &str = include_str!(
    "../../../packages/client/ui-conversation/src/client/settings/EnterBehaviorRow.module.css"
);

thread_local! {
    static COMPONENT: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    menu: JsValue,
    chevron: JsValue,
}

/// Configures the compiled busy-state Enter preference row.
///
/// # Errors
///
/// Returns on missing React/ui-primitives faces or stylesheet failure.
#[wasm_bindgen(js_name = configureClientUiConversationEnterBehavior)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_conversation_enter_behavior(
    react: JsValue,
    ui_primitives: JsValue,
) -> Result<(), JsValue> {
    for method in ["createElement", "useState"] {
        required_function(&react, method, "React")?;
    }
    let modules = BrowserModules {
        menu: required_property(&ui_primitives, "Menu", "ui-primitives")?,
        chevron: required_property(&ui_primitives, "IconChevronDownOutline14", "ui-primitives")?,
        react,
    };
    inject_style(
        "EnterBehaviorRow",
        ENTER_CSS,
        &[
            ("chevron", "seekdeep-conversation-enterBehavior-chevron"),
            ("desc", "seekdeep-conversation-enterBehavior-desc"),
            ("row", "seekdeep-conversation-enterBehavior-row"),
            ("rowText", "seekdeep-conversation-enterBehavior-rowText"),
            ("selector", "seekdeep-conversation-enterBehavior-selector"),
            ("title", "seekdeep-conversation-enterBehavior-title"),
        ],
    )?;
    let component =
        Closure::wrap(
            Box::new(move |props: JsValue| render_enter_behavior(&modules, &props))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        )
        .into_js_value();
    COMPONENT.with(|configured| *configured.borrow_mut() = Some(component));
    Ok(())
}

/// Returns the compiled `EnterBehaviorRow` component.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = enterBehaviorRowComponent)]
pub fn enter_behavior_row_component() -> Result<JsValue, JsValue> {
    COMPONENT.with(|component| {
        component.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation EnterBehaviorRow was not configured").into()
        })
    })
}

#[allow(clippy::too_many_lines)] // Closed preference row and Menu prop order stay together.
fn render_enter_behavior(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let selector =
        Closure::wrap(Box::new(move |value: JsValue| value) as Box<dyn FnMut(JsValue) -> JsValue>);
    let behavior = required_function(props, "useBusyEnter", "EnterBehaviorRow props")?
        .call1(&JsValue::UNDEFINED, &selector.into_js_value())?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("busy Enter behavior must be a string"))?;
    let state = required_function(&modules.react, "useState", "React")?
        .call1(&modules.react, &JsValue::FALSE)?
        .dyn_into::<Array>()?;
    let open = state
        .get(0)
        .as_bool()
        .ok_or_else(|| js_sys::TypeError::new("EnterBehaviorRow open state must be a boolean"))?;
    let set_open = state.get(1).dyn_into::<Function>()?;
    let translate = required_function(props, "t", "EnterBehaviorRow props")?;
    let title = translate.call1(
        &JsValue::UNDEFINED,
        &JsValue::from_str("settings.enter.title"),
    )?;
    let description = translate.call1(
        &JsValue::UNDEFINED,
        &JsValue::from_str("settings.enter.description"),
    )?;
    let row_text = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-enterBehavior-rowText")?),
        &[
            create_element(
                &modules.react,
                &JsValue::from_str("div"),
                Some(&class_props("seekdeep-conversation-enterBehavior-title")?),
                &[title],
            )?,
            create_element(
                &modules.react,
                &JsValue::from_str("div"),
                Some(&class_props("seekdeep-conversation-enterBehavior-desc")?),
                &[description],
            )?,
        ],
    )?;
    let queue_label = translate.call1(
        &JsValue::UNDEFINED,
        &JsValue::from_str("settings.enter.queue"),
    )?;
    let steer_label = translate.call1(
        &JsValue::UNDEFINED,
        &JsValue::from_str("settings.enter.steer"),
    )?;
    let items = Array::of2(
        object(&[("id", JsValue::from_str("queue")), ("label", queue_label)])?.as_ref(),
        object(&[("id", JsValue::from_str("steer")), ("label", steer_label)])?.as_ref(),
    );
    let selected_label_key = if behavior == "queue" {
        "settings.enter.queue"
    } else {
        "settings.enter.steer"
    };
    let close_setter = set_open.clone();
    let on_close = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        close_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    let select_setter = set_open.clone();
    let set_busy = required_function(props, "setBusyEnter", "EnterBehaviorRow props")?;
    let on_select = Closure::wrap(Box::new(move |id: JsValue| -> Result<(), JsValue> {
        select_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        set_busy.call1(&JsValue::UNDEFINED, &id)?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
    .into_js_value();
    let toggle_setter = set_open;
    let toggle = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let invert = Closure::wrap(
            Box::new(move |value: JsValue| !value.as_bool().unwrap_or(false))
                as Box<dyn FnMut(JsValue) -> bool>,
        )
        .into_js_value();
        toggle_setter.call1(&JsValue::UNDEFINED, &invert)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    let anchor = create_element(
        &modules.react,
        &JsValue::from_str("button"),
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str("seekdeep-conversation-enterBehavior-selector"),
            ),
            ("aria-haspopup", JsValue::from_str("menu")),
            ("aria-expanded", JsValue::from_bool(open)),
            ("onClick", toggle),
        ])?),
        &[
            translate.call1(&JsValue::UNDEFINED, &JsValue::from_str(selected_label_key))?,
            create_element(
                &modules.react,
                &modules.chevron,
                Some(&object(&[(
                    "className",
                    JsValue::from_str("seekdeep-conversation-enterBehavior-chevron"),
                )])?),
                &[],
            )?,
        ],
    )?;
    let menu = create_element(
        &modules.react,
        &modules.menu,
        Some(&object(&[
            ("open", JsValue::from_bool(open)),
            ("onClose", on_close),
            ("items", items.into()),
            ("selectedId", JsValue::from_str(&behavior)),
            ("onSelect", on_select),
            ("align", JsValue::from_str("end")),
            ("portal", JsValue::TRUE),
            ("anchor", anchor),
        ])?),
        &[],
    )?;
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-enterBehavior-row")?),
        &[row_text, menu],
    )
}

fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
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
