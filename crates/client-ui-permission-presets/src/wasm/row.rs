//! Rust/WASM Permission Settings row and Full access risk gate.

use js_sys::{Array, Function, Reflect};
use wasm_bindgen::{JsValue, closure::Closure};

use super::{
    BrowserModules, component as react_component, fragment, object, optional, required,
    required_bool, required_function, required_string, tag, translated, use_effect, use_state,
};
use crate::FULL_ACCESS_PRESET;

pub(crate) fn component(modules: &BrowserModules) -> JsValue {
    let modules = modules.clone();
    Closure::wrap(Box::new(move |props: JsValue| render(&modules, &props))
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value()
}

#[allow(clippy::too_many_lines)]
fn render(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let load = required_function(props, "load", "PermissionRow")?;
    let select = required_function(props, "select", "PermissionRow")?;
    let use_permission = required_function(props, "usePermission", "PermissionRow")?;
    let translate = required_function(props, "t", "PermissionRow")?;
    let selector = Closure::wrap(
        Box::new(move |snapshot: JsValue| snapshot) as Box<dyn FnMut(JsValue) -> JsValue>
    );
    let state = use_permission.call1(&JsValue::UNDEFINED, &selector.into_js_value())?;
    let (open, set_open) = use_state(&modules.react, &JsValue::FALSE)?;
    let (confirming, set_confirming) = use_state(&modules.react, &JsValue::FALSE)?;
    let (acknowledged, set_acknowledged) = use_state(&modules.react, &JsValue::FALSE)?;
    let open = open.as_bool().unwrap_or(false);
    let confirming = confirming.as_bool().unwrap_or(false);
    let acknowledged = acknowledged.as_bool().unwrap_or(false);
    let status = required_string(&state, "status", "Permission Settings state")?;
    let writable = required_bool(&state, "writable", "Permission Settings state")?;
    let current = required_string(&state, "currentValue", "Permission Settings state")?;
    let options = Array::from(&required(&state, "options", "Permission Settings state")?);
    let error = optional(&state, "error")?
        .map(|value| {
            value.as_string().ok_or_else(|| {
                JsValue::from(js_sys::Error::new(
                    "Permission Settings error must be a string",
                ))
            })
        })
        .transpose()?;

    install_load_effect(&modules.react, &load)?;
    install_availability_effect(
        &modules.react,
        &status,
        writable,
        &set_open,
        &set_confirming,
        &set_acknowledged,
    )?;
    if status == "unavailable" {
        return Ok(JsValue::NULL);
    }

    let selected = options.iter().find(|option| {
        Reflect::get(option, &JsValue::from_str("id"))
            .ok()
            .and_then(|value| value.as_string())
            .as_deref()
            == Some(&current)
    });
    let busy = matches!(status.as_str(), "loading" | "saving") || confirming;
    let label = selected
        .as_ref()
        .map(|option| required_string(option, "label", "Permission option"))
        .transpose()?
        .map_or_else(
            || translated(&translate, if busy { "loading" } else { "unavailable" }),
            |label| Ok(JsValue::from_str(&label)),
        )?;
    let description = error.as_ref().map_or_else(
        || translated(&translate, "description"),
        |error| Ok(JsValue::from_str(error)),
    )?;

    let anchor = render_anchor(
        modules,
        label,
        open,
        busy || !writable || options.length() == 0,
        &set_open,
    )?;
    let menu = render_menu(
        modules,
        &options,
        &current,
        open,
        anchor,
        &set_open,
        &set_confirming,
        &set_acknowledged,
        &select,
    )?;
    let row = tag(
        &modules.react,
        "div",
        Some(&object(&[(
            "className",
            JsValue::from_str("seekdeep-permission-row"),
        )])?),
        &[
            tag(
                &modules.react,
                "div",
                Some(&object(&[(
                    "className",
                    JsValue::from_str("seekdeep-permission-rowText"),
                )])?),
                &[
                    tag(
                        &modules.react,
                        "div",
                        Some(&object(&[(
                            "className",
                            JsValue::from_str("seekdeep-permission-title"),
                        )])?),
                        &[translated(&translate, "title")?],
                    )?,
                    tag(
                        &modules.react,
                        "div",
                        Some(&object(&[
                            ("className", JsValue::from_str("seekdeep-permission-desc")),
                            (
                                "role",
                                if error.is_some() {
                                    JsValue::from_str("alert")
                                } else {
                                    JsValue::UNDEFINED
                                },
                            ),
                        ])?),
                        &[description],
                    )?,
                ],
            )?,
            menu,
        ],
    )?;
    let risk = render_risk(
        modules,
        &translate,
        confirming,
        acknowledged,
        !writable || status == "saving",
        &set_confirming,
        &set_acknowledged,
        &select,
    )?;
    fragment(&modules.react, &[row, risk])
}

fn install_load_effect(react: &JsValue, load: &Function) -> Result<(), JsValue> {
    let load_call = load.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        load_call.call0(&JsValue::UNDEFINED)?;
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(
        react,
        &effect.into_js_value(),
        &Array::of1(&load.clone().into()),
    )
}

fn install_availability_effect(
    react: &JsValue,
    status: &str,
    writable: bool,
    set_open: &Function,
    set_confirming: &Function,
    set_acknowledged: &Function,
) -> Result<(), JsValue> {
    let open = set_open.clone();
    let confirming = set_confirming.clone();
    let acknowledged = set_acknowledged.clone();
    let available = writable && status != "unavailable";
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if !available {
            open.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
            acknowledged.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
            confirming.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        }
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(
        react,
        &effect.into_js_value(),
        &Array::of2(&JsValue::from_str(status), &JsValue::from_bool(writable)),
    )
}

fn render_anchor(
    modules: &BrowserModules,
    label: JsValue,
    open: bool,
    disabled: bool,
    set_open: &Function,
) -> Result<JsValue, JsValue> {
    let setter = set_open.clone();
    let click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let update = Closure::wrap(Box::new(|value: bool| !value) as Box<dyn FnMut(bool) -> bool>);
        setter.call1(&JsValue::UNDEFINED, &update.into_js_value())?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    tag(
        &modules.react,
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str("seekdeep-permission-selector"),
            ),
            ("aria-haspopup", JsValue::from_str("menu")),
            ("aria-expanded", JsValue::from_bool(open)),
            ("disabled", JsValue::from_bool(disabled)),
            ("onClick", click.into_js_value()),
        ])?),
        &[
            label,
            react_component(
                &modules.react,
                &modules.chevron_down,
                Some(&object(&[(
                    "className",
                    JsValue::from_str("seekdeep-permission-chevron"),
                )])?),
                &[],
            )?,
        ],
    )
}

