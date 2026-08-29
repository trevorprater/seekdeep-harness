//! Compiled Todo plan strip, dock adapter, and registration entry.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::browser_reasoning::inject_style;

const TODO_CSS: &str = include_str!(
    "../../../packages/client/ui-conversation/src/client/skeleton/TodoPanel.module.css"
);
const LOCALE_NAMESPACE: &str = "conversation";

thread_local! {
    static COMPONENTS: RefCell<Option<TodoComponents>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    checklist: JsValue,
    chevron_down: JsValue,
    chevron_up: JsValue,
}

#[derive(Clone)]
struct TodoComponents {
    panel: JsValue,
    dock: JsValue,
    status: JsValue,
    completed: JsValue,
    progress: JsValue,
    pending: JsValue,
    entry: JsValue,
}

/// Configures the compiled Todo panel family.
///
/// # Errors
///
/// Returns on missing React/icon faces, SVG construction, or stylesheet failure.
#[wasm_bindgen(js_name = configureClientUiConversationTodoPanel)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_conversation_todo_panel(
    react: JsValue,
    ui_primitives: JsValue,
) -> Result<(), JsValue> {
    for method in ["createElement", "useId", "useState"] {
        required_function(&react, method, "React")?;
    }
    let modules = BrowserModules {
        checklist: required_property(&ui_primitives, "IconChecklistOutline14", "ui-primitives")?,
        chevron_down: required_property(
            &ui_primitives,
            "IconChevronDownOutline14",
            "ui-primitives",
        )?,
        chevron_up: required_property(&ui_primitives, "IconChevronUpOutline14", "ui-primitives")?,
        react,
    };
    inject_style(
        "TodoPanel",
        TODO_CSS,
        &[
            ("body", "seekdeep-conversation-todo-body"),
            ("chevron", "seekdeep-conversation-todo-chevron"),
            ("content", "seekdeep-conversation-todo-content"),
            ("glyph", "seekdeep-conversation-todo-glyph"),
            (
                "glyphCompleted",
                "seekdeep-conversation-todo-glyphCompleted",
            ),
            ("glyphPending", "seekdeep-conversation-todo-glyphPending"),
            ("glyphProgress", "seekdeep-conversation-todo-glyphProgress"),
            ("header", "seekdeep-conversation-todo-header"),
            ("item", "seekdeep-conversation-todo-item"),
            ("lead", "seekdeep-conversation-todo-lead"),
            ("list", "seekdeep-conversation-todo-list"),
            ("progress", "seekdeep-conversation-todo-progress"),
            ("root", "seekdeep-conversation-todo-root"),
            ("title", "seekdeep-conversation-todo-title"),
        ],
    )?;
    let completed_modules = modules.clone();
    let completed = raw_component(move |_props| completed_glyph(&completed_modules));
    let progress_modules = modules.clone();
    let progress = raw_component(move |_props| progress_glyph(&progress_modules));
    let pending_modules = modules.clone();
    let pending = raw_component(move |_props| pending_glyph(&pending_modules));
    let status_modules = modules.clone();
    let status_completed = completed.clone();
    let status_progress = progress.clone();
    let status_pending = pending.clone();
    let status = raw_component(move |props| {
        render_status(
            &status_modules,
            &status_completed,
            &status_progress,
            &status_pending,
            props,
        )
    });
    let panel_modules = modules.clone();
    let panel_status = status.clone();
    let panel = raw_component(move |props| render_todo_panel(&panel_modules, &panel_status, props));
    let dock_modules = modules.clone();
    let dock_panel = panel.clone();
    let dock = raw_component(move |props| render_todo_dock(&dock_modules, &dock_panel, props));
    let entry = todo_entry(&dock)?;
    COMPONENTS.with(|configured| {
        *configured.borrow_mut() = Some(TodoComponents {
            panel,
            dock,
            status,
            completed,
            progress,
            pending,
            entry,
        });
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

/// Returns the compiled `TodoPanel` component.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = todoPanelComponent)]
pub fn todo_panel_component() -> Result<JsValue, JsValue> {
    Ok(configured_components()?.panel)
}

/// Returns the compiled `TodoDock` component.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = todoDockComponent)]
pub fn todo_dock_component() -> Result<JsValue, JsValue> {
    Ok(configured_components()?.dock)
}

/// Returns the Todo dock registration object.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = todoDockEntry)]
pub fn todo_dock_entry_browser() -> Result<JsValue, JsValue> {
    Ok(configured_components()?.entry)
}

/// Internal status component getters for live same-crate parity tests.
#[doc(hidden)]
pub fn todo_status_components() -> Result<Array, JsValue> {
    let components = configured_components()?;
    Ok(Array::of4(
        &components.status,
        &components.completed,
        &components.progress,
        &components.pending,
    ))
}

fn completed_glyph(modules: &BrowserModules) -> Result<JsValue, JsValue> {
    let circle = circle(
        modules,
        &[
            ("cx", JsValue::from_str("7")),
            ("cy", JsValue::from_str("7")),
            ("r", JsValue::from_str("6.4")),
            ("stroke", JsValue::from_str("currentColor")),
            ("strokeWidth", JsValue::from_str("1.2")),
        ],
    )?;
    let check = path(
        modules,
        &[
            (
                "d",
                JsValue::from_str(
                    "M10.9631 5.71411L7.70154 8.97571C7.48011 9.19714 7.27736 9.40099 7.09229 9.54993C6.89742 9.70669 6.66314 9.85279 6.3634 9.90027C6.2049 9.92534 6.04339 9.92534 5.88489 9.90027C5.58515 9.85279 5.35087 9.70669 5.15601 9.54993C4.97093 9.40099 4.76818 9.19714 4.54675 8.97571L3.03516 7.46411L3.96313 6.53613L5.47473 8.04773C5.7169 8.28989 5.86196 8.43389 5.97888 8.52795C6.08597 8.61409 6.10875 8.60701 6.08997 8.604C6.11259 8.60758 6.13571 8.60758 6.15833 8.604C6.13954 8.60701 6.16232 8.61409 6.26941 8.52795C6.38633 8.43389 6.53139 8.28989 6.77356 8.04773L10.0352 4.78613L10.9631 5.71411Z",
                ),
            ),
            ("fill", JsValue::from_str("currentColor")),
        ],
    )?;
    status_svg(modules, "glyphCompleted", &[circle, check])
}

fn progress_glyph(modules: &BrowserModules) -> Result<JsValue, JsValue> {
    let gradient_id = required_function(&modules.react, "useId", "React")?.call0(&modules.react)?;
    let gradient = create_element(
        &modules.react,
        &JsValue::from_str("linearGradient"),
        Some(&object(&[
            ("id", gradient_id.clone()),
            ("x1", JsValue::from_str("2.5")),
            ("y1", JsValue::from_str("12")),
            ("x2", JsValue::from_str("10.5")),
            ("y2", JsValue::from_str("3.5")),
            ("gradientUnits", JsValue::from_str("userSpaceOnUse")),
        ])?),
        &[
            create_element(
                &modules.react,
                &JsValue::from_str("stop"),
                Some(&object(&[(
                    "stopColor",
                    JsValue::from_str("currentColor"),
                )])?),
                &[],
            )?,
            create_element(
                &modules.react,
                &JsValue::from_str("stop"),
                Some(&object(&[
                    ("offset", JsValue::from_str("1")),
                    ("stopColor", JsValue::from_str("currentColor")),
                    ("stopOpacity", JsValue::from_str("0")),
                ])?),
                &[],
            )?,
        ],
    )?;
    let defs = create_element(
        &modules.react,
        &JsValue::from_str("defs"),
        None,
        &[gradient],
    )?;
    let id = gradient_id
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("React useId did not return a string"))?;
    let ring = circle(
        modules,
        &[
            ("cx", JsValue::from_str("7")),
            ("cy", JsValue::from_str("7")),
            ("r", JsValue::from_str("6.4")),
            ("stroke", JsValue::from_str(&format!("url(#{id})"))),
            ("strokeWidth", JsValue::from_str("1.2")),
        ],
    )?;
    status_svg(modules, "glyphProgress", &[defs, ring])
}

fn pending_glyph(modules: &BrowserModules) -> Result<JsValue, JsValue> {
    let ring = circle(
        modules,
        &[
            ("cx", JsValue::from_str("7")),
            ("cy", JsValue::from_str("7")),
            ("r", JsValue::from_str("6.4")),
            ("stroke", JsValue::from_str("currentColor")),
            ("strokeWidth", JsValue::from_str("1.2")),
            ("strokeDasharray", JsValue::from_str("2.4 2.4")),
        ],
    )?;
    status_svg(modules, "glyphPending", &[ring])
}

fn status_svg(
    modules: &BrowserModules,
    class: &str,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    create_element(
        &modules.react,
        &JsValue::from_str("svg"),
        Some(&object(&[
            ("width", JsValue::from_f64(14.0)),
            ("height", JsValue::from_f64(14.0)),
            ("viewBox", JsValue::from_str("0 0 14 14")),
            ("fill", JsValue::from_str("none")),
            ("aria-hidden", JsValue::from_str("true")),
            (
                "className",
                JsValue::from_str(&format!("seekdeep-conversation-todo-{class}")),
            ),
        ])?),
        children,
    )
}

fn circle(modules: &BrowserModules, props: &[(&str, JsValue)]) -> Result<JsValue, JsValue> {
    create_element(
        &modules.react,
        &JsValue::from_str("circle"),
        Some(&object(props)?),
        &[],
    )
}

fn path(modules: &BrowserModules, props: &[(&str, JsValue)]) -> Result<JsValue, JsValue> {
    create_element(
        &modules.react,
        &JsValue::from_str("path"),
        Some(&object(props)?),
        &[],
    )
}

fn render_status(
    modules: &BrowserModules,
    completed: &JsValue,
    progress: &JsValue,
    pending: &JsValue,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let status = Reflect::get(props, &JsValue::from_str("status"))?;
    let component = match status.as_string().as_deref() {
        Some("completed") => completed,
        Some("in_progress") => progress,
        Some("pending") => pending,
        _ => {
            return Err(js_sys::Error::new(&format!(
                "unreachable todo status: {}",
                javascript_string(&status)?
            ))
            .into());
        }
    };
    create_element(&modules.react, component, None, &[])
}

#[allow(clippy::too_many_lines)] // Closed panel tree and status fold stay together.
fn render_todo_panel(
    modules: &BrowserModules,
    status_component: &JsValue,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let state = required_function(&modules.react, "useState", "React")?
        .call1(&modules.react, &JsValue::TRUE)?
        .dyn_into::<Array>()?;
    let collapsed = state
        .get(0)
        .as_bool()
        .ok_or_else(|| js_sys::TypeError::new("TodoPanel collapsed state must be a boolean"))?;
    let set_collapsed = state.get(1).dyn_into::<Function>()?;
    let todos = required_property(props, "todos", "TodoPanel props")?.dyn_into::<Array>()?;
    if todos.length() == 0 {
        return Ok(JsValue::NULL);
    }
    let translate = required_function(props, "t", "TodoPanel props")?;
    let aria_label = translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("todo.title"))?;
    let title = translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("todo.title"))?;
    let progress_label = progress_label(&todos, &translate)?;
    let toggle_setter = set_collapsed;
    let toggle = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let invert = Closure::wrap(
            Box::new(move |value: JsValue| !value.as_bool().unwrap_or(true))
                as Box<dyn FnMut(JsValue) -> bool>,
        )
        .into_js_value();
        toggle_setter.call1(&JsValue::UNDEFINED, &invert)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    let header = create_element(
        &modules.react,
        &JsValue::from_str("button"),
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str("seekdeep-conversation-todo-header"),
            ),
            ("aria-expanded", JsValue::from_bool(!collapsed)),
            ("onClick", toggle),
        ])?),
        &[
            create_element(
                &modules.react,
                &JsValue::from_str("span"),
                Some(&object(&[
                    (
                        "className",
                        JsValue::from_str("seekdeep-conversation-todo-lead"),
                    ),
                    ("aria-hidden", JsValue::TRUE),
                ])?),
                &[create_element(
                    &modules.react,
                    &modules.checklist,
                    None,
                    &[],
                )?],
            )?,
            span(modules, "title", title)?,
            span(modules, "progress", JsValue::from_str(&progress_label))?,
            create_element(
                &modules.react,
                &JsValue::from_str("span"),
                Some(&object(&[
                    (
                        "className",
                        JsValue::from_str("seekdeep-conversation-todo-chevron"),
                    ),
                    ("aria-hidden", JsValue::TRUE),
                ])?),
                &[create_element(
                    &modules.react,
                    if collapsed {
                        &modules.chevron_up
                    } else {
                        &modules.chevron_down
                    },
                    None,
                    &[],
                )?],
            )?,
        ],
    )?;
    let list = if collapsed {
        JsValue::FALSE
    } else {
        let mut items = Vec::new();
        for index in 0..todos.length() {
            let key = JsValue::from_f64(f64::from(index));
            if !Reflect::has(todos.as_ref(), &key)? {
                continue;
            }
            let item = todos.get(index);
            let key_content = required_string(&item, "content", "Todo item")?;
            let data_status = required_string(&item, "status", "Todo item")?;
            let glyph_status = required_string(&item, "status", "Todo item")?;
            let visible_content = required_string(&item, "content", "Todo item")?;
            items.push(create_element(
                &modules.react,
                &JsValue::from_str("li"),
                Some(&object(&[
                    ("key", JsValue::from_str(&key_content)),
                    (
                        "className",
                        JsValue::from_str("seekdeep-conversation-todo-item"),
                    ),
                    ("data-status", JsValue::from_str(&data_status)),
                ])?),
                &[
                    create_element(
                        &modules.react,
                        &JsValue::from_str("span"),
                        Some(&object(&[
                            (
                                "className",
                                JsValue::from_str("seekdeep-conversation-todo-glyph"),
                            ),
                            ("aria-hidden", JsValue::TRUE),
                        ])?),
                        &[create_element(
                            &modules.react,
                            status_component,
                            Some(&object(&[("status", JsValue::from_str(&glyph_status))])?),
                            &[],
                        )?],
                    )?,
                    span(modules, "content", JsValue::from_str(&visible_content))?,
                ],
            )?);
        }
        create_element(
            &modules.react,
            &JsValue::from_str("ul"),
            Some(&object(&[(
                "className",
                JsValue::from_str("seekdeep-conversation-todo-list"),
            )])?),
            &items,
        )?
    };
    let body = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[(
            "className",
            JsValue::from_str("seekdeep-conversation-todo-body"),
        )])?),
        &[header, list],
    )?;
    create_element(
        &modules.react,
        &JsValue::from_str("section"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-todo-root"),
            ),
            ("data-testid", JsValue::from_str("todo-panel")),
            ("aria-label", aria_label),
        ])?),
        &[body],
    )
}

