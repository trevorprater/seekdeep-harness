//! Browser popup-select presentation and focus lifecycle.

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};

use super::{
    BrowserModules, call_method, component as react_component, fragment, object, optional,
    required, required_function, required_string, tag, translated, translated_values, use_effect,
    use_ref,
};
use crate::{SelectOption, filter_options};

pub(crate) fn component(modules: &BrowserModules) -> JsValue {
    let modules = modules.clone();
    Closure::wrap(Box::new(move |props: JsValue| render(&modules, &props))
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value()
}

#[allow(clippy::too_many_lines)]
fn render(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let popup = required(props, "popup", "PopupSelectView")?;
    let translate = required_function(props, "t", "PopupSelectView")?;
    let store = required(&popup, "state", "PopupSelectController")?;
    let state = use_store(&modules.react, &store)?;
    let card_ref = use_ref(&modules.react, &JsValue::NULL)?;
    let search_ref = use_ref(&modules.react, &JsValue::NULL)?;
    let max_height = modules.anchored_max_height.call3(
        &JsValue::UNDEFINED,
        &card_ref,
        &JsValue::from_f64(320.0),
        &state,
    )?;
    let open = required_bool(&state, "open", "Popup state")?;
    let confirming = optional(&state, "confirming")?;
    let active = open.then(|| required_index(&state, "active")).transpose()?;
    install_scroll_effect(&modules.react, active, &card_ref)?;
    install_outside_effect(&modules.react, open, confirming.as_ref(), &card_ref, &popup)?;
    install_focus_effect(&modules.react, open, confirming.as_ref(), &search_ref)?;
    if !open {
        return Ok(JsValue::NULL);
    }

    let command = required(&state, "command", "Popup state")?;
    let status = required_string(&state, "status", "Popup state")?;
    let submitting = required_bool(&state, "submitting", "Popup state")?;
    let search = required_string(&state, "search", "Popup state")?;
    let options: Vec<SelectOption> =
        serde_wasm_bindgen::from_value(required(&state, "options", "Popup state")?)
            .map_err(|error| js_sys::Error::new(&error.to_string()))?;
    let rows = filter_options(&options, &search);
    let mut children = Vec::new();
    if confirming.is_none() {
        children.push(render_card(
            modules,
            &popup,
            &translate,
            &state,
            &command,
            &status,
            submitting,
            &search,
            rows.as_ref(),
            &card_ref,
            &search_ref,
            &max_height,
        )?);
    }
    if let Some(option) = confirming {
        children.push(render_confirmation(modules, &popup, &state, &option)?);
    }
    fragment(&modules.react, &children)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn render_card(
    modules: &BrowserModules,
    popup: &JsValue,
    translate: &Function,
    state: &JsValue,
    command: &JsValue,
    status: &str,
    submitting: bool,
    search: &str,
    rows: &[SelectOption],
    card_ref: &JsValue,
    search_ref: &JsValue,
    max_height: &JsValue,
) -> Result<JsValue, JsValue> {
    let active = required_index(state, "active")?;
    let key_popup = popup.clone();
    let key_down = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let key = required_string(&event, "key", "keyboard event")?;
        match key.as_str() {
            "ArrowDown" => {
                call_method(&event, "preventDefault", &[])?;
                call_method(&key_popup, "move", &[JsValue::from_f64(1.0)])?;
            }
            "ArrowUp" => {
                call_method(&event, "preventDefault", &[])?;
                call_method(&key_popup, "move", &[JsValue::from_f64(-1.0)])?;
            }
            "Enter" => {
                call_method(&event, "preventDefault", &[])?;
                call_method(
                    &key_popup,
                    "select",
                    &[JsValue::from_f64(usize_as_f64(active))],
                )?;
            }
            "Escape" => {
                call_method(&event, "preventDefault", &[])?;
                call_method(
                    &key_popup,
                    "dismiss",
                    &[object(&[("focusComposer", JsValue::TRUE)])?.into()],
                )?;
            }
            _ => {}
        }
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let change_popup = popup.clone();
    let change = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let current = required(&event, "currentTarget", "change event")?;
        let value = required_string(&current, "value", "search input")?;
        call_method(&change_popup, "setSearch", &[JsValue::from_str(&value)])?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let mut card_children = vec![tag(
        &modules.react,
        "input",
        Some(&object(&[
            ("ref", search_ref.clone()),
            ("className", JsValue::from_str("seekdeep-command-search")),
            ("type", JsValue::from_str("text")),
            ("placeholder", translated(translate, "search.placeholder")?),
            ("aria-label", translated(translate, "search.aria")?),
            ("value", JsValue::from_str(search)),
            ("readOnly", JsValue::from_bool(submitting)),
            ("onChange", change.into_js_value()),
        ])?),
        &[],
    )?];
    if let Some(error) = optional(state, "error")? {
        let mut error_children = vec![tag(
            &modules.react,
            "span",
            Some(&object(&[(
                "className",
                JsValue::from_str("seekdeep-command-errorText"),
            )])?),
            &[error],
        )?];
        if status == "failed" {
            let retry_popup = popup.clone();
            let retry = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                call_method(&retry_popup, "retry", &[])?;
                Ok(())
            }) as Box<dyn FnMut() -> Result<(), JsValue>>);
            error_children.push(tag(
                &modules.react,
                "button",
                Some(&object(&[
                    ("type", JsValue::from_str("button")),
                    ("className", JsValue::from_str("seekdeep-command-retry")),
                    ("onClick", retry.into_js_value()),
                ])?),
                &[translated(translate, "retry")?],
            )?);
        }
        card_children.push(tag(
            &modules.react,
            "div",
            Some(&object(&[
                ("className", JsValue::from_str("seekdeep-command-error")),
                ("role", JsValue::from_str("alert")),
            ])?),
            &error_children,
        )?);
    }
    if status == "pending" {
        card_children.push(status_node(modules, translate, "status.loading")?);
    }
    if submitting {
        card_children.push(status_node(modules, translate, "status.applying")?);
    }
    if status == "ready" && rows.is_empty() {
        card_children.push(status_node(modules, translate, "status.empty")?);
    }
    if status == "ready" {
        let mut row_nodes = Vec::with_capacity(rows.len());
        for (index, option) in rows.iter().enumerate() {
            row_nodes.push(render_row(modules, popup, option, index, index == active)?);
        }
        row_nodes.shrink_to_fit();
        card_children.push(tag(
            &modules.react,
            "div",
            Some(&object(&[
                ("role", JsValue::from_str("listbox")),
                (
                    "aria-label",
                    translated_values(translate, "listbox.aria", &[("command", command.clone())])?,
                ),
                ("className", JsValue::from_str("seekdeep-command-viewport")),
            ])?),
            &row_nodes,
        )?);
    }
    let style = Object::new();
    Reflect::set(&style, &JsValue::from_str("maxHeight"), max_height)?;
    tag(
        &modules.react,
        "div",
        Some(&object(&[
            ("ref", card_ref.clone()),
            ("className", JsValue::from_str("seekdeep-command-card")),
            ("style", style.into()),
            (
                "aria-label",
                translated_values(translate, "overlay.aria", &[("command", command.clone())])?,
            ),
            ("onKeyDown", key_down.into_js_value()),
        ])?),
        &card_children,
    )
}

