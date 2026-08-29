//! Compiled transient queue dock and session-scoped registration entry.

use std::cell::RefCell;

use js_sys::{Array, Function, JsString, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::browser_reasoning::inject_style;

const QUEUE_CSS: &str =
    include_str!("../../../packages/client/ui-conversation/src/client/queue/QueueDock.module.css");
const LOCALE_NAMESPACE: &str = "conversation";

thread_local! {
    static COMPONENTS: RefCell<Option<QueueComponents>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    tooltip: JsValue,
    check: JsValue,
    chevron_down: JsValue,
    chevron_up: JsValue,
    close: JsValue,
    edit: JsValue,
    queue: JsValue,
    send: JsValue,
    trash: JsValue,
}

#[derive(Clone)]
struct QueueComponents {
    dock: JsValue,
    entry: JsValue,
}

/// Configures the compiled queue dock and registration entry.
///
/// # Errors
///
/// Returns on missing React/icon/Tooltip faces or stylesheet failure.
#[wasm_bindgen(js_name = configureClientUiConversationQueueDock)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_conversation_queue_dock(
    react: JsValue,
    ui_primitives: JsValue,
) -> Result<(), JsValue> {
    for method in ["createElement", "useEffect", "useId", "useMemo", "useState"] {
        required_function(&react, method, "React")?;
    }
    let modules = BrowserModules {
        tooltip: required_property(&ui_primitives, "Tooltip", "ui-primitives")?,
        check: required_property(&ui_primitives, "IconCheckOutline16", "ui-primitives")?,
        chevron_down: required_property(
            &ui_primitives,
            "IconChevronDownOutline14",
            "ui-primitives",
        )?,
        chevron_up: required_property(&ui_primitives, "IconChevronUpOutline14", "ui-primitives")?,
        close: required_property(&ui_primitives, "IconCloseOutline16", "ui-primitives")?,
        edit: required_property(&ui_primitives, "IconEditOutline16", "ui-primitives")?,
        queue: required_property(&ui_primitives, "IconQueueOutline14", "ui-primitives")?,
        send: required_property(&ui_primitives, "IconSendOutline14", "ui-primitives")?,
        trash: required_property(&ui_primitives, "IconTrashOutline16", "ui-primitives")?,
        react,
    };
    inject_style(
        "QueueDock",
        QUEUE_CSS,
        &[
            ("action", "seekdeep-conversation-queue-action"),
            ("actions", "seekdeep-conversation-queue-actions"),
            ("chevron", "seekdeep-conversation-queue-chevron"),
            ("count", "seekdeep-conversation-queue-count"),
            ("dock", "seekdeep-conversation-queue-dock"),
            ("editor", "seekdeep-conversation-queue-editor"),
            ("header", "seekdeep-conversation-queue-header"),
            ("lead", "seekdeep-conversation-queue-lead"),
            ("list", "seekdeep-conversation-queue-list"),
            ("panel", "seekdeep-conversation-queue-panel"),
            ("preview", "seekdeep-conversation-queue-preview"),
            ("row", "seekdeep-conversation-queue-row"),
        ],
    )?;
    let dock_modules = modules;
    let dock = raw_component(move |props| render_queue_dock(&dock_modules, props));
    let entry = queue_entry(&dock)?;
    COMPONENTS.with(|configured| {
        *configured.borrow_mut() = Some(QueueComponents { dock, entry });
    });
    Ok(())
}

