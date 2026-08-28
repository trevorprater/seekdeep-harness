//! Compiled React renderers for simple trajectory rows and headers.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{SIMPLE_COMPONENT_STYLES, TRAJECTORY_TABLE_STYLES, format_elapsed_seconds};

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    primitives: Option<JsValue>,
}

/// Configures React and injects the compiled simple-component stylesheet once.
///
/// # Errors
///
/// Returns DOM stylesheet construction failures.
#[wasm_bindgen(js_name = configureClientUiTrajectory)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_trajectory(react: JsValue) -> Result<(), JsValue> {
    MODULES.with(|slot| {
        *slot.borrow_mut() = Some(BrowserModules {
            react,
            primitives: None,
        });
    });
    inject_styles()
}

/// Configures React plus shared UI primitives for the complete component set.
///
/// # Errors
///
/// Returns DOM stylesheet construction failures.
#[wasm_bindgen(js_name = configureClientUiTrajectoryModules)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_trajectory_modules(
    react: JsValue,
    primitives: JsValue,
) -> Result<(), JsValue> {
    MODULES.with(|slot| {
        *slot.borrow_mut() = Some(BrowserModules {
            react,
            primitives: Some(primitives),
        });
    });
    inject_styles()
}

/// Returns the compiled `TrajectoryCell` React component.
///
/// # Errors
///
/// Returns before module configuration.
#[wasm_bindgen(js_name = trajectoryCellComponent)]
pub fn trajectory_cell_component() -> Result<JsValue, JsValue> {
    let ui = ui()?;
    Ok(
        Closure::wrap(Box::new(move |props: JsValue| render_cell(&ui, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
        .into_js_value(),
    )
}

/// Returns the compiled `TrajectoryGroupHeader` React component.
///
/// # Errors
///
/// Returns before module configuration.
#[wasm_bindgen(js_name = trajectoryGroupHeaderComponent)]
pub fn trajectory_group_header_component() -> Result<JsValue, JsValue> {
    let ui = ui()?;
    Ok(Closure::wrap(
        Box::new(move |props: JsValue| render_group_header(&ui, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value())
}

/// Returns the compiled `TrajectoryTurnHeader` React component.
///
/// # Errors
///
/// Returns before module configuration.
#[wasm_bindgen(js_name = trajectoryTurnHeaderComponent)]
pub fn trajectory_turn_header_component() -> Result<JsValue, JsValue> {
    let ui = ui()?;
    Ok(Closure::wrap(
        Box::new(move |props: JsValue| render_turn_header(&ui, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value())
}

/// Returns the compiled `TrajectoryTurn` React component.
///
/// # Errors
///
/// Returns before module configuration.
#[wasm_bindgen(js_name = trajectoryTurnComponent)]
pub fn trajectory_turn_component() -> Result<JsValue, JsValue> {
    let ui = ui()?;
    Ok(
        Closure::wrap(Box::new(move |props: JsValue| render_turn(&ui, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
        .into_js_value(),
    )
}

/// Returns the compiled `TrajectoryToolbar` React component.
///
/// # Errors
///
/// Returns before React and UI-primitives configuration.
#[wasm_bindgen(js_name = trajectoryToolbarComponent)]
pub fn trajectory_toolbar_component() -> Result<JsValue, JsValue> {
    let ui = ui()?;
    if ui.primitives.is_none() {
        return Err(
            js_sys::Error::new("client-ui-trajectory Toolbar requires UI primitives").into(),
        );
    }
    Ok(
        Closure::wrap(Box::new(move |props: JsValue| render_toolbar(&ui, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
        .into_js_value(),
    )
}

#[allow(clippy::too_many_lines)] // Exact prop stripping and row geometry stay auditable together.
fn render_cell(ui: &ReactUi, props: &JsValue) -> Result<JsValue, JsValue> {
    const CONSUMED: &[&str] = &[
        "index",
        "kind",
        "text",
        "inputDetail",
        "promptDetail",
        "previousPromptDetail",
        "outputDetail",
        "thinkingDetail",
        "sourceBlocks",
        "outputBlocks",
        "schemaDetail",
        "assistantMetrics",
        "result",
        "callId",
        "isError",
        "timeSeconds",
        "startedAt",
        "input",
        "output",
        "think",
        "selected",
        "className",
    ];
    let index = required(props, "index", "TrajectoryCell")?;
    let kind = required_string(props, "kind", "TrajectoryCell")?;
    let text = required(props, "text", "TrajectoryCell")?;
    let selected = optional(props, "selected")?
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let custom_class = optional(props, "className")?.and_then(|value| value.as_string());
    let mut classes = vec!["seekdeep-trajectory-cell"];
    if selected {
        classes.push("seekdeep-trajectory-cell-selected");
    }
    let root_props = clone_rest(props, CONSUMED)?;
    set(
        &root_props,
        "className",
        &JsValue::from_str(&match custom_class {
            Some(custom) => format!("{} {custom}", classes.join(" ")),
            None => classes.join(" "),
        }),
    )?;
    set(&root_props, "data-kind", &JsValue::from_str(&kind))?;
    let selected_value = if selected {
        JsValue::TRUE
    } else {
        JsValue::UNDEFINED
    };
    set(&root_props, "data-selected", &selected_value)?;

    let label = kind_label(&kind)?;
    let tag = ui.tag(
        "span",
        Some(&object(&[(
            "className",
            JsValue::from_str(&format!(
                "seekdeep-trajectory-tag {}",
                kind_tag_class(&kind)?
            )),
        )])?),
        &[JsValue::from_str(label)],
    )?;
    let tag_slot = ui.tag(
        "span",
        Some(&class("seekdeep-trajectory-tag-slot")?),
        &[tag],
    )?;
    let index = ui.tag(
        "span",
        Some(&class("seekdeep-trajectory-index")?),
        &[JsValue::from_str(&format!(
            "#{}",
            js_number_string(&index)?
        ))],
    )?;
    let text = ui.tag("span", Some(&class("seekdeep-trajectory-text")?), &[text])?;
    let mut trailing = Vec::new();
    if kind == "message" {
        for key in ["input", "output", "think"] {
            trailing.push(ui.tag(
                "span",
                Some(&class("seekdeep-trajectory-metric")?),
                &[nullish(optional(props, key)?)],
            )?);
        }
    }
    let seconds = optional(props, "timeSeconds")?
        .and_then(|value| (!value.is_null()).then(|| value.as_f64()).flatten());
    trailing.push(ui.tag(
        "span",
        Some(&class("seekdeep-trajectory-time")?),
        &[JsValue::from_str(&format_elapsed_seconds(seconds))],
    )?);
    let trailing = ui.tag(
        "span",
        Some(&class("seekdeep-trajectory-trailing")?),
        &trailing,
    )?;
    ui.tag("div", Some(&root_props), &[index, tag_slot, text, trailing])
}

fn render_group_header(ui: &ReactUi, props: &JsValue) -> Result<JsValue, JsValue> {
    let title = required(props, "title", "TrajectoryGroupHeader")?;
    let description = optional(props, "description")?;
    let mut children = vec![ui.tag(
        "span",
        Some(&class("seekdeep-trajectory-group-title")?),
        &[title],
    )?];
    if let Some(description) =
        description.filter(|value| value.as_string().is_some_and(|text| !text.is_empty()))
    {
        children.push(ui.tag(
            "span",
            Some(&class("seekdeep-trajectory-group-description")?),
            &[description],
        )?);
    }
    ui.tag(
        "div",
        Some(&class("seekdeep-trajectory-group-header")?),
        &children,
    )
}

fn render_turn_header(ui: &ReactUi, props: &JsValue) -> Result<JsValue, JsValue> {
    let turn = required(props, "turn", "TrajectoryTurnHeader")?;
    let title = ui.tag(
        "span",
        Some(&class("seekdeep-trajectory-turn-title")?),
        &[JsValue::from_str(&format!(
            "Turn {}",
            js_number_string(&turn)?
        ))],
    )?;
    let columns = ["Input", "Output", "Think", "Time"]
        .into_iter()
        .map(|label| {
            ui.tag(
                "span",
                Some(&class("seekdeep-trajectory-turn-column")?),
                &[JsValue::from_str(label)],
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let columns = ui.tag(
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-turn-columns"),
            ),
            ("aria-hidden", JsValue::TRUE),
        ])?),
        &columns,
    )?;
    let inner = ui.tag(
        "div",
        Some(&class("seekdeep-trajectory-turn-header-inner")?),
        &[title, columns],
    )?;
    ui.tag(
        "div",
        Some(&class("seekdeep-trajectory-turn-header")?),
        &[inner],
    )
}

fn render_turn(ui: &ReactUi, props: &JsValue) -> Result<JsValue, JsValue> {
    let turn = required(props, "turn", "TrajectoryTurn")?;
    let header_props = object(&[("turn", turn.clone())])?;
    let header = render_turn_header(ui, header_props.as_ref())?;
    let body = ui.tag(
        "div",
        Some(&class("seekdeep-trajectory-turn-body")?),
        &[optional(props, "children")?.unwrap_or(JsValue::UNDEFINED)],
    )?;
    let section_props = object(&[
        ("className", JsValue::from_str("seekdeep-trajectory-turn")),
        ("data-turn", turn),
    ])?;
    ui.tag("section", Some(&section_props), &[header, body])
}

#[allow(clippy::too_many_lines)] // Source toolbar controls remain one auditable render surface.
fn render_toolbar(ui: &ReactUi, props: &JsValue) -> Result<JsValue, JsValue> {
    let actual_duration = required_bool(props, "actualDuration", "TrajectoryToolbar")?;
    let actual_time = required_bool(props, "actualTime", "TrajectoryToolbar")?;
    let turns_collapsed = required_bool(props, "allTurnsCollapsed", "TrajectoryToolbar")?;
    let assistants_collapsed = required_bool(props, "allAssistantsCollapsed", "TrajectoryToolbar")?;
    let search_query = required(props, "searchQuery", "TrajectoryToolbar")?;
    let translate = required(props, "t", "TrajectoryToolbar")?.dyn_into::<Function>()?;

    let duration_change =
        required(props, "onActualDurationChange", "TrajectoryToolbar")?.dyn_into::<Function>()?;
    let duration_click = Closure::wrap(Box::new(move || {
        duration_change.call1(&JsValue::UNDEFINED, &JsValue::from_bool(!actual_duration))
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let duration_label = translated(&translate, "toolbar.useActualDuration")?;
    let duration_title = translated(
        &translate,
        if actual_duration {
            "toolbar.useEqualWidth"
        } else {
            "toolbar.useActualDuration"
        },
    )?;
    let circle = ui.tag(
        "circle",
        Some(&object(&[
            ("cx", JsValue::from_str("8")),
            ("cy", JsValue::from_str("8")),
            ("r", JsValue::from_str("5.25")),
        ])?),
        &[],
    )?;
    let path = ui.tag(
        "path",
        Some(&object(&[("d", JsValue::from_str("M8 4.75V8l2.25 1.5"))])?),
        &[],
    )?;
    let clock = ui.tag(
        "svg",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-toolbar-toggle-icon"),
            ),
            ("viewBox", JsValue::from_str("0 0 16 16")),
            ("fill", JsValue::from_str("none")),
            ("aria-hidden", JsValue::TRUE),
        ])?),
        &[circle, path],
    )?;
    let duration = ui.tag(
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-toolbar-toggle"),
            ),
            ("aria-label", duration_label),
            ("aria-pressed", JsValue::from_bool(actual_duration)),
            ("title", duration_title),
            ("onClick", duration_click.into_js_value()),
        ])?),
        &[clock, translated(&translate, "toolbar.duration")?],
    )?;

    let time_change =
        required(props, "onActualTimeChange", "TrajectoryToolbar")?.dyn_into::<Function>()?;
    let time_click = Closure::wrap(Box::new(move || {
        time_change.call1(&JsValue::UNDEFINED, &JsValue::from_bool(!actual_time))
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let thumb = ui.tag(
        "span",
        Some(&class("seekdeep-trajectory-toolbar-control-thumb")?),
        &[],
    )?;
    let track = ui.tag(
        "span",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-toolbar-control-track"),
            ),
            (
                "data-on",
                if actual_time {
                    JsValue::TRUE
                } else {
                    JsValue::UNDEFINED
                },
            ),
            ("aria-hidden", JsValue::TRUE),
        ])?),
        &[thumb],
    )?;
    let actual_time_control = ui.tag(
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-toolbar-control"),
            ),
            ("role", JsValue::from_str("switch")),
            ("aria-checked", JsValue::from_bool(actual_time)),
            ("hidden", JsValue::TRUE),
            ("onClick", time_click.into_js_value()),
        ])?),
        &[translated(&translate, "toolbar.actualTime")?, track],
    )?;
    let turns = toolbar_action(
        ui,
        props,
        &translate,
        "onToggleAllTurns",
        turns_collapsed,
        "toolbar.expandTurns",
        "toolbar.collapseTurns",
        "toolbar.turns",
    )?;
    let calls = toolbar_action(
        ui,
        props,
        &translate,
        "onToggleAllAssistants",
        assistants_collapsed,
        "toolbar.expandCalls",
        "toolbar.collapseCalls",
        "toolbar.calls",
    )?;
    let actions = ui.tag(
        "div",
        Some(&class("seekdeep-trajectory-toolbar-actions")?),
        &[duration, actual_time_control, turns, calls],
    )?;

    let icon = ui.primitive(
        "IconSearchOutline16",
        Some(&object(&[
            ("size", JsValue::from_f64(11.0)),
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-toolbar-search-icon"),
            ),
        ])?),
        &[],
    )?;
    let search_change =
        required(props, "onSearchQueryChange", "TrajectoryToolbar")?.dyn_into::<Function>()?;
    let on_change = Closure::wrap(Box::new(move |event: JsValue| {
        let current = required(&event, "currentTarget", "search event")?;
        let value = required(&current, "value", "search input")?;
        search_change.call1(&JsValue::UNDEFINED, &value)
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    let input = ui.tag(
        "input",
        Some(&object(&[
            ("type", JsValue::from_str("search")),
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-toolbar-search-input"),
            ),
            ("aria-label", translated(&translate, "toolbar.search")?),
            (
                "placeholder",
                translated(&translate, "toolbar.searchPlaceholder")?,
            ),
            ("value", search_query),
            ("onChange", on_change.into_js_value()),
        ])?),
        &[],
    )?;
    let search = ui.tag(
        "div",
        Some(&class("seekdeep-trajectory-toolbar-search")?),
        &[icon, input],
    )?;
    let inner = ui.tag(
        "div",
        Some(&class("seekdeep-trajectory-toolbar-inner")?),
        &[actions, search],
    )?;
    ui.tag(
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-toolbar"),
            ),
            ("role", JsValue::from_str("toolbar")),
            ("aria-label", translated(&translate, "toolbar.aria")?),
        ])?),
        &[inner],
    )
}

#[allow(clippy::too_many_arguments)]
fn toolbar_action(
    ui: &ReactUi,
    props: &JsValue,
    translate: &Function,
    callback: &str,
    collapsed: bool,
    expand_key: &str,
    collapse_key: &str,
    text_key: &str,
) -> Result<JsValue, JsValue> {
    let callback = required(props, callback, "TrajectoryToolbar")?;
    let label = translated(translate, if collapsed { expand_key } else { collapse_key })?;
    let icon = ui.tag(
        "span",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-toolbar-action-icon"),
            ),
            ("aria-hidden", JsValue::TRUE),
        ])?),
        &[JsValue::from_str(if collapsed { "⊞" } else { "⊟" })],
    )?;
    ui.tag(
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-toolbar-action"),
            ),
            ("aria-label", label.clone()),
            ("aria-pressed", JsValue::from_bool(collapsed)),
            ("title", label),
            ("onClick", callback),
        ])?),
        &[icon, translated(translate, text_key)?],
    )
}