fn render_row(
    modules: &BrowserModules,
    popup: &JsValue,
    option: &SelectOption,
    index: usize,
    active: bool,
) -> Result<JsValue, JsValue> {
    let click_popup = popup.clone();
    let click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        call_method(
            &click_popup,
            "select",
            &[JsValue::from_f64(usize_as_f64(index))],
        )?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let hover_popup = popup.clone();
    let hover = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        call_method(
            &hover_popup,
            "highlight",
            &[JsValue::from_f64(usize_as_f64(index))],
        )?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let mut children = vec![tag(
        &modules.react,
        "span",
        Some(&object(&[(
            "className",
            JsValue::from_str("seekdeep-command-label"),
        )])?),
        &[JsValue::from_str(&option.label)],
    )?];
    if let Some(detail) = &option.detail {
        children.push(tag(
            &modules.react,
            "span",
            Some(&object(&[(
                "className",
                JsValue::from_str("seekdeep-command-detail"),
            )])?),
            &[JsValue::from_str(detail)],
        )?);
    }
    if option.active == Some(true) {
        children.push(tag(
            &modules.react,
            "span",
            Some(&object(&[(
                "className",
                JsValue::from_str("seekdeep-command-check"),
            )])?),
            &[react_component(&modules.react, &modules.check, None, &[])?],
        )?);
    }
    tag(
        &modules.react,
        "div",
        Some(&object(&[
            ("key", JsValue::from_str(&option.id)),
            ("role", JsValue::from_str("option")),
            ("aria-selected", JsValue::from_bool(active)),
            (
                "className",
                JsValue::from_str(if active {
                    "seekdeep-command-row seekdeep-command-rowActive"
                } else {
                    "seekdeep-command-row"
                }),
            ),
            ("onClick", click.into_js_value()),
            ("onMouseEnter", hover.into_js_value()),
        ])?),
        &children,
    )
}

