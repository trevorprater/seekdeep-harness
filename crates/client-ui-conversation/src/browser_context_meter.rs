//! Compiled composer context-occupancy ring and breakdown panel.

use std::cell::RefCell;

use js_sys::{Array, Function, JsString, Math, Number, Object, Reflect, Symbol};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::browser_reasoning::inject_style;

const CONTEXT_CSS: &str = include_str!(
    "../../../packages/client/ui-conversation/src/client/skeleton/ContextMeter.module.css"
);
const RADIUS: f64 = 5.5;
const CIRCUMFERENCE: f64 = 2.0 * std::f64::consts::PI * RADIUS;
const READING_SLOT: &str = "\0";

thread_local! {
    static COMPONENT: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    tooltip: JsValue,
}

#[derive(Clone, Copy)]
struct Occupancy {
    percent: f64,
    used_tokens: f64,
    context_window: f64,
}

#[derive(Clone, Copy)]
struct Breakdown {
    system: f64,
    tools: f64,
    messages: f64,
}

#[derive(Clone, Copy)]
struct Row {
    key: &'static str,
    label: &'static str,
    color: &'static str,
}

const ROWS: [Row; 3] = [
    Row {
        key: "systemTokens",
        label: "context.system",
        color: "seekdeep-conversation-contextMeter-colorSystem",
    },
    Row {
        key: "toolsTokens",
        label: "context.tools",
        color: "seekdeep-conversation-contextMeter-colorTools",
    },
    Row {
        key: "messageTokens",
        label: "context.messages",
        color: "seekdeep-conversation-contextMeter-colorMessages",
    },
];

/// Configures the compiled context meter.
///
/// # Errors
///
/// Returns on missing React/Tooltip faces or stylesheet failure.
#[wasm_bindgen(js_name = configureClientUiConversationContextMeter)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_conversation_context_meter(
    react: JsValue,
    ui_primitives: JsValue,
) -> Result<(), JsValue> {
    for method in ["createElement", "useEffect", "useRef", "useState"] {
        required_function(&react, method, "React")?;
    }
    let modules = BrowserModules {
        tooltip: required_property(&ui_primitives, "Tooltip", "ui-primitives")?,
        react,
    };
    inject_style(
        "ContextMeter",
        CONTEXT_CSS,
        &[
            ("bar", "seekdeep-conversation-contextMeter-bar"),
            (
                "colorMessages",
                "seekdeep-conversation-contextMeter-colorMessages",
            ),
            (
                "colorSystem",
                "seekdeep-conversation-contextMeter-colorSystem",
            ),
            (
                "colorTools",
                "seekdeep-conversation-contextMeter-colorTools",
            ),
            ("figures", "seekdeep-conversation-contextMeter-figures"),
            ("fill", "seekdeep-conversation-contextMeter-fill"),
            ("header", "seekdeep-conversation-contextMeter-header"),
            ("headline", "seekdeep-conversation-contextMeter-headline"),
            ("panel", "seekdeep-conversation-contextMeter-panel"),
            ("percent", "seekdeep-conversation-contextMeter-percent"),
            ("root", "seekdeep-conversation-contextMeter-root"),
            ("row", "seekdeep-conversation-contextMeter-row"),
            ("rows", "seekdeep-conversation-contextMeter-rows"),
            ("segment", "seekdeep-conversation-contextMeter-segment"),
            ("swatch", "seekdeep-conversation-contextMeter-swatch"),
            ("track", "seekdeep-conversation-contextMeter-track"),
            ("trigger", "seekdeep-conversation-contextMeter-trigger"),
        ],
    )?;
    let component =
        Closure::wrap(
            Box::new(move |props: JsValue| render_context_meter(&modules, &props))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        )
        .into_js_value();
    COMPONENT.with(|configured| *configured.borrow_mut() = Some(component));
    Ok(())
}