fn raw_component<F>(renderer: F) -> JsValue
where
    F: 'static + FnMut(&JsValue) -> Result<JsValue, JsValue>,
{
    let mut renderer = renderer;
    Closure::wrap(Box::new(move |props: JsValue| renderer(&props))
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value()
}

/// Returns the compiled `QueueDock` component.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = queueDockComponent)]
pub fn queue_dock_component() -> Result<JsValue, JsValue> {
    Ok(configured_components()?.dock)
}

/// Returns the queue dock registration entry.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = queueDockEntry)]
pub fn queue_dock_entry_browser() -> Result<JsValue, JsValue> {
    Ok(configured_components()?.entry)
}

#[allow(clippy::too_many_lines)] // Closed queue state machine and row tree stay auditable together.
fn render_queue_dock(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let inbox = select_session(props, "queue")?.dyn_into::<Array>()?;
    let filter_inbox = inbox.clone();
    let queue_factory = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let queued = Closure::wrap(Box::new(move |row: JsValue| -> Result<bool, JsValue> {
            Ok(Reflect::get(&row, &JsValue::from_str("placement"))?
                .as_string()
                .as_deref()
                == Some("queued"))
        })
            as Box<dyn FnMut(JsValue) -> Result<bool, JsValue>>)
        .into_js_value();
        call_method(&filter_inbox, "filter", &[queued])
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let queue = required_function(&modules.react, "useMemo", "React")?
        .call2(
            &modules.react,
            &queue_factory.into_js_value(),
            &Array::of1(inbox.as_ref()),
        )?
        .dyn_into::<Array>()?;
    let running = select_session(props, "running")?
        .as_bool()
        .ok_or_else(|| js_sys::TypeError::new("Session running must be a boolean"))?;
    let queue_mutable = select_session(props, "subagent")?.is_null();
    let (editing, set_editing) = use_state(&modules.react, &JsValue::NULL)?;
    let (busy, set_busy) = use_state(&modules.react, &JsValue::NULL)?;
    let (collapsed_value, set_collapsed) = use_state(&modules.react, &JsValue::TRUE)?;
    let collapsed = collapsed_value
        .as_bool()
        .ok_or_else(|| js_sys::TypeError::new("QueueDock collapsed state must be a boolean"))?;
    let list_id = required_function(&modules.react, "useId", "React")?.call0(&modules.react)?;
    install_queue_effect(
        &modules.react,
        collapsed,
        &editing,
        &queue,
        queue_mutable,
        &set_collapsed,
        &set_editing,
    )?;
    if queue.length() == 0 {
        return Ok(JsValue::NULL);
    }
    let interaction_active = queue_mutable && (!editing.is_null() || !busy.is_null());
    let expanded = !collapsed || interaction_active;
    let list_visible = queue.length() == 1 || expanded;
    let apply_action = apply_action_callback(props, &set_busy)?;
    let save_edit = save_edit_callback(props, &editing, &apply_action, &set_editing)?;
    let header = if queue.length() > 1 {
        render_header(
            modules,
            props,
            queue.length(),
            &list_id,
            expanded,
            interaction_active,
            &set_collapsed,
        )?
    } else {
        JsValue::FALSE
    };
    let rows = if list_visible {
        render_rows(
            modules,
            props,
            &queue,
            running,
            queue_mutable,
            &editing,
            &busy,
            &set_editing,
            &save_edit,
            &apply_action,
        )?
    } else {
        Vec::new()
    };
    let list = create_element(
        &modules.react,
        &JsValue::from_str("ul"),
        Some(&object(&[
            ("id", list_id),
            (
                "className",
                JsValue::from_str("seekdeep-conversation-queue-list"),
            ),
            ("hidden", JsValue::from_bool(!list_visible)),
        ])?),
        &rows,
    )?;
    let panel = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-queue-panel")?),
        &[header, list],
    )?;
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-queue-dock"),
            ),
            ("data-queue-dock", JsValue::from_str("")),
        ])?),
        &[panel],
    )
}

fn install_queue_effect(
    react: &JsValue,
    collapsed: bool,
    editing: &JsValue,
    queue: &Array,
    queue_mutable: bool,
    set_collapsed: &Function,
    set_editing: &Function,
) -> Result<(), JsValue> {
    let effect_queue = queue.clone();
    let effect_editing = editing.clone();
    let collapse = set_collapsed.clone();
    let edit = set_editing.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if effect_queue.length() == 0 && !collapsed {
            collapse.call1(&JsValue::UNDEFINED, &JsValue::TRUE)?;
        }
        if !effect_editing.is_null() {
            let editing_id = Reflect::get(&effect_editing, &JsValue::from_str("id"))?;
            let found_id = editing_id.clone();
            let found = Closure::wrap(Box::new(move |row: JsValue| -> Result<bool, JsValue> {
                Ok(strict_string_equal(
                    &Reflect::get(&row, &JsValue::from_str("id"))?,
                    &found_id,
                ))
            })
                as Box<dyn FnMut(JsValue) -> Result<bool, JsValue>>)
            .into_js_value();
            let still_present = call_method(&effect_queue, "some", &[found])?
                .as_bool()
                .unwrap_or(false);
            if !queue_mutable || !still_present {
                edit.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
            }
        }
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    required_function(react, "useEffect", "React")?.call2(
        react,
        &effect.into_js_value(),
        &Array::of4(
            &JsValue::from_bool(collapsed),
            editing,
            queue.as_ref(),
            &JsValue::from_bool(queue_mutable),
        ),
    )?;
    Ok(())
}