fn inject_styles() -> Result<(), JsValue> {
    const PACKAGE: &str = "@seekdeep-ai/seekdeep-client-ui-trajectory";
    const TAG: &str = "@seekdeep-ai/seekdeep-client-ui-trajectory/simple-components.css";
    let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))?;
    if document.is_null() || document.is_undefined() {
        return Ok(());
    }
    let selector = format!(
        "style[data-plugin-css={}]",
        serde_json::to_string(TAG).expect("static selector")
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
    call_method(
        &style,
        "setAttribute",
        &[JsValue::from_str("data-plugin-css"), JsValue::from_str(TAG)],
    )?;
    Reflect::set(
        &style,
        &JsValue::from_str("textContent"),
        &JsValue::from_str(&format!(
            "{SIMPLE_COMPONENT_STYLES}\n{TRAJECTORY_TABLE_STYLES}"
        )),
    )?;
    let head = required(&document, "head", "document")?;
    call_method(&head, "appendChild", &[style])?;
    Ok(())
}

fn ui() -> Result<ReactUi, JsValue> {
    MODULES.with(|modules| {
        modules
            .borrow()
            .clone()
            .map(|modules| ReactUi {
                react: modules.react,
                primitives: modules.primitives,
            })
            .ok_or_else(|| js_sys::Error::new("client-ui-trajectory is not configured").into())
    })
}