/// Returns the compiled `ContextMeter` component.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = contextMeterComponent)]
pub fn context_meter_component() -> Result<JsValue, JsValue> {
    COMPONENT.with(|component| {
        component.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation ContextMeter was not configured").into()
        })
    })
}

#[allow(clippy::too_many_lines)] // Closed meter tree and Hook order stay auditable together.
fn render_context_meter(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let use_projection = required_function(props, "useProjection", "ContextMeter props")?;
    let pressure =
        use_projection.call1(&JsValue::UNDEFINED, &JsValue::from_str("contextPressure"))?;
    let breakdown_value =
        use_projection.call1(&JsValue::UNDEFINED, &JsValue::from_str("contextBreakdown"))?;
    let state = required_function(&modules.react, "useState", "React")?
        .call1(&modules.react, &JsValue::FALSE)?
        .dyn_into::<Array>()?;
    let open = state
        .get(0)
        .as_bool()
        .ok_or_else(|| js_sys::TypeError::new("ContextMeter open state must be a boolean"))?;
    let set_open = state.get(1).dyn_into::<Function>()?;
    let root_ref = required_function(&modules.react, "useRef", "React")?
        .call1(&modules.react, &JsValue::NULL)?;
    let context = context_occupancy(&pressure)?;
    let available = context.is_some();
    install_availability_effect(&modules.react, available, open, &set_open)?;
    install_outside_effect(&modules.react, available, open, &set_open, &root_ref)?;
    let Some(context) = context else {
        return Ok(JsValue::NULL);
    };
    let breakdown = if breakdown_value.is_undefined() {
        None
    } else {
        Some(parse_breakdown(&breakdown_value)?)
    };
    let percent = context.percent;
    let reading = format!("{}%", number_string(percent)?);
    let translate = required_function(props, "t", "ContextMeter props")?;
    let headline = translate.apply(
        &JsValue::UNDEFINED,
        &Array::of2(
            &JsValue::from_str("context.aria"),
            object(&[("percent", JsValue::from_str(READING_SLOT))])?.as_ref(),
        ),
    )?;
    let headline = headline
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("context aria translation must be a string"))?;
    let mut headline_parts = headline.split(READING_SLOT);
    let before = trim_js(headline_parts.next().unwrap_or_default());
    let after = trim_js(headline_parts.next().unwrap_or_default());
    let tooltip_label = context_aria(&translate, &reading)?;
    let button_label = context_aria(&translate, &reading)?;
    let toggle_setter = set_open.clone();
    let toggle = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        toggle_setter.call1(&JsValue::UNDEFINED, &JsValue::from_bool(!open))?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    let trigger = render_trigger(modules, open, percent, button_label, toggle)?;
    let tooltip = create_element(
        &modules.react,
        &modules.tooltip,
        Some(&object(&[
            ("label", tooltip_label),
            ("side", JsValue::from_str("top")),
            ("delayMs", JsValue::from_f64(200.0)),
            ("disabled", JsValue::from_bool(open)),
        ])?),
        &[trigger],
    )?;
    let panel = if open {
        render_panel(
            modules, &translate, context, breakdown, &before, &after, &reading,
        )?
    } else {
        JsValue::FALSE
    };
    create_element(
        &modules.react,
        &JsValue::from_str("span"),
        Some(&object(&[
            ("ref", root_ref),
            (
                "className",
                JsValue::from_str("seekdeep-conversation-contextMeter-root"),
            ),
        ])?),
        &[tooltip, panel],
    )
}