fn render_header(
    modules: &BrowserModules,
    props: &JsValue,
    count: u32,
    list_id: &JsValue,
    expanded: bool,
    interaction_active: bool,
    set_collapsed: &Function,
) -> Result<JsValue, JsValue> {
    let translate = required_function(props, "t", "QueueDock props")?;
    let count_label = translate.apply(
        &JsValue::UNDEFINED,
        &Array::of2(
            &JsValue::from_str("queue.count"),
            object(&[("n", JsValue::from_f64(f64::from(count)))])?.as_ref(),
        ),
    )?;
    let setter = set_collapsed.clone();
    let on_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let invert = Closure::wrap(
            Box::new(move |value: JsValue| !value.as_bool().unwrap_or(true))
                as Box<dyn FnMut(JsValue) -> bool>,
        )
        .into_js_value();
        setter.call1(&JsValue::UNDEFINED, &invert)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    create_element(
        &modules.react,
        &JsValue::from_str("button"),
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str("seekdeep-conversation-queue-header"),
            ),
            ("aria-controls", list_id.clone()),
            ("aria-expanded", JsValue::from_bool(expanded)),
            ("disabled", JsValue::from_bool(interaction_active)),
            ("onClick", on_click),
        ])?),
        &[
            leading_icon(modules)?,
            span(modules, "count", count_label)?,
            create_element(
                &modules.react,
                &JsValue::from_str("span"),
                Some(&object(&[
                    (
                        "className",
                        JsValue::from_str("seekdeep-conversation-queue-chevron"),
                    ),
                    ("aria-hidden", JsValue::TRUE),
                ])?),
                &[create_element(
                    &modules.react,
                    if expanded {
                        &modules.chevron_down
                    } else {
                        &modules.chevron_up
                    },
                    None,
                    &[],
                )?],
            )?,
        ],
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn render_rows(
    modules: &BrowserModules,
    props: &JsValue,
    queue: &Array,
    running: bool,
    queue_mutable: bool,
    editing: &JsValue,
    busy: &JsValue,
    set_editing: &Function,
    save_edit: &Function,
    apply_action: &Function,
) -> Result<Vec<JsValue>, JsValue> {
    let mut rows = Vec::new();
    for index in 0..queue.length() {
        let row = queue.get(index);
        let id = required_property(&row, "id", "queue row")?;
        let row_editing = !editing.is_null()
            && strict_string_equal(&Reflect::get(editing, &JsValue::from_str("id"))?, &id);
        let mut children = Vec::new();
        if queue.length() == 1 {
            children.push(leading_icon(modules)?);
        } else {
            children.push(JsValue::FALSE);
        }
        children.push(if row_editing {
            render_editor(modules, props, &row, editing, set_editing, save_edit)?
        } else {
            span(
                modules,
                "preview",
                required_property(&row, "preview", "queue row")?,
            )?
        });
        if queue_mutable {
            children.push(render_actions(
                modules,
                props,
                &row,
                row_editing,
                running,
                editing,
                busy,
                set_editing,
                save_edit,
                apply_action,
            )?);
        } else {
            children.push(JsValue::FALSE);
        }
        rows.push(create_element(
            &modules.react,
            &JsValue::from_str("li"),
            Some(&object(&[
                ("key", id),
                (
                    "className",
                    JsValue::from_str("seekdeep-conversation-queue-row"),
                ),
            ])?),
            &children,
        )?);
    }
    Ok(rows)
}