fn render_confirmation(
    modules: &BrowserModules,
    popup: &JsValue,
    state: &JsValue,
    option: &JsValue,
) -> Result<JsValue, JsValue> {
    let confirmation = required(option, "confirmation", "confirming option")?;
    let acknowledge_popup = popup.clone();
    let acknowledge = Closure::wrap(Box::new(move |value: bool| -> Result<(), JsValue> {
        call_method(
            &acknowledge_popup,
            "acknowledge",
            &[JsValue::from_bool(value)],
        )?;
        Ok(())
    }) as Box<dyn FnMut(bool) -> Result<(), JsValue>>);
    let cancel_popup = popup.clone();
    let cancel = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        call_method(&cancel_popup, "cancelConfirmation", &[])?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let confirm_popup = popup.clone();
    let confirm = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        call_method(&confirm_popup, "confirm", &[])?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    react_component(
        &modules.react,
        &modules.risk_confirmation,
        Some(&object(&[
            ("open", JsValue::TRUE),
            (
                "title",
                required(&confirmation, "title", "select confirmation")?,
            ),
            (
                "description",
                required(&confirmation, "description", "select confirmation")?,
            ),
            (
                "acknowledgeLabel",
                required(&confirmation, "acknowledgeLabel", "select confirmation")?,
            ),
            (
                "cancelLabel",
                required(&confirmation, "cancelLabel", "select confirmation")?,
            ),
            (
                "confirmLabel",
                required(&confirmation, "confirmLabel", "select confirmation")?,
            ),
            (
                "acknowledged",
                required(state, "acknowledged", "Popup state")?,
            ),
            ("onAcknowledgedChange", acknowledge.into_js_value()),
            ("onCancel", cancel.into_js_value()),
            ("onConfirm", confirm.into_js_value()),
        ])?),
        &[],
    )
}

fn status_node(
    modules: &BrowserModules,
    translate: &Function,
    key: &str,
) -> Result<JsValue, JsValue> {
    tag(
        &modules.react,
        "div",
        Some(&object(&[(
            "className",
            JsValue::from_str("seekdeep-command-status"),
        )])?),
        &[translated(translate, key)?],
    )
}

fn use_store(react: &JsValue, store: &JsValue) -> Result<JsValue, JsValue> {
    let subscribe_store = store.clone();
    let subscribe = Closure::wrap(Box::new(move |listener: Function| {
        call_method(&subscribe_store, "subscribe", &[listener.into()])
    })
        as Box<dyn FnMut(Function) -> Result<JsValue, JsValue>>);
    let snapshot_store = store.clone();
    let snapshot = Closure::wrap(
        Box::new(move || call_method(&snapshot_store, "getSnapshot", &[]))
            as Box<dyn FnMut() -> Result<JsValue, JsValue>>,
    );
    required_function(react, "useSyncExternalStore", "React")?.call2(
        react,
        &subscribe.into_js_value(),
        &snapshot.into_js_value(),
    )
}

