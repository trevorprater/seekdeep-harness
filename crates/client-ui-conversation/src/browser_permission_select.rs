//! Compiled permission-preset selector and Full access confirmation.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::browser_reasoning::inject_style;

const PERMISSION_CSS: &str = include_str!(
    "../../../packages/client/ui-conversation/src/client/skeleton/PermissionSelect.module.css"
);
const FULL_ACCESS: &str = "danger-full-access";
const SHIELD_OUTLINE: &str = "M8.20554 0.899994L14.7901 3.36857V7.01026C14.7901 12 11.0466 14.2103 8.20554 15.3C5.36446 14.2103 1.62012 12 1.62012 7.01026V3.36857L8.20554 0.899994Z";

thread_local! {
    static COMPONENT: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    fragment: JsValue,
    menu: JsValue,
    risk_confirmation: JsValue,
    chevron: JsValue,
    read_only_glyph: JsValue,
    workspace_write_glyph: JsValue,
    full_access_glyph: JsValue,
}

/// Configures the compiled permission selector.
///
/// # Errors
///
/// Returns on missing React/ui-primitives faces, SVG construction, or stylesheet failure.
#[wasm_bindgen(js_name = configureClientUiConversationPermissionSelect)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_conversation_permission_select(
    react: JsValue,
    ui_primitives: JsValue,
) -> Result<(), JsValue> {
    for method in ["createElement", "useEffect", "useState"] {
        required_function(&react, method, "React")?;
    }
    let modules = BrowserModules {
        fragment: required_property(&react, "Fragment", "React")?,
        menu: required_property(&ui_primitives, "Menu", "ui-primitives")?,
        risk_confirmation: required_property(&ui_primitives, "RiskConfirmation", "ui-primitives")?,
        chevron: required_property(&ui_primitives, "IconChevronDownOutline14", "ui-primitives")?,
        read_only_glyph: read_only_glyph(&react)?,
        workspace_write_glyph: workspace_write_glyph(&react)?,
        full_access_glyph: full_access_glyph(&react)?,
        react,
    };
    inject_style(
        "PermissionSelect",
        PERMISSION_CSS,
        &[
            ("chevron", "seekdeep-conversation-permission-chevron"),
            (
                "chevronOpen",
                "seekdeep-conversation-permission-chevronOpen",
            ),
            ("trigger", "seekdeep-conversation-permission-trigger"),
            (
                "triggerIcon",
                "seekdeep-conversation-permission-triggerIcon",
            ),
            (
                "triggerLabel",
                "seekdeep-conversation-permission-triggerLabel",
            ),
        ],
    )?;
    let component =
        Closure::wrap(
            Box::new(move |props: JsValue| render_permission_select(&modules, &props))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        )
        .into_js_value();
    COMPONENT.with(|configured| *configured.borrow_mut() = Some(component));
    Ok(())
}

/// Returns the compiled `PermissionSelect` component.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = permissionSelectComponent)]
pub fn permission_select_component() -> Result<JsValue, JsValue> {
    COMPONENT.with(|component| {
        component.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation PermissionSelect was not configured").into()
        })
    })
}