#[allow(clippy::too_many_arguments)]
fn render_menu(
    modules: &BrowserModules,
    options: &Array,
    current: &str,
    open: bool,
    anchor: JsValue,
    set_open: &Function,
    set_confirming: &Function,
    set_acknowledged: &Function,
    select: &Function,
) -> Result<JsValue, JsValue> {
    let items = Array::new();
    for option in options.iter() {
        let item: JsValue = object(&[
            (
                "id",
                JsValue::from_str(&required_string(&option, "id", "Permission option")?),
            ),
            (
                "label",
                JsValue::from_str(&required_string(&option, "label", "Permission option")?),
            ),
        ])?
        .into();
        items.push(&item);
    }
    let close_setter = set_open.clone();
    let close = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        close_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let pick_open = set_open.clone();
    let pick_confirming = set_confirming.clone();
    let pick_acknowledged = set_acknowledged.clone();
    let pick_select = select.clone();
    let selected_id = current.to_owned();
    let current = current.to_owned();
    let on_select = Closure::wrap(Box::new(move |id: String| -> Result<(), JsValue> {
        pick_open.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        if id == current {
            return Ok(());
        }
        if id == FULL_ACCESS_PRESET {
            pick_acknowledged.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
            pick_confirming.call1(&JsValue::UNDEFINED, &JsValue::TRUE)?;
            return Ok(());
        }
        pick_select.call1(&JsValue::UNDEFINED, &JsValue::from_str(&id))?;
        Ok(())
    }) as Box<dyn FnMut(String) -> Result<(), JsValue>>);
    react_component(
        &modules.react,
        &modules.menu,
        Some(&object(&[
            ("open", JsValue::from_bool(open)),
            ("onClose", close.into_js_value()),
            ("items", items.into()),
            ("selectedId", JsValue::from_str(&selected_id)),
            ("onSelect", on_select.into_js_value()),
            ("align", JsValue::from_str("end")),
            ("portal", JsValue::TRUE),
            ("anchor", anchor),
        ])?),
        &[],
    )
}

#[allow(clippy::too_many_arguments)]
fn render_risk(
    modules: &BrowserModules,
    translate: &Function,
    open: bool,
    acknowledged: bool,
    disabled: bool,
    set_confirming: &Function,
    set_acknowledged: &Function,
    select: &Function,
) -> Result<JsValue, JsValue> {
    let acknowledged_setter = set_acknowledged.clone();
    let on_acknowledged = Closure::wrap(Box::new(move |value: bool| -> Result<(), JsValue> {
        acknowledged_setter.call1(&JsValue::UNDEFINED, &JsValue::from_bool(value))?;
        Ok(())
    }) as Box<dyn FnMut(bool) -> Result<(), JsValue>>);
    let cancel_acknowledged = set_acknowledged.clone();
    let cancel_confirming = set_confirming.clone();
    let cancel = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        cancel_acknowledged.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        cancel_confirming.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let confirm_acknowledged = set_acknowledged.clone();
    let confirm_confirming = set_confirming.clone();
    let confirm_select = select.clone();
    let confirm = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        confirm_acknowledged.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        confirm_confirming.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        confirm_select.call1(&JsValue::UNDEFINED, &JsValue::from_str(FULL_ACCESS_PRESET))?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    react_component(
        &modules.react,
        &modules.risk_confirmation,
        Some(&object(&[
            ("open", JsValue::from_bool(open)),
            ("title", translated(translate, "confirm.title")?),
            ("description", translated(translate, "confirm.description")?),
            (
                "acknowledgeLabel",
                translated(translate, "confirm.acknowledge")?,
            ),
            ("cancelLabel", translated(translate, "confirm.cancel")?),
            ("confirmLabel", translated(translate, "confirm.enable")?),
            ("acknowledged", JsValue::from_bool(acknowledged)),
            ("disabled", JsValue::from_bool(disabled)),
            ("onAcknowledgedChange", on_acknowledged.into_js_value()),
            ("onCancel", cancel.into_js_value()),
            ("onConfirm", confirm.into_js_value()),
        ])?),
        &[],
    )
}