fn install_scroll_effect(
    react: &JsValue,
    active: Option<usize>,
    card_ref: &JsValue,
) -> Result<(), JsValue> {
    let card_ref = card_ref.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if active.is_none() {
            return Ok(JsValue::UNDEFINED);
        }
        let card = Reflect::get(&card_ref, &JsValue::from_str("current"))?;
        if card.is_null() {
            return Ok(JsValue::UNDEFINED);
        }
        let selected = call_method(
            &card,
            "querySelector",
            &[JsValue::from_str("[aria-selected=\"true\"]")],
        )?;
        if !selected.is_null() {
            call_method(
                &selected,
                "scrollIntoView",
                &[object(&[("block", JsValue::from_str("nearest"))])?.into()],
            )?;
        }
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(
        react,
        &effect.into_js_value(),
        &Array::of1(&active.map_or(JsValue::NULL, |value| {
            JsValue::from_f64(usize_as_f64(value))
        })),
    )
}

fn install_outside_effect(
    react: &JsValue,
    open: bool,
    confirming: Option<&JsValue>,
    card_ref: &JsValue,
    popup: &JsValue,
) -> Result<(), JsValue> {
    let confirming_present = confirming.is_some();
    let confirming_dependency = confirming.cloned().unwrap_or(JsValue::NULL);
    let card_ref = card_ref.clone();
    let popup = popup.clone();
    let popup_dependency = popup.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if !open || confirming_present {
            return Ok(JsValue::UNDEFINED);
        }
        let document = required(&js_sys::global(), "document", "global")?;
        let listener_ref = card_ref.clone();
        let listener_popup = popup.clone();
        let listener = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            let target = required(&event, "target", "pointer event")?;
            if !target.is_instance_of::<web_sys::Node>() {
                return Ok(());
            }
            let card = required(&listener_ref, "current", "popup card ref")?;
            if card
                .dyn_ref::<web_sys::Node>()
                .zip(target.dyn_ref::<web_sys::Node>())
                .is_some_and(|(card, target)| card.contains(Some(target)))
            {
                return Ok(());
            }
            call_method(&listener_popup, "dismiss", &[])?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value();
        call_method(
            &document,
            "addEventListener",
            &[
                JsValue::from_str("pointerdown"),
                listener.clone(),
                JsValue::TRUE,
            ],
        )?;
        Ok(Closure::wrap(Box::new(move || {
            let _ = call_method(
                &document,
                "removeEventListener",
                &[
                    JsValue::from_str("pointerdown"),
                    listener.clone(),
                    JsValue::TRUE,
                ],
            );
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let dependencies = Array::new();
    dependencies.push(&JsValue::from_bool(open));
    dependencies.push(&confirming_dependency);
    dependencies.push(&popup_dependency);
    use_effect(react, &effect.into_js_value(), &dependencies)
}

fn install_focus_effect(
    react: &JsValue,
    open: bool,
    confirming: Option<&JsValue>,
    search_ref: &JsValue,
) -> Result<(), JsValue> {
    let confirming_present = confirming.is_some();
    let confirming_dependency = confirming.cloned().unwrap_or(JsValue::NULL);
    let search_ref = search_ref.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if open && !confirming_present {
            let search = Reflect::get(&search_ref, &JsValue::from_str("current"))?;
            if !search.is_null() {
                call_method(&search, "focus", &[])?;
            }
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    use_effect(
        react,
        &effect.into_js_value(),
        &Array::of2(&JsValue::from_bool(open), &confirming_dependency),
    )
}

fn required_bool(value: &JsValue, key: &str, owner: &str) -> Result<bool, JsValue> {
    required(value, key, owner)?
        .as_bool()
        .ok_or_else(|| js_sys::Error::new(&format!("{owner} {key:?} must be a boolean")).into())
}

fn required_index(value: &JsValue, key: &str) -> Result<usize, JsValue> {
    let value = required(value, key, "numeric value")?
        .as_f64()
        .ok_or_else(|| js_sys::Error::new(&format!("{key} must be numeric")))?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(value as usize)
}

fn usize_as_f64(value: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}