fn progress_label(todos: &Array, translate: &Function) -> Result<String, JsValue> {
    let mut done = 0_u32;
    for index in 0..todos.length() {
        if Reflect::has(todos.as_ref(), &JsValue::from_f64(f64::from(index)))?
            && required_string(&todos.get(index), "status", "Todo item")? == "completed"
        {
            done += 1;
        }
    }
    let mut active = 0_u32;
    for index in 0..todos.length() {
        if Reflect::has(todos.as_ref(), &JsValue::from_f64(f64::from(index)))?
            && required_string(&todos.get(index), "status", "Todo item")? == "in_progress"
        {
            active += 1;
        }
    }
    let pending = todos.length() - done - active;
    let mut labels = Vec::new();
    if done > 0 {
        labels.push(progress_copy(
            translate,
            "todo.progress.done",
            "done",
            done,
        )?);
    }
    if active > 0 {
        labels.push(progress_copy(
            translate,
            "todo.progress.active",
            "active",
            active,
        )?);
    }
    if pending > 0 {
        labels.push(progress_copy(
            translate,
            "todo.progress.pending",
            "pending",
            pending,
        )?);
    }
    Ok(labels.join("\u{2002}·\u{2002}"))
}

fn progress_copy(
    translate: &Function,
    key: &str,
    variable: &str,
    value: u32,
) -> Result<String, JsValue> {
    translate
        .apply(
            &JsValue::UNDEFINED,
            &Array::of2(
                &JsValue::from_str(key),
                object(&[(variable, JsValue::from_f64(f64::from(value)))])?.as_ref(),
            ),
        )?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("Todo progress translation must be a string").into())
}