#[allow(clippy::too_many_lines)] // Closed four-state selector and confirmation lifecycle stay together.
fn render_permission_select(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let (pick, set_pick) = use_state(&modules.react, &JsValue::NULL)?;
    let (open_value, set_open) = use_state(&modules.react, &JsValue::FALSE)?;
    let open = open_value
        .as_bool()
        .ok_or_else(|| js_sys::TypeError::new("permission open state must be a boolean"))?;
    let (confirmation, set_confirmation) = use_state(&modules.react, &JsValue::NULL)?;
    let (acknowledged_value, set_acknowledged) = use_state(&modules.react, &JsValue::FALSE)?;
    let acknowledged = acknowledged_value
        .as_bool()
        .ok_or_else(|| js_sys::TypeError::new("permission acknowledgement must be a boolean"))?;
    let value = Reflect::get(props, &JsValue::from_str("value"))?;
    let locked = Reflect::get(props, &JsValue::from_str("locked"))?
        .as_bool()
        .unwrap_or(false);
    install_reset_effect(
        &modules.react,
        locked,
        &value,
        &set_open,
        &set_acknowledged,
        &set_confirmation,
    )?;
    if value.is_undefined() {
        return Ok(JsValue::NULL);
    }
    let configured_current = required_string(&value, "currentValue", "PermissionSelect value")?;
    let current_value = pick
        .as_string()
        .unwrap_or_else(|| configured_current.clone());
    let options =
        required_property(&value, "options", "PermissionSelect value")?.dyn_into::<Array>()?;
    let current = find_option(&options, &current_value)?;
    let busy = !pick.is_null() || !confirmation.is_null();
    let items = menu_items(modules, &options)?;
    let submit = submit_callback(props, &set_pick)?;
    let choose = choose_callback(
        &set_open,
        &set_acknowledged,
        &set_confirmation,
        &submit,
        &configured_current,
    );
    let close_confirmation = close_confirmation_callback(&set_acknowledged, &set_confirmation);
    let confirm = confirm_callback(
        locked,
        acknowledged,
        &confirmation,
        &close_confirmation,
        &submit,
    );
    let close_setter = set_open.clone();
    let on_close = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        close_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    let toggle_setter = set_open;
    let toggle = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        toggle_setter.call1(&JsValue::UNDEFINED, &JsValue::from_bool(!open))?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    let display = if let Some(option) = current.as_ref() {
        option_label(option)?
    } else {
        display_name(&current_value)
    };
    let translate = required_function(props, "t", "PermissionSelect props")?;
    let aria_label = translate.apply(
        &JsValue::UNDEFINED,
        &Array::of2(
            &JsValue::from_str("input.accessMode"),
            object(&[("name", JsValue::from_str(&display))])?.as_ref(),
        ),
    )?;
    let current_glyph = permission_glyph(modules, &current_value);
    let icon = if let Some(glyph) = current_glyph.as_ref() {
        create_element(
            &modules.react,
            &JsValue::from_str("span"),
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str("seekdeep-conversation-permission-triggerIcon"),
                ),
                ("aria-hidden", JsValue::TRUE),
            ])?),
            std::slice::from_ref(glyph),
        )?
    } else {
        JsValue::FALSE
    };
    let description = if let Some(option) = current.as_ref() {
        Reflect::get(option, &JsValue::from_str("description"))?
    } else {
        JsValue::UNDEFINED
    };
    let anchor = create_element(
        &modules.react,
        &JsValue::from_str("button"),
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str("seekdeep-conversation-permission-trigger"),
            ),
            ("aria-label", aria_label),
            ("title", description),
            ("disabled", JsValue::from_bool(locked || busy)),
            ("onClick", toggle),
        ])?),
        &[
            icon,
            create_element(
                &modules.react,
                &JsValue::from_str("span"),
                Some(&object(&[(
                    "className",
                    JsValue::from_str("seekdeep-conversation-permission-triggerLabel"),
                )])?),
                &[JsValue::from_str(&display)],
            )?,
            create_element(
                &modules.react,
                &JsValue::from_str("span"),
                Some(&object(&[
                    (
                        "className",
                        JsValue::from_str(if open {
                            "seekdeep-conversation-permission-chevron seekdeep-conversation-permission-chevronOpen"
                        } else {
                            "seekdeep-conversation-permission-chevron"
                        }),
                    ),
                    ("aria-hidden", JsValue::TRUE),
                ])?),
                &[create_element(&modules.react, &modules.chevron, None, &[])?],
            )?,
        ],
    )?;
    let menu = create_element(
        &modules.react,
        &modules.menu,
        Some(&object(&[
            ("open", JsValue::from_bool(open)),
            ("items", items.into()),
            ("selectedId", JsValue::from_str(&current_value)),
            ("onSelect", choose),
            ("onClose", on_close),
            ("side", JsValue::from_str("top")),
            ("anchor", anchor),
        ])?),
        &[],
    )?;
    let risk = create_element(
        &modules.react,
        &modules.risk_confirmation,
        Some(&object(&[
            ("open", JsValue::from_bool(!confirmation.is_null())),
            (
                "title",
                translate.call1(
                    &JsValue::UNDEFINED,
                    &JsValue::from_str("access.confirm.title"),
                )?,
            ),
            (
                "description",
                translate.call1(
                    &JsValue::UNDEFINED,
                    &JsValue::from_str("access.confirm.description"),
                )?,
            ),
            (
                "acknowledgeLabel",
                translate.call1(
                    &JsValue::UNDEFINED,
                    &JsValue::from_str("access.confirm.acknowledge"),
                )?,
            ),
            (
                "cancelLabel",
                translate.call1(
                    &JsValue::UNDEFINED,
                    &JsValue::from_str("access.confirm.cancel"),
                )?,
            ),
            (
                "confirmLabel",
                translate.call1(
                    &JsValue::UNDEFINED,
                    &JsValue::from_str("access.confirm.enable"),
                )?,
            ),
            ("acknowledged", JsValue::from_bool(acknowledged)),
            ("disabled", JsValue::from_bool(locked)),
            ("onAcknowledgedChange", set_acknowledged.into()),
            ("onCancel", close_confirmation),
            ("onConfirm", confirm),
        ])?),
        &[],
    )?;
    create_element(&modules.react, &modules.fragment, None, &[menu, risk])
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let state = required_function(react, "useState", "React")?
        .call1(react, initial)?
        .dyn_into::<Array>()?;
    Ok((state.get(0), state.get(1).dyn_into()?))
}