fn render_editor(
    modules: &BrowserModules,
    props: &JsValue,
    row: &JsValue,
    editing: &JsValue,
    set_editing: &Function,
    save_edit: &Function,
) -> Result<JsValue, JsValue> {
    let translate = required_function(props, "t", "QueueDock props")?;
    let label = translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("queue.edit"))?;
    let edit_id = required_property(row, "id", "queue row")?;
    let change_setter = set_editing.clone();
    let change_id = edit_id.clone();
    let on_change = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let target = required_property(&event, "currentTarget", "change event")?;
        let value = Reflect::get(&target, &JsValue::from_str("value"))?;
        change_setter.call1(
            &JsValue::UNDEFINED,
            object(&[("id", change_id.clone()), ("text", value)])?.as_ref(),
        )?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
    .into_js_value();
    let key_setter = set_editing.clone();
    let key_save = save_edit.clone();
    let on_key_down = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let key = Reflect::get(&event, &JsValue::from_str("key"))?
            .as_string()
            .unwrap_or_default();
        if key == "Escape" {
            key_setter.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
            return Ok(());
        }
        if key == "Enter" {
            let native = required_property(&event, "nativeEvent", "keyboard event")?;
            if !Reflect::get(&native, &JsValue::from_str("isComposing"))?
                .as_bool()
                .unwrap_or(false)
            {
                call_method(&event, "preventDefault", &[])?;
                key_save.call0(&JsValue::UNDEFINED)?;
            }
        }
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
    .into_js_value();
    create_element(
        &modules.react,
        &JsValue::from_str("input"),
        Some(&object(&[
            ("autoFocus", JsValue::TRUE),
            (
                "className",
                JsValue::from_str("seekdeep-conversation-queue-editor"),
            ),
            ("aria-label", label),
            (
                "value",
                required_property(editing, "text", "queue editing state")?,
            ),
            ("onChange", on_change),
            ("onKeyDown", on_key_down),
        ])?),
        &[],
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn render_actions(
    modules: &BrowserModules,
    props: &JsValue,
    row: &JsValue,
    row_editing: bool,
    running: bool,
    editing: &JsValue,
    busy: &JsValue,
    set_editing: &Function,
    save_edit: &Function,
    apply_action: &Function,
) -> Result<JsValue, JsValue> {
    let actions = if row_editing {
        editing_actions(modules, props, editing, busy, set_editing, save_edit)?
    } else {
        ordinary_actions(
            modules,
            props,
            row,
            running,
            busy,
            set_editing,
            apply_action,
        )?
    };
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-queue-actions")?),
        &actions,
    )
}