pub(crate) fn trajectory_browser_modules() -> Result<(JsValue, Option<JsValue>), JsValue> {
    MODULES.with(|modules| {
        modules
            .borrow()
            .clone()
            .map(|modules| (modules.react, modules.primitives))
            .ok_or_else(|| js_sys::Error::new("client-ui-trajectory is not configured").into())
    })
}

#[derive(Clone)]
struct ReactUi {
    react: JsValue,
    primitives: Option<JsValue>,
}

impl ReactUi {
    fn tag(
        &self,
        name: &str,
        props: Option<&Object>,
        children: &[JsValue],
    ) -> Result<JsValue, JsValue> {
        let arguments = Array::new();
        arguments.push(&JsValue::from_str(name));
        arguments.push(props.map_or(&JsValue::NULL, AsRef::as_ref));
        for child in children {
            arguments.push(child);
        }
        function(&self.react, "createElement")?.apply(&self.react, &arguments)
    }

    fn primitive(
        &self,
        name: &str,
        props: Option<&Object>,
        children: &[JsValue],
    ) -> Result<JsValue, JsValue> {
        let primitives = self.primitives.as_ref().ok_or_else(|| {
            js_sys::Error::new("client-ui-trajectory UI primitives are not configured")
        })?;
        let kind = required(primitives, name, "UI primitives")?;
        let arguments = Array::new();
        arguments.push(&kind);
        arguments.push(props.map_or(&JsValue::NULL, AsRef::as_ref));
        for child in children {
            arguments.push(child);
        }
        function(&self.react, "createElement")?.apply(&self.react, &arguments)
    }
}