fn install_reset_effect(
    react: &JsValue,
    locked: bool,
    value: &JsValue,
    set_open: &Function,
    set_acknowledged: &Function,
    set_confirmation: &Function,
) -> Result<(), JsValue> {
    let open = set_open.clone();
    let acknowledged = set_acknowledged.clone();
    let confirmation = set_confirmation.clone();
    let has_value = !value.is_undefined();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if !locked && has_value {
            return Ok(JsValue::UNDEFINED);
        }
        open.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        acknowledged.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        confirmation.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    required_function(react, "useEffect", "React")?.call2(
        react,
        &effect.into_js_value(),
        &Array::of2(&JsValue::from_bool(locked), value),
    )?;
    Ok(())
}

fn submit_callback(props: &JsValue, set_pick: &Function) -> Result<Function, JsValue> {
    let command = required_function(props, "command", "PermissionSelect props")?;
    let setter = set_pick.clone();
    Closure::wrap(Box::new(move |id: JsValue| -> Result<(), JsValue> {
        setter.call1(&JsValue::UNDEFINED, &id)?;
        let id_text = id
            .as_string()
            .ok_or_else(|| js_sys::TypeError::new("permission id must be a string"))?;
        let pending = command.call1(
            &JsValue::UNDEFINED,
            &JsValue::from_str(&format!("/permission {id_text}")),
        )?;
        let rejected = Closure::wrap(
            Box::new(move |_error: JsValue| false) as Box<dyn FnMut(JsValue) -> bool>
        )
        .into_js_value();
        let settled_setter = setter.clone();
        let settled = Closure::wrap(Box::new(move |_result: JsValue| -> Result<(), JsValue> {
            settled_setter.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value();
        let caught = required_function(&pending, "catch", "permission command Promise")?
            .call1(&pending, &rejected)?;
        required_function(&caught, "then", "permission command Promise")?
            .call1(&caught, &settled)?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
    .into_js_value()
    .dyn_into()
}

fn choose_callback(
    set_open: &Function,
    set_acknowledged: &Function,
    set_confirmation: &Function,
    submit: &Function,
    configured_current: &str,
) -> JsValue {
    let open = set_open.clone();
    let acknowledged = set_acknowledged.clone();
    let confirmation = set_confirmation.clone();
    let submit = submit.clone();
    let current = configured_current.to_owned();
    Closure::wrap(Box::new(move |id: JsValue| -> Result<(), JsValue> {
        open.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        let id_text = id
            .as_string()
            .ok_or_else(|| js_sys::TypeError::new("permission id must be a string"))?;
        if id_text == current {
            return Ok(());
        }
        if id_text == FULL_ACCESS {
            acknowledged.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
            confirmation.call1(&JsValue::UNDEFINED, &id)?;
            return Ok(());
        }
        submit.call1(&JsValue::UNDEFINED, &id)?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
    .into_js_value()
}

fn close_confirmation_callback(
    set_acknowledged: &Function,
    set_confirmation: &Function,
) -> JsValue {
    let acknowledged = set_acknowledged.clone();
    let confirmation = set_confirmation.clone();
    Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        acknowledged.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        confirmation.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value()
}

fn confirm_callback(
    locked: bool,
    acknowledged: bool,
    confirmation: &JsValue,
    close: &JsValue,
    submit: &Function,
) -> JsValue {
    let confirmation = confirmation.clone();
    let close = close.clone();
    let submit = submit.clone();
    Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if locked || !acknowledged || confirmation.is_null() {
            return Ok(());
        }
        close
            .dyn_ref::<Function>()
            .ok_or_else(|| js_sys::TypeError::new("confirmation close must be a function"))?
            .call0(&JsValue::UNDEFINED)?;
        submit.call1(&JsValue::UNDEFINED, &confirmation)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value()
}

fn find_option(options: &Array, value: &str) -> Result<Option<JsValue>, JsValue> {
    for index in 0..options.length() {
        let option = options.get(index);
        if required_string(&option, "value", "permission option")? == value {
            return Ok(Some(option));
        }
    }
    Ok(None)
}

fn menu_items(modules: &BrowserModules, options: &Array) -> Result<Array, JsValue> {
    let items = Array::new();
    for index in 0..options.length() {
        let option = options.get(index);
        let value = required_string(&option, "value", "permission option")?;
        if value == "custom" {
            continue;
        }
        let mut entries = vec![
            ("id", JsValue::from_str(&value)),
            ("label", JsValue::from_str(&option_label(&option)?)),
        ];
        if let Some(icon) = permission_glyph(modules, &value) {
            entries.push(("icon", icon));
        }
        let item = object(&entries)?;
        items.push(item.as_ref());
    }
    Ok(items)
}

fn option_label(option: &JsValue) -> Result<String, JsValue> {
    let value = required_string(option, "value", "permission option")?;
    if value == FULL_ACCESS {
        Ok("Full access".to_owned())
    } else {
        Ok(display_name(&required_string(
            option,
            "name",
            "permission option",
        )?))
    }
}

fn display_name(name: &str) -> String {
    let kebab = !name.is_empty()
        && name.split('-').all(|word| {
            !word.is_empty()
                && word
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        });
    if !kebab {
        return name.to_owned();
    }
    name.split('-')
        .map(|word| {
            let (first, rest) = word.split_at(1);
            format!("{}{rest}", first.to_ascii_uppercase())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn permission_glyph(modules: &BrowserModules, value: &str) -> Option<JsValue> {
    match value {
        "read-only" => Some(modules.read_only_glyph.clone()),
        "workspace-write" => Some(modules.workspace_write_glyph.clone()),
        FULL_ACCESS => Some(modules.full_access_glyph.clone()),
        _ => None,
    }
}

fn svg(react: &JsValue, children: &[JsValue]) -> Result<JsValue, JsValue> {
    create_element(
        react,
        &JsValue::from_str("svg"),
        Some(&object(&[
            ("width", JsValue::from_str("16")),
            ("height", JsValue::from_str("16")),
            ("viewBox", JsValue::from_str("0 0 16 16")),
            ("fill", JsValue::from_str("none")),
            ("aria-hidden", JsValue::TRUE),
        ])?),
        children,
    )
}

fn outline_path(react: &JsValue) -> Result<JsValue, JsValue> {
    path(
        react,
        &[
            ("d", JsValue::from_str(SHIELD_OUTLINE)),
            ("stroke", JsValue::from_str("currentColor")),
            ("strokeWidth", JsValue::from_str("1.31831")),
            ("strokeLinejoin", JsValue::from_str("round")),
        ],
    )
}

fn read_only_glyph(react: &JsValue) -> Result<JsValue, JsValue> {
    svg(
        react,
        &[
            outline_path(react)?,
            filled_path(
                react,
                "M12.1654 5.7552L8.9447 9.41475C8.73044 9.65816 8.53628 9.8804 8.35774 10.0423C8.1713 10.2114 7.94235 10.3717 7.64016 10.4254C7.48207 10.4535 7.32 10.4552 7.16151 10.4294C6.85843 10.3801 6.62728 10.2223 6.43836 10.0559C6.25752 9.89653 6.06037 9.67732 5.84264 9.43705L4.72925 8.20897L5.63557 7.38707L6.74897 8.61594C6.98603 8.87755 7.12974 9.03533 7.24673 9.13839C7.31033 9.19443 7.34485 9.21476 7.35823 9.22122C7.38068 9.22484 7.40352 9.22515 7.42593 9.22122C7.40522 9.22502 7.42893 9.23294 7.53583 9.136C7.65132 9.03126 7.79316 8.87139 8.02643 8.60638L11.2479 4.94763L12.1654 5.7552Z",
            )?,
        ],
    )
}

fn workspace_write_glyph(react: &JsValue) -> Result<JsValue, JsValue> {
    svg(
        react,
        &[
            filled_path(
                react,
                "M8.08887 0.251709C8.20479 0.23085 8.32486 0.241168 8.43652 0.282959L15.0215 2.75171C15.2787 2.84819 15.4492 3.09414 15.4492 3.3689V7.0105C15.4492 7.10986 15.4441 7.2081 15.4414 7.30542C15.0285 7.07175 14.5905 6.87695 14.1309 6.73022V3.82495L8.20508 1.60327L2.2793 3.82495V7.0105C2.27936 9.7171 3.4745 11.5379 5.02734 12.7947C5.01025 12.9942 5 13.1962 5 13.4001C5.00001 13.7617 5.02722 14.1169 5.08008 14.4636C2.91555 13.0393 0.961014 10.752 0.960938 7.0105V3.3689C0.960938 3.09417 1.13146 2.84821 1.38867 2.75171L7.97461 0.282959L8.08887 0.251709Z",
            )?,
            filled_path(react, "M11.3525 5.64688V6.85688H5V5.64688H11.3525Z")?,
            filled_path(react, "M9.5824 8.29376V9.50376H5V8.29376H9.5824Z")?,
            filled_path(
                react,
                "M14.6647 15.6852H10.0338C10.3878 15.3751 10.7567 15.0517 11.0772 14.7706C11.2531 14.6164 11.4144 14.4746 11.5511 14.3547H14.6647V15.6852Z",
            )?,
            filled_path(
                react,
                "M8.14852 14.1308L7.33925 15.4976C7.22458 15.6912 7.42245 15.9194 7.63037 15.8333L9.09785 15.2254L15.0399 10.0719L14.0905 8.97733L8.14852 14.1308Z",
            )?,
        ],
    )
}

fn full_access_glyph(react: &JsValue) -> Result<JsValue, JsValue> {
    svg(
        react,
        &[
            outline_path(react)?,
            filled_path(react, "M9.10094 4.5V8.75939H7.59888V4.5H9.10094Z")?,
            filled_path(react, "M9.10094 9.8114V11.5H7.59888V9.8114H9.10094Z")?,
        ],
    )
}

fn filled_path(react: &JsValue, d: &str) -> Result<JsValue, JsValue> {
    path(
        react,
        &[
            ("d", JsValue::from_str(d)),
            ("fill", JsValue::from_str("currentColor")),
        ],
    )
}

fn path(react: &JsValue, props: &[(&str, JsValue)]) -> Result<JsValue, JsValue> {
    create_element(
        react,
        &JsValue::from_str("path"),
        Some(&object(props)?),
        &[],
    )
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