fn context_occupancy(pressure: &JsValue) -> Result<Option<Occupancy>, JsValue> {
    if pressure.is_null() || pressure.is_undefined() {
        return Ok(None);
    }
    let projected = Reflect::get(pressure, &JsValue::from_str("projectedTokens"))?;
    let used = if projected.is_null() || projected.is_undefined() {
        Reflect::get(pressure, &JsValue::from_str("pressureTokens"))?
    } else {
        projected
    };
    let window = Reflect::get(pressure, &JsValue::from_str("contextWindow"))?;
    if used.is_undefined() || window.is_undefined() {
        return Ok(None);
    }
    let used_tokens = javascript_number(&used)?;
    let context_window = javascript_number(&window)?;
    Ok(Some(Occupancy {
        percent: Math::min(100.0, Math::round(used_tokens / context_window * 100.0)),
        used_tokens,
        context_window,
    }))
}

fn parse_breakdown(value: &JsValue) -> Result<Breakdown, JsValue> {
    Ok(Breakdown {
        system: javascript_number(&Reflect::get(value, &JsValue::from_str("systemTokens"))?)?,
        tools: javascript_number(&Reflect::get(value, &JsValue::from_str("toolsTokens"))?)?,
        messages: javascript_number(&Reflect::get(value, &JsValue::from_str("messageTokens"))?)?,
    })
}

fn install_availability_effect(
    react: &JsValue,
    available: bool,
    open: bool,
    set_open: &Function,
) -> Result<(), JsValue> {
    let setter = set_open.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if !available && open {
            setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        }
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    required_function(react, "useEffect", "React")?.call2(
        react,
        &effect.into_js_value(),
        &Array::of2(&JsValue::from_bool(available), &JsValue::from_bool(open)),
    )?;
    Ok(())
}

fn install_outside_effect(
    react: &JsValue,
    available: bool,
    open: bool,
    set_open: &Function,
    root_ref: &JsValue,
) -> Result<(), JsValue> {
    let setter = set_open.clone();
    let root = root_ref.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if !open || !available {
            return Ok(JsValue::UNDEFINED);
        }
        let document = required_property(&js_sys::global(), "document", "global")?;
        let pointer_setter = setter.clone();
        let pointer_root = root.clone();
        let pointer = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            let target = Reflect::get(&event, &JsValue::from_str("target"))?;
            if is_node(&target)? {
                let current = Reflect::get(&pointer_root, &JsValue::from_str("current"))?;
                if !current.is_null()
                    && call_method(&current, "contains", std::slice::from_ref(&target))?.as_bool()
                        == Some(true)
                {
                    return Ok(());
                }
            }
            pointer_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value();
        let key_setter = setter.clone();
        let keydown = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            if Reflect::get(&event, &JsValue::from_str("key"))?
                .as_string()
                .as_deref()
                == Some("Escape")
            {
                key_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
            }
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value();
        add_listener(&document, "pointerdown", &pointer)?;
        add_listener(&document, "keydown", &keydown)?;
        Ok(Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            remove_listener(&document, "pointerdown", &pointer)?;
            remove_listener(&document, "keydown", &keydown)?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    required_function(react, "useEffect", "React")?.call2(
        react,
        &effect.into_js_value(),
        &Array::of2(&JsValue::from_bool(available), &JsValue::from_bool(open)),
    )?;
    Ok(())
}

fn render_trigger(
    modules: &BrowserModules,
    open: bool,
    percent: f64,
    label: JsValue,
    on_click: JsValue,
) -> Result<JsValue, JsValue> {
    let track = create_element(
        &modules.react,
        &JsValue::from_str("circle"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-contextMeter-track"),
            ),
            ("cx", JsValue::from_f64(7.0)),
            ("cy", JsValue::from_f64(7.0)),
            ("r", JsValue::from_f64(RADIUS)),
        ])?),
        &[],
    )?;
    let dash = format!(
        "{} {}",
        number_string(CIRCUMFERENCE * percent / 100.0)?,
        number_string(CIRCUMFERENCE)?
    );
    let fill = create_element(
        &modules.react,
        &JsValue::from_str("circle"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-contextMeter-fill"),
            ),
            ("cx", JsValue::from_f64(7.0)),
            ("cy", JsValue::from_f64(7.0)),
            ("r", JsValue::from_f64(RADIUS)),
            ("strokeDasharray", JsValue::from_str(&dash)),
            ("transform", JsValue::from_str("rotate(-90 7 7)")),
        ])?),
        &[],
    )?;
    let svg = create_element(
        &modules.react,
        &JsValue::from_str("svg"),
        Some(&object(&[
            ("viewBox", JsValue::from_str("0 0 14 14")),
            ("width", JsValue::from_f64(14.0)),
            ("height", JsValue::from_f64(14.0)),
            ("aria-hidden", JsValue::TRUE),
        ])?),
        &[track, fill],
    )?;
    create_element(
        &modules.react,
        &JsValue::from_str("button"),
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str("seekdeep-conversation-contextMeter-trigger"),
            ),
            ("aria-label", label),
            ("aria-haspopup", JsValue::from_str("dialog")),
            ("aria-expanded", JsValue::from_bool(open)),
            ("onClick", on_click),
        ])?),
        &[svg],
    )
}