fn clone_rest(value: &JsValue, excluded: &[&str]) -> Result<Object, JsValue> {
    let output = Object::new();
    for key in Reflect::own_keys(value)?.iter() {
        if key
            .as_string()
            .is_some_and(|key| excluded.contains(&key.as_str()))
        {
            continue;
        }
        Reflect::set(&output, &key, &Reflect::get(value, &key)?)?;
    }
    Ok(output)
}

fn kind_label(kind: &str) -> Result<&'static str, JsValue> {
    match kind {
        "system" => Ok("System"),
        "user" => Ok("User"),
        "context" => Ok("Context"),
        "compacted" => Ok("Compacted"),
        "message" => Ok("Message"),
        "tool" => Ok("Tool"),
        "subtool" => Ok("Sub"),
        _ => Err(js_sys::Error::new(&format!("unknown trajectory cell kind {kind:?}")).into()),
    }
}

fn kind_tag_class(kind: &str) -> Result<&'static str, JsValue> {
    match kind {
        "system" | "compacted" => Ok("seekdeep-trajectory-tag-system"),
        "user" => Ok("seekdeep-trajectory-tag-user"),
        "context" => Ok("seekdeep-trajectory-tag-context"),
        "message" => Ok("seekdeep-trajectory-tag-message"),
        "tool" => Ok("seekdeep-trajectory-tag-tool"),
        "subtool" => Ok("seekdeep-trajectory-tag-subtool"),
        _ => Err(js_sys::Error::new(&format!("unknown trajectory cell kind {kind:?}")).into()),
    }
}