fn editing_actions(
    modules: &BrowserModules,
    props: &JsValue,
    editing: &JsValue,
    busy: &JsValue,
    set_editing: &Function,
    save_edit: &Function,
) -> Result<Vec<JsValue>, JsValue> {
    let translate = required_function(props, "t", "QueueDock props")?;
    let save_tooltip = translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("queue.save"))?;
    let save_label = translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("queue.save"))?;
    let text = required_string(editing, "text", "queue editing state")?;
    let save = save_edit.clone();
    let save_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        save.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    let save_button = action_button(
        modules,
        save_label,
        !busy.is_null() || trim_js(&text).is_empty(),
        JsValue::UNDEFINED,
        save_click,
        create_element(
            &modules.react,
            &modules.check,
            Some(&object(&[("size", JsValue::from_f64(14.0))])?),
            &[],
        )?,
    )?;
    let cancel_tooltip =
        translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("queue.cancelEdit"))?;
    let cancel_label =
        translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("queue.cancelEdit"))?;
    let cancel_setter = set_editing.clone();
    let cancel = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        cancel_setter.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    let cancel_button = action_button(
        modules,
        cancel_label,
        !busy.is_null(),
        JsValue::UNDEFINED,
        cancel,
        create_element(
            &modules.react,
            &modules.close,
            Some(&object(&[("size", JsValue::from_f64(14.0))])?),
            &[],
        )?,
    )?;
    Ok(vec![
        tooltip(modules, save_tooltip, false, save_button)?,
        tooltip(modules, cancel_tooltip, false, cancel_button)?,
    ])
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn ordinary_actions(
    modules: &BrowserModules,
    props: &JsValue,
    row: &JsValue,
    running: bool,
    busy: &JsValue,
    set_editing: &Function,
    apply_action: &Function,
) -> Result<Vec<JsValue>, JsValue> {
    let translate = required_function(props, "t", "QueueDock props")?;
    let row_id = required_property(row, "id", "queue row")?;
    let row_text = Reflect::get(row, &JsValue::from_str("text"))?;
    let edit_tooltip = translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("queue.edit"))?;
    let edit_label = translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("queue.edit"))?;
    let edit_title = if row_text.is_null() {
        translate.call1(
            &JsValue::UNDEFINED,
            &JsValue::from_str("queue.edit.unsupported"),
        )?
    } else {
        JsValue::UNDEFINED
    };
    let edit_setter = set_editing.clone();
    let edit_id = row_id.clone();
    let edit_text = row_text.clone();
    let edit_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if !edit_text.is_null() {
            edit_setter.call1(
                &JsValue::UNDEFINED,
                object(&[("id", edit_id.clone()), ("text", edit_text.clone())])?.as_ref(),
            )?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    let edit_button = action_button(
        modules,
        edit_label,
        !busy.is_null() || row_text.is_null(),
        edit_title,
        edit_click,
        create_element(
            &modules.react,
            &modules.edit,
            Some(&object(&[("size", JsValue::from_f64(14.0))])?),
            &[],
        )?,
    )?;
    let remove_tooltip =
        translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("queue.remove"))?;
    let remove_label = translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("queue.remove"))?;
    let remove_apply = apply_action.clone();
    let remove_id = row_id.clone();
    let remove_translate = translate.clone();
    let remove_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let failure = remove_translate.call1(
            &JsValue::UNDEFINED,
            &JsValue::from_str("queue.removeFailed"),
        )?;
        remove_apply.apply(
            &JsValue::UNDEFINED,
            &Array::of3(
                &remove_id,
                object(&[("kind", JsValue::from_str("remove"))])?.as_ref(),
                &failure,
            ),
        )?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    let remove_button = action_button(
        modules,
        remove_label,
        !busy.is_null(),
        JsValue::UNDEFINED,
        remove_click,
        create_element(
            &modules.react,
            &modules.trash,
            Some(&object(&[("size", JsValue::from_f64(14.0))])?),
            &[],
        )?,
    )?;
    let steer_tooltip = translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("queue.steer"))?;
    let steer_label = translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("queue.steer"))?;
    let steer_title = if running {
        JsValue::UNDEFINED
    } else {
        translate.call1(
            &JsValue::UNDEFINED,
            &JsValue::from_str("queue.steer.unavailable"),
        )?
    };
    let steer_apply = apply_action.clone();
    let steer_id = row_id;
    let steer_translate = translate;
    let steer_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let failure =
            steer_translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("queue.steerFailed"))?;
        steer_apply.apply(
            &JsValue::UNDEFINED,
            &Array::of3(
                &steer_id,
                object(&[("kind", JsValue::from_str("steer"))])?.as_ref(),
                &failure,
            ),
        )?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    let steer_button = action_button(
        modules,
        steer_label,
        !busy.is_null() || !running,
        steer_title,
        steer_click,
        create_element(&modules.react, &modules.send, None, &[])?,
    )?;
    Ok(vec![
        tooltip(modules, edit_tooltip, row_text.is_null(), edit_button)?,
        tooltip(modules, remove_tooltip, false, remove_button)?,
        tooltip(modules, steer_tooltip, !running, steer_button)?,
    ])
}

fn tooltip(
    modules: &BrowserModules,
    label: JsValue,
    disabled: bool,
    child: JsValue,
) -> Result<JsValue, JsValue> {
    create_element(
        &modules.react,
        &modules.tooltip,
        Some(&object(&[
            ("label", label),
            ("side", JsValue::from_str("bottom")),
            ("delayMs", JsValue::from_f64(500.0)),
            ("disabled", JsValue::from_bool(disabled)),
        ])?),
        &[child],
    )
}

fn action_button(
    modules: &BrowserModules,
    label: JsValue,
    disabled: bool,
    title: JsValue,
    on_click: JsValue,
    icon: JsValue,
) -> Result<JsValue, JsValue> {
    create_element(
        &modules.react,
        &JsValue::from_str("button"),
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str("seekdeep-conversation-queue-action"),
            ),
            ("aria-label", label),
            ("title", title),
            ("disabled", JsValue::from_bool(disabled)),
            ("onClick", on_click),
        ])?),
        &[icon],
    )
}

