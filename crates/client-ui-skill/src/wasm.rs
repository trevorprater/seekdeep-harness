//! Browser Skill row, catalog adapter, and Client plugin registration.

mod catalog_adapter;

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_tool::{ToolCallBlock, ToolCallHead, ToolErrorInfo};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{SKILL_ROW_STYLES, SkillRowModel, SkillRowState, skill_row_model};

pub(crate) const INJECT: &[&str] = &[
    "inputTriggers",
    "connection",
    "sessions",
    "slots",
    "locale",
    "remote",
];

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
pub(crate) struct BrowserModules {
    pub(crate) react: JsValue,
    chevron_down: JsValue,
    inspect_icon: JsValue,
    skill_icon: JsValue,
    state_dot: JsValue,
}

/// Configures React, UI primitives, and the compiled stylesheet.
///
/// # Errors
///
/// Returns missing primitive or DOM stylesheet-injection failures.
#[wasm_bindgen(js_name = configureClientUiSkill)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_skill(react: JsValue, primitives: JsValue) -> Result<(), JsValue> {
    MODULES.with(|modules| {
        *modules.borrow_mut() = Some(BrowserModules {
            react,
            chevron_down: required(&primitives, "IconChevronDownOutline14", "UI primitives")?,
            inspect_icon: required(&primitives, "IconInspectOutline12", "UI primitives")?,
            skill_icon: required(&primitives, "IconSkillOutline16", "UI primitives")?,
            state_dot: required(&primitives, "StateDot", "UI primitives")?,
        });
        Ok::<_, JsValue>(())
    })?;
    inject_styles()
}

/// Applies the Skill browser plugin.
///
/// # Errors
///
/// Returns missing service, registration, catalog, or component failures.
#[wasm_bindgen(js_name = applyClientUiSkill)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_client_ui_skill(ctx: JsValue) -> Result<(), JsValue> {
    catalog_adapter::apply(&configured_modules()?, &ctx)
}

/// Returns the exact browser dependency order.
#[wasm_bindgen(js_name = skillInject)]
pub fn skill_inject() -> Array {
    let values = Array::new();
    for dependency in INJECT {
        values.push(&JsValue::from_str(dependency));
    }
    values
}

/// Returns the compiled `SkillRow` component.
///
/// # Errors
///
/// Returns before browser modules are configured.
#[wasm_bindgen(js_name = skillRowComponent)]
pub fn exported_skill_row_component() -> Result<JsValue, JsValue> {
    Ok(skill_row_component(&configured_modules()?))
}