#[allow(clippy::float_cmp)] // Source distinguishes an exact zero aggregate before proportional division.
fn render_panel(
    modules: &BrowserModules,
    translate: &Function,
    context: Occupancy,
    breakdown: Option<Breakdown>,
    before: &str,
    after: &str,
    reading: &str,
) -> Result<JsValue, JsValue> {
    let panel_label = translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("context.used"))?;
    let figures = format!(
        "~{} / {}",
        format_tokens(context.used_tokens)?,
        format_tokens(context.context_window)?
    );
    let header = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-contextMeter-header")?),
        &[
            span(modules, "headline", JsValue::from_str(before))?,
            span(modules, "percent", JsValue::from_str(reading))?,
            span(modules, "headline", JsValue::from_str(after))?,
            span(modules, "figures", JsValue::from_str(&figures))?,
        ],
    )?;
    let total = breakdown.map_or(0.0, |value| value.system + value.tools + value.messages);
    let mut segments = Vec::new();
    if breakdown.is_none() || total == 0.0 {
        if context.percent > 0.0 {
            segments.push(segment(modules, "total", None, context.percent)?);
        }
    } else if let Some(values) = breakdown {
        for (row, value) in ROWS
            .iter()
            .zip([values.system, values.tools, values.messages])
        {
            let width = context.percent * value / total;
            if width > 0.0 {
                segments.push(segment(modules, row.key, Some(row.color), width)?);
            }
        }
    }
    let bar = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-contextMeter-bar")?),
        &segments,
    )?;
    let rows = if let Some(values) = breakdown {
        render_rows(modules, translate, values)?
    } else {
        JsValue::FALSE
    };
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-contextMeter-panel"),
            ),
            ("role", JsValue::from_str("dialog")),
            ("aria-label", panel_label),
        ])?),
        &[header, bar, rows],
    )
}

fn render_rows(
    modules: &BrowserModules,
    translate: &Function,
    values: Breakdown,
) -> Result<JsValue, JsValue> {
    let numbers = [values.system, values.tools, values.messages];
    let mut rows = Vec::new();
    for (row, value) in ROWS.iter().zip(numbers) {
        let term = create_element(
            &modules.react,
            &JsValue::from_str("dt"),
            None,
            &[
                create_element(
                    &modules.react,
                    &JsValue::from_str("span"),
                    Some(&object(&[
                        (
                            "className",
                            JsValue::from_str(&format!(
                                "seekdeep-conversation-contextMeter-swatch {}",
                                row.color
                            )),
                        ),
                        ("aria-hidden", JsValue::TRUE),
                    ])?),
                    &[],
                )?,
                translate.call1(&JsValue::UNDEFINED, &JsValue::from_str(row.label))?,
            ],
        )?;
        let definition = create_element(
            &modules.react,
            &JsValue::from_str("dd"),
            None,
            &[JsValue::from_str(&format!("~{}", format_tokens(value)?))],
        )?;
        rows.push(create_element(
            &modules.react,
            &JsValue::from_str("div"),
            Some(&object(&[
                ("key", JsValue::from_str(row.key)),
                (
                    "className",
                    JsValue::from_str("seekdeep-conversation-contextMeter-row"),
                ),
            ])?),
            &[term, definition],
        )?);
    }
    create_element(
        &modules.react,
        &JsValue::from_str("dl"),
        Some(&class_props("seekdeep-conversation-contextMeter-rows")?),
        &rows,
    )
}