fn apply_action_callback(props: &JsValue, set_busy: &Function) -> Result<Function, JsValue> {
    let update = required_function(props, "updateQueue", "QueueDock props")?;
    let notify = required_function(props, "notify", "QueueDock props")?;
    let busy_setter = set_busy.clone();
    Closure::wrap(Box::new(
        move |item_id: JsValue, action: JsValue, failure: JsValue| -> Result<JsValue, JsValue> {
            busy_setter.call1(&JsValue::UNDEFINED, &item_id)?;
            let returned = match update.apply(&JsValue::UNDEFINED, &Array::of2(&item_id, &action)) {
                Ok(value) => value,
                Err(error) => Promise::reject(&error).into(),
            };
            let pending = Promise::resolve(&returned);
            let success = Closure::wrap(
                Box::new(move |_value: JsValue| true) as Box<dyn FnMut(JsValue) -> bool>
            )
            .into_js_value();
            let failure_notify = notify.clone();
            let failure_text = failure.clone();
            let rejected =
                Closure::wrap(Box::new(move |_error: JsValue| -> Result<bool, JsValue> {
                    failure_notify.apply(
                        &JsValue::UNDEFINED,
                        &Array::of2(&JsValue::from_str("error"), &failure_text),
                    )?;
                    Ok(false)
                })
                    as Box<dyn FnMut(JsValue) -> Result<bool, JsValue>>)
                .into_js_value();
            let settled = call_method(&pending, "then", &[success, rejected])?;
            let final_setter = busy_setter.clone();
            let final_id = item_id;
            let release = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                let expected = final_id.clone();
                let clear = Closure::wrap(Box::new(move |current: JsValue| {
                    if strict_string_equal(&current, &expected) {
                        JsValue::NULL
                    } else {
                        current
                    }
                }) as Box<dyn FnMut(JsValue) -> JsValue>)
                .into_js_value();
                final_setter.call1(&JsValue::UNDEFINED, &clear)?;
                Ok(())
            }) as Box<dyn FnMut() -> Result<(), JsValue>>)
            .into_js_value();
            call_method(&settled, "finally", &[release])
        },
    )
        as Box<
            dyn FnMut(JsValue, JsValue, JsValue) -> Result<JsValue, JsValue>,
        >)
    .into_js_value()
    .dyn_into()
}