fn render_todo_dock(
    modules: &BrowserModules,
    panel: &JsValue,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let todos = required_function(props, "useProjection", "TodoDock props")?
        .call1(&JsValue::UNDEFINED, &JsValue::from_str("todos"))?;
    let todos = if todos.is_null() || todos.is_undefined() {
        Array::new().into()
    } else {
        todos
    };
    create_element(
        &modules.react,
        panel,
        Some(&object(&[
            ("todos", todos),
            ("t", required_property(props, "t", "TodoDock props")?),
        ])?),
        &[],
    )
}

fn todo_entry(dock: &JsValue) -> Result<JsValue, JsValue> {
    let apply_dock = dock.clone();
    let apply = Closure::wrap(Box::new(move |context: JsValue| -> Result<(), JsValue> {
        let slots = required_property(&context, "slots", "Todo dock context")?;
        let inject_slots = slots.clone();
        let inject_dock = apply_dock.clone();
        let callback = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
            let options = object(&[
                ("name", JsValue::from_str("conversation.input.dock")),
                ("id", JsValue::from_str("todo")),
                ("order", JsValue::from_f64(0.0)),
                ("locale", JsValue::from_str(LOCALE_NAMESPACE)),
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
        ("name", JsValue::from_str("conversation-todo-dock")),
        ("inject", Array::of1(&JsValue::from_str("slots")).into()),
        ("apply", apply),
    ])?
    .into())
}

fn span(modules: &BrowserModules, class: &str, text: JsValue) -> Result<JsValue, JsValue> {
    create_element(
        &modules.react,
        &JsValue::from_str("span"),
        Some(&object(&[(
            "className",
            JsValue::from_str(&format!("seekdeep-conversation-todo-{class}")),
        )])?),
        &[text],
    )
}

fn configured_components() -> Result<TodoComponents, JsValue> {
    COMPONENTS.with(|components| {
        components.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation Todo panel was not configured").into()
        })
    })
}

fn javascript_string(value: &JsValue) -> Result<String, JsValue> {
    required_function(&js_sys::global(), "String", "global")?
        .call1(&JsValue::UNDEFINED, value)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("String() returned a non-string").into())
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