fn js_number_string(value: &JsValue) -> Result<String, JsValue> {
    let value = value
        .as_f64()
        .ok_or_else(|| js_sys::Error::new("trajectory numeric prop must be a number"))?;
    Ok(if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    })
}

fn nullish(value: Option<JsValue>) -> JsValue {
    value
        .filter(|value| !value.is_null() && !value.is_undefined())
        .unwrap_or_else(|| JsValue::from_str(""))
}

fn class(value: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(value))])
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        set(&object, key, value)?;
    }
    Ok(object)
}

fn optional(value: &JsValue, key: &str) -> Result<Option<JsValue>, JsValue> {
    let value = Reflect::get(value, &JsValue::from_str(key))?;
    Ok((!value.is_undefined()).then_some(value))
}

fn required(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    optional(value, key)?.ok_or_else(|| {
        js_sys::Error::new(&format!("{owner} omitted required property {key:?}")).into()
    })
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::Error::new(&format!("{owner} {key:?} must be a string")).into())
}

fn required_bool(value: &JsValue, key: &str, owner: &str) -> Result<bool, JsValue> {
    required(value, key, owner)?
        .as_bool()
        .ok_or_else(|| js_sys::Error::new(&format!("{owner} {key:?} must be a boolean")).into())
}

fn translated(translate: &Function, key: &str) -> Result<JsValue, JsValue> {
    translate.call1(&JsValue::UNDEFINED, &JsValue::from_str(key))
}

fn function(value: &JsValue, key: &str) -> Result<Function, JsValue> {
    required(value, key, "object")?.dyn_into::<Function>()
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(object, &JsValue::from_str(key), value).map(|_| ())
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}