fn save_edit_callback(
    props: &JsValue,
    editing: &JsValue,
    apply_action: &Function,
    set_editing: &Function,
) -> Result<Function, JsValue> {
    let editing = editing.clone();
    let apply = apply_action.clone();
    let setter = set_editing.clone();
    let translate = required_function(props, "t", "QueueDock props")?;
    Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if editing.is_null() {
            return Ok(Promise::resolve(&JsValue::UNDEFINED).into());
        }
        let text = required_string(&editing, "text", "queue editing state")?;
        if trim_js(&text).is_empty() {
            return Ok(Promise::resolve(&JsValue::UNDEFINED).into());
        }
        let action = object(&[
            ("kind", JsValue::from_str("edit")),
            (
                "content",
                Array::of1(
                    object(&[
                        ("type", JsValue::from_str("text")),
                        ("text", JsValue::from_str(&text)),
                    ])?
                    .as_ref(),
                )
                .into(),
            ),
        ])?;
        let failure =
            translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("queue.editFailed"))?;
        let pending = apply.apply(
            &JsValue::UNDEFINED,
            &Array::of3(
                &required_property(&editing, "id", "queue editing state")?,
                action.as_ref(),
                &failure,
            ),
        )?;
        let success_setter = setter.clone();
        let success = Closure::wrap(Box::new(move |ok: JsValue| -> Result<(), JsValue> {
            if ok.as_bool() == Some(true) {
                success_setter.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
            }
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value();
        call_method(&pending, "then", &[success])
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>)
    .into_js_value()
    .dyn_into()
}

fn select_session(props: &JsValue, field: &'static str) -> Result<JsValue, JsValue> {
    let selector = Closure::wrap(Box::new(move |snapshot: JsValue| {
        Reflect::get(&snapshot, &JsValue::from_str(field))
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    required_function(props, "useSession", "QueueDock props")?
        .call1(&JsValue::UNDEFINED, &selector.into_js_value())
}

fn leading_icon(modules: &BrowserModules) -> Result<JsValue, JsValue> {
    create_element(
        &modules.react,
        &JsValue::from_str("span"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-queue-lead"),
            ),
            ("aria-hidden", JsValue::TRUE),
        ])?),
        &[create_element(&modules.react, &modules.queue, None, &[])?],
    )
}

fn span(modules: &BrowserModules, class: &str, text: JsValue) -> Result<JsValue, JsValue> {
    create_element(
        &modules.react,
        &JsValue::from_str("span"),
        Some(&class_props(&format!(
            "seekdeep-conversation-queue-{class}"
        ))?),
        &[text],
    )
}

fn trim_js(value: &str) -> String {
    String::from(JsString::from(value).trim())
}

fn strict_string_equal(left: &JsValue, right: &JsValue) -> bool {
    left.as_string() == right.as_string() && left.is_string() && right.is_string()
}

fn queue_entry(dock: &JsValue) -> Result<JsValue, JsValue> {
    let apply_dock = dock.clone();
    let apply = Closure::wrap(Box::new(move |context: JsValue| -> Result<(), JsValue> {
        let slots = required_property(&context, "slots", "Queue dock context")?;
        let inject_slots = slots.clone();
        let inject_context = context.clone();
        let inject_dock = apply_dock.clone();
        let callback = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
            let option_context = inject_context.clone();
            let inject = Closure::wrap(Box::new(move |session_id: JsValue| {
                queue_injected(&option_context, &session_id)
            })
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
            .into_js_value();
            let options = object(&[
                ("name", JsValue::from_str("conversation.input.dock")),
                ("id", JsValue::from_str("queue")),
                ("order", JsValue::from_f64(20.0)),
                ("locale", JsValue::from_str(LOCALE_NAMESPACE)),
                ("inject", inject),
            ])?;
            required_function(&inject_slots, "register", "slots")?
                .apply(&inject_slots, &Array::of2(options.as_ref(), &inject_dock))
        }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>)
        .into_js_value();
        required_function(&slots, "inject", "slots")?.apply(
            &slots,
            &Array::of2(&JsValue::from_str("conversation.input.dock"), &callback),
        )?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
    .into_js_value();
    Ok(object(&[
        ("name", JsValue::from_str("conversation-queue-dock")),
        (
            "inject",
            Array::of3(
                &JsValue::from_str("slots"),
                &JsValue::from_str("conversation"),
                &JsValue::from_str("sessions"),
            )
            .into(),
        ),
        ("apply", apply),
    ])?
    .into())
}

fn queue_injected(context: &JsValue, session_id: &JsValue) -> Result<JsValue, JsValue> {
    let sessions = required_property(context, "sessions", "Queue dock context")?;
    let actx = required_function(&sessions, "scope", "sessions")?.call1(&sessions, session_id)?;
    if actx.is_undefined() {
        return Err(js_sys::Error::new(&format!(
            "queue dock: session \"{}\" resolved no scope",
            javascript_string(session_id)?
        ))
        .into());
    }
    let conversation = required_function(&actx, "get", "session context")?
        .call1(&actx, &JsValue::from_str("conversation"))?;
    if conversation.is_undefined() {
        return Err(js_sys::Error::new("queue dock: conversation service unavailable").into());
    }
    let update_conversation = conversation.clone();
    let update_queue = Closure::wrap(Box::new(
        move |item_id: JsValue, action: JsValue| -> Result<JsValue, JsValue> {
            required_function(&update_conversation, "updateQueue", "conversation service")?
                .apply(&update_conversation, &Array::of2(&item_id, &action))
        },
    )
        as Box<dyn FnMut(JsValue, JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value();
    let notify_conversation = conversation;
    let notify_actx = actx;
    let notify = Closure::wrap(Box::new(
        move |level: JsValue, text: JsValue| -> Result<(), JsValue> {
            let input = required_property(&notify_conversation, "input", "conversation service")?;
            let service = required_function(&input, "for", "conversation input")?
                .call1(&input, &notify_actx)?;
            required_function(&service, "notify", "conversation input service")?
                .apply(&service, &Array::of2(&level, &text))?;
            Ok(())
        },
    )
        as Box<dyn FnMut(JsValue, JsValue) -> Result<(), JsValue>>)
    .into_js_value();
    Ok(object(&[("updateQueue", update_queue), ("notify", notify)])?.into())
}

fn javascript_string(value: &JsValue) -> Result<String, JsValue> {
    required_function(&js_sys::global(), "String", "global")?
        .call1(&JsValue::UNDEFINED, value)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("String() returned a non-string").into())
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let state = required_function(react, "useState", "React")?
        .call1(react, initial)?
        .dyn_into::<Array>()?;
    Ok((state.get(0), state.get(1).dyn_into()?))
}

fn configured_components() -> Result<QueueComponents, JsValue> {
    COMPONENTS.with(|components| {
        components.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation QueueDock was not configured").into()
        })
    })
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