fn segment(
    modules: &BrowserModules,
    key: &str,
    color: Option<&str>,
    width: f64,
) -> Result<JsValue, JsValue> {
    let class_name = color.map_or_else(
        || "seekdeep-conversation-contextMeter-segment".to_owned(),
        |color| format!("seekdeep-conversation-contextMeter-segment {color}"),
    );
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            ("key", JsValue::from_str(key)),
            ("className", JsValue::from_str(&class_name)),
            (
                "style",
                object(&[(
                    "width",
                    JsValue::from_str(&format!("{}%", number_string(width)?)),
                )])?
                .into(),
            ),
        ])?),
        &[],
    )
}

fn span(modules: &BrowserModules, class: &str, text: JsValue) -> Result<JsValue, JsValue> {
    create_element(
        &modules.react,
        &JsValue::from_str("span"),
        Some(&class_props(&format!(
            "seekdeep-conversation-contextMeter-{class}"
        ))?),
        &[text],
    )
}

fn context_aria(translate: &Function, reading: &str) -> Result<JsValue, JsValue> {
    translate.apply(
        &JsValue::UNDEFINED,
        &Array::of2(
            &JsValue::from_str("context.aria"),
            object(&[("percent", JsValue::from_str(reading))])?.as_ref(),
        ),
    )
}

fn format_tokens(value: f64) -> Result<String, JsValue> {
    if value < 1_000.0 {
        return number_string(value);
    }
    if value < 1_000_000.0 {
        return Ok(format!("{}K", scaled(value / 1_000.0)?));
    }
    Ok(format!("{}M", scaled(value / 1_000_000.0)?))
}

fn scaled(value: f64) -> Result<String, JsValue> {
    number_string(if value >= 100.0 {
        Math::round(value)
    } else {
        Math::round(value * 10.0) / 10.0
    })
}

fn number_string(value: f64) -> Result<String, JsValue> {
    Number::from(value)
        .to_string_with_radix(10)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("Number.toString() returned a non-string").into())
}

fn javascript_number(value: &JsValue) -> Result<f64, JsValue> {
    required_function(&js_sys::global(), "Number", "global")?
        .call1(&JsValue::UNDEFINED, value)?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new("Number() returned a non-number").into())
}

fn trim_js(value: &str) -> String {
    String::from(JsString::from(value).trim())
}

fn is_node(value: &JsValue) -> Result<bool, JsValue> {
    let constructor = required_property(&js_sys::global(), "Node", "global")?;
    let has_instance =
        Reflect::get(&constructor, Symbol::has_instance().as_ref())?.dyn_into::<Function>()?;
    Ok(has_instance
        .call1(&constructor, value)?
        .as_bool()
        .unwrap_or(false))
}

fn add_listener(target: &JsValue, name: &str, listener: &JsValue) -> Result<(), JsValue> {
    call_method(
        target,
        "addEventListener",
        &[JsValue::from_str(name), listener.clone()],
    )?;
    Ok(())
}

fn remove_listener(target: &JsValue, name: &str, listener: &JsValue) -> Result<(), JsValue> {
    call_method(
        target,
        "removeEventListener",
        &[JsValue::from_str(name), listener.clone()],
    )?;
    Ok(())
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

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let arguments: Array = arguments.iter().collect();
    method.apply(value, &arguments)
}