pub(crate) fn skill_row_component(modules: &BrowserModules) -> JsValue {
    let modules = modules.clone();
    Closure::wrap(
        Box::new(move |props: JsValue| render_skill_row(&modules, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value()
}

#[allow(clippy::too_many_lines)]
fn render_skill_row(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let block = parse_block(&required(props, "block", "SkillRow")?)?;
    let model = skill_row_model(&block);
    let inspect = Reflect::get(props, &JsValue::from_str("inspect"))?;
    let translate = required_function(props, "t", "SkillRow")?;
    let (expanded, set_expanded) = use_state(&modules.react, &JsValue::FALSE)?;
    let expandable = model.output.is_some();
    let open = expanded.as_bool().unwrap_or(false) && expandable;
    let status = state_status(model.state, &translate)?;
    let summary = model
        .error_summary
        .as_deref()
        .unwrap_or(&model.name)
        .to_owned();
    let leading = render_disclosure_leading(modules, model.state, open, expandable)?;

    let toggle_setter = set_expanded.clone();
    let toggle = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let update = Closure::wrap(Box::new(|value: bool| !value) as Box<dyn FnMut(bool) -> bool>);
        toggle_setter.call1(&JsValue::UNDEFINED, &update.into_js_value())?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    let keyboard_setter = set_expanded;
    let keydown = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let key = Reflect::get(&event, &JsValue::from_str("key"))?.as_string();
        if !expandable || !matches!(key.as_deref(), Some("Enter" | " ")) {
            return Ok(());
        }
        call_method(&event, "preventDefault", &[])?;
        let update = Closure::wrap(Box::new(|value: bool| !value) as Box<dyn FnMut(bool) -> bool>);
        keyboard_setter.call1(&JsValue::UNDEFINED, &update.into_js_value())?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
    .into_js_value();

    let mut row_children = vec![tag(
        &modules.react,
        "span",
        Some(&class("seekdeep-skill-leading")?),
        &[leading],
    )?];
    if let Some(status) = status {
        row_children.push(tag(
            &modules.react,
            "span",
            Some(&class("seekdeep-skill-visuallyHidden")?),
            &[JsValue::from_str(&status)],
        )?);
    }
    row_children.push(tag(
        &modules.react,
        "span",
        Some(&class("seekdeep-skill-title")?),
        &[JsValue::from_str("Skill")],
    )?);
    row_children.push(tag(
        &modules.react,
        "span",
        Some(&object(&[
            ("className", JsValue::from_str("seekdeep-skill-separator")),
            ("aria-hidden", JsValue::TRUE),
        ])?),
        &[],
    )?);
    row_children.push(tag(
        &modules.react,
        "span",
        Some(&class(if model.error_summary.is_some() {
            "seekdeep-skill-summary seekdeep-skill-errorSummary"
        } else {
            "seekdeep-skill-summary"
        })?),
        &[JsValue::from_str(&summary)],
    )?);
    let row = tag(
        &modules.react,
        "div",
        Some(&object(&[
            ("className", JsValue::from_str("seekdeep-skill-row")),
            (
                "data-expandable",
                if expandable {
                    JsValue::TRUE
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "role",
                if expandable {
                    JsValue::from_str("button")
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "tabIndex",
                if expandable {
                    JsValue::from_f64(0.0)
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "aria-expanded",
                if expandable {
                    JsValue::from_bool(open)
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "onClick",
                if expandable {
                    toggle
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "onKeyDown",
                if expandable {
                    keydown
                } else {
                    JsValue::UNDEFINED
                },
            ),
        ])?),
        &row_children,
    )?;
    let mut children = vec![row];
    if open {
        children.push(render_body(modules, &model, &inspect, &translate)?);
    }
    tag(
        &modules.react,
        "div",
        Some(&object(&[
            ("className", JsValue::from_str("seekdeep-skill-card")),
            ("data-tool", JsValue::from_str("skill")),
            ("data-state", JsValue::from_str(row_state_name(model.state))),
        ])?),
        &children,
    )
}

fn render_disclosure_leading(
    modules: &BrowserModules,
    state: SkillRowState,
    open: bool,
    expandable: bool,
) -> Result<JsValue, JsValue> {
    if open {
        return component(
            &modules.react,
            &modules.chevron_down,
            Some(&class("seekdeep-skill-chevron")?),
            &[],
        );
    }
    let icon = match state {
        SkillRowState::Error => component(
            &modules.react,
            &modules.state_dot,
            Some(&object(&[("state", JsValue::from_str("error"))])?),
            &[],
        )?,
        SkillRowState::Stopped => component(
            &modules.react,
            &modules.state_dot,
            Some(&object(&[("state", JsValue::from_str("warning"))])?),
            &[],
        )?,
        SkillRowState::Running | SkillRowState::Ok => component(
            &modules.react,
            &modules.skill_icon,
            Some(&object(&[("size", JsValue::from_f64(14.0))])?),
            &[],
        )?,
    };
    if !expandable {
        return Ok(icon);
    }
    fragment(
        &modules.react,
        &[
            tag(
                &modules.react,
                "span",
                Some(&class("seekdeep-skill-iconIdle")?),
                &[icon],
            )?,
            component(
                &modules.react,
                &modules.chevron_down,
                Some(&class(
                    "seekdeep-skill-chevron seekdeep-skill-chevronHover",
                )?),
                &[],
            )?,
        ],
    )
}

fn render_body(
    modules: &BrowserModules,
    model: &SkillRowModel,
    inspect: &JsValue,
    translate: &Function,
) -> Result<JsValue, JsValue> {
    let instructions = translated(translate, "row.instructions")?;
    let section = tag(
        &modules.react,
        "section",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-skill-instructionsCard"),
            ),
            ("aria-label", instructions.clone()),
        ])?),
        &[
            tag(
                &modules.react,
                "div",
                Some(&class("seekdeep-skill-instructionsHeader")?),
                std::slice::from_ref(&instructions),
            )?,
            tag(
                &modules.react,
                "pre",
                Some(&object(&[
                    (
                        "className",
                        JsValue::from_str("seekdeep-skill-instructions"),
                    ),
                    (
                        "data-error",
                        if model.state == SkillRowState::Error {
                            JsValue::TRUE
                        } else {
                            JsValue::UNDEFINED
                        },
                    ),
                ])?),
                &[JsValue::from_str(
                    model.output.as_deref().unwrap_or_default(),
                )],
            )?,
        ],
    )?;
    let mut children = vec![section];
    if inspect.is_function() {
        let invoke = inspect.clone().dyn_into::<Function>()?;
        let click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            invoke.call0(&JsValue::UNDEFINED)?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        children.push(tag(
            &modules.react,
            "button",
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                (
                    "className",
                    JsValue::from_str("seekdeep-skill-inspectButton"),
                ),
                ("onClick", click.into_js_value()),
            ])?),
            &[
                component(&modules.react, &modules.inspect_icon, None, &[])?,
                JsValue::from_str("Inspect"),
            ],
        )?);
    }
    tag(
        &modules.react,
        "div",
        Some(&class("seekdeep-skill-bodyWrap")?),
        &children,
    )
}

fn parse_block(value: &JsValue) -> Result<ToolCallBlock, JsValue> {
    let call_id = required_string(value, "callId", "Skill tool block")?;
    if !Reflect::has(value, &JsValue::from_str("kind"))? {
        return Ok(ToolCallBlock::Running {
            call_id,
            args_raw: required_string(value, "argsRaw", "running Skill call")?,
            call_view: None,
        });
    }
    let call = Reflect::get(value, &JsValue::from_str("call"))?;
    let call = if call.is_null() || call.is_undefined() {
        None
    } else {
        Some(ToolCallHead {
            args_raw: required_string(&call, "argsRaw", "settled Skill call")?,
        })
    };
    let content = serde_wasm_bindgen::from_value::<Vec<serde_json::Value>>(required(
        value,
        "content",
        "settled Skill result",
    )?)
    .map_err(js_error_from_display)?;
    let error = Reflect::get(value, &JsValue::from_str("error"))?;
    let error = if error.is_null() || error.is_undefined() {
        None
    } else {
        Some(ToolErrorInfo {
            name: required_string(&error, "name", "Skill result error")?,
            code: required_string(&error, "code", "Skill result error")?,
        })
    };
    Ok(ToolCallBlock::Settled {
        call_id,
        call,
        call_view: None,
        result_view: None,
        content,
        is_error: required_bool(value, "isError", "settled Skill result")?,
        error,
    })
}

fn state_status(state: SkillRowState, translate: &Function) -> Result<Option<String>, JsValue> {
    let key = match state {
        SkillRowState::Running => Some("row.running"),
        SkillRowState::Error => Some("row.failed"),
        SkillRowState::Stopped => Some("row.stopped"),
        SkillRowState::Ok => None,
    };
    key.map(|key| {
        translated(translate, key)?
            .as_string()
            .ok_or_else(|| js_sys::Error::new("Skill state must translate to a string").into())
    })
    .transpose()
}

const fn row_state_name(state: SkillRowState) -> &'static str {
    match state {
        SkillRowState::Running => "running",
        SkillRowState::Ok => "ok",
        SkillRowState::Error => "error",
        SkillRowState::Stopped => "stopped",
    }
}

fn inject_styles() -> Result<(), JsValue> {
    const PACKAGE: &str = "@seekdeep-ai/seekdeep-client-ui-skill";
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
        &JsValue::from_str(SKILL_ROW_STYLES),
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
            .ok_or_else(|| js_sys::Error::new("client-ui-skill is not configured").into())
    })
}

pub(crate) fn translated(translate: &Function, key: &str) -> Result<JsValue, JsValue> {
    translate.call1(&JsValue::UNDEFINED, &JsValue::from_str(key))
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

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let pair = Array::from(&required_function(react, "useState", "React")?.call1(react, initial)?);
    Ok((pair.get(0), pair.get(1).dyn_into()?))
}

pub(crate) fn class(value: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(value))])
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

pub(crate) fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::Error::new(&format!("{owner} {key:?} must be a string")).into())
}

fn required_bool(value: &JsValue, key: &str, owner: &str) -> Result<bool, JsValue> {
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

pub(crate) fn js_error_from_display(error: impl std::fmt::Display) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}
