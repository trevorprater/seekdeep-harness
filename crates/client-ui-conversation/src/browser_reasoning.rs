//! Compiled assistant reasoning disclosure and frame-throttled summary alignment.

use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Function, JsString, Object, Reflect};
use seekdeep_client_ui_primitives::{disclosure_row_component, icon_components};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

const REASONING_CSS: &str = include_str!(
    "../../../packages/client/ui-conversation/src/client/chat/ReasoningRow.module.css"
);
const ACCESSIBILITY_CSS: &str = include_str!(
    "../../../packages/client/ui-conversation/src/client/chat/accessibility.module.css"
);
const DEFAULT_INTERVAL_FRAMES: f64 = 3.0;

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    fragment: JsValue,
    disclosure_row: JsValue,
    think_icon: JsValue,
}

/// Configures the compiled reasoning surface over React and compiled ui-primitives.
///
/// # Errors
///
/// Returns on missing React hooks, missing primitive configuration, or stylesheet failure.
#[wasm_bindgen(js_name = configureClientUiConversationReasoning)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_conversation_reasoning(react: JsValue) -> Result<(), JsValue> {
    for method in [
        "createElement",
        "useCallback",
        "useEffect",
        "useLayoutEffect",
        "useRef",
        "useState",
    ] {
        required_function(&react, method, "React")?;
    }
    let fragment = required_property(&react, "Fragment", "React")?;
    let disclosure_row = disclosure_row_component()?;
    let icons = icon_components()?;
    let think_icon = required_property(&icons, "IconThinkOutline14", "ui-primitives icons")?;
    inject_style(
        "ReasoningRow",
        REASONING_CSS,
        &[
            ("thinkBody", "seekdeep-conversation-reasoning-thinkBody"),
            ("separator", "seekdeep-conversation-reasoning-separator"),
            ("chevron", "seekdeep-conversation-reasoning-chevron"),
            ("summary", "seekdeep-conversation-reasoning-summary"),
            ("leading", "seekdeep-conversation-reasoning-leading"),
            ("title", "seekdeep-conversation-reasoning-title"),
            ("root", "seekdeep-conversation-reasoning-root"),
            ("row", "seekdeep-conversation-reasoning-row"),
        ],
    )?;
    inject_style(
        "accessibility",
        ACCESSIBILITY_CSS,
        &[(
            "visuallyHidden",
            "seekdeep-conversation-accessibility-visuallyHidden",
        )],
    )?;
    MODULES.with(|modules| {
        *modules.borrow_mut() = Some(BrowserModules {
            react,
            fragment,
            disclosure_row,
            think_icon,
        });
    });
    Ok(())
}

/// Returns the compiled `ReasoningRow` component.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = reasoningRowComponent)]
pub fn reasoning_row_component() -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    Ok(Closure::wrap(
        Box::new(move |props: JsValue| render_reasoning_row(&modules, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value())
}

/// React hook implementing the source's stable three-frame visual scheduler.
///
/// # Errors
///
/// Returns before configuration or on missing browser frame APIs.
#[wasm_bindgen(js_name = useThrottledVisualUpdate)]
#[allow(clippy::needless_pass_by_value)] // wasm-bindgen owns the JavaScript callback argument.
pub fn use_throttled_visual_update(
    update: Function,
    interval_frames: Option<f64>,
) -> Result<Function, JsValue> {
    let modules = configured_modules()?;
    use_throttled_visual_update_with(
        &modules.react,
        &update,
        interval_frames.unwrap_or(DEFAULT_INTERVAL_FRAMES),
    )
}

#[allow(clippy::too_many_lines)] // Closed source component tree stays auditable in one renderer.
fn render_reasoning_row(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let react = &modules.react;
    let text = required_string(props, "text", "ReasoningRow props")?;
    let running = required_bool(props, "running", "ReasoningRow props")?;
    let translate = required_function(props, "t", "ReasoningRow props")?;
    let (expanded_value, set_expanded) = use_state(react, &JsValue::FALSE)?;
    let expanded = expanded_value.as_bool().unwrap_or(false);
    let summary_ref = use_ref(react, &JsValue::NULL)?;
    let summary = if running {
        latest_line(&text)
    } else {
        first_line(&text).to_owned()
    };

    let scroll_ref = summary_ref.clone();
    let scroll = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let element = current(&scroll_ref)?;
        if element.is_null() {
            return Ok(());
        }
        let target = if running {
            required_number(&element, "scrollWidth", "reasoning summary")?
                - required_number(&element, "clientWidth", "reasoning summary")?
        } else {
            0.0
        };
        Reflect::set(
            &element,
            &JsValue::from_str("scrollLeft"),
            &JsValue::from_f64(target),
        )?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value()
    .dyn_into::<Function>()?;
    let schedule = use_throttled_visual_update_with(react, &scroll, DEFAULT_INTERVAL_FRAMES)?;
    let effect_schedule = schedule.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        effect_schedule.call0(&JsValue::UNDEFINED)?;
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let effect_dependencies = Array::of3(
        &JsValue::from_bool(running),
        schedule.as_ref(),
        &JsValue::from_str(&summary),
    );
    required_function(react, "useEffect", "React")?.call2(
        react,
        &effect.into_js_value(),
        &effect_dependencies,
    )?;

    let toggle_setter = set_expanded;
    let on_toggle = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let updater = Closure::wrap(Box::new(move |value: JsValue| {
            JsValue::from_bool(value.as_bool() != Some(true))
        }) as Box<dyn FnMut(JsValue) -> JsValue>);
        toggle_setter
            .call1(&JsValue::UNDEFINED, &updater.into_js_value())
            .map(|_| ())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let icon = create_element(
        react,
        &modules.think_icon,
        Some(&object(&[("size", JsValue::from_f64(14.0))])?),
        &[],
    )?;
    let separator = create_element(
        react,
        &JsValue::from_str("span"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-reasoning-separator"),
            ),
            ("aria-hidden", JsValue::TRUE),
        ])?),
        &[],
    )?;
    let summary_element = create_element(
        react,
        &JsValue::from_str("span"),
        Some(&object(&[
            ("ref", summary_ref),
            (
                "className",
                JsValue::from_str("seekdeep-conversation-reasoning-summary"),
            ),
            (
                "data-follow-end",
                if running {
                    JsValue::TRUE
                } else {
                    JsValue::UNDEFINED
                },
            ),
        ])?),
        &[JsValue::from_str(&summary)],
    )?;
    let collapsed = create_element(
        react,
        &modules.fragment,
        None,
        &[separator, summary_element],
    )?;
    let body = create_element(
        react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-reasoning-thinkBody")?),
        &[JsValue::from_str(&text)],
    )?;
    let disclosure = create_element(
        react,
        &modules.disclosure_row,
        Some(&object(&[
            (
                "rowClassName",
                JsValue::from_str("seekdeep-conversation-reasoning-row"),
            ),
            (
                "leadingClassName",
                JsValue::from_str("seekdeep-conversation-reasoning-leading"),
            ),
            (
                "titleClassName",
                JsValue::from_str("seekdeep-conversation-reasoning-title"),
            ),
            (
                "chevronClassName",
                JsValue::from_str("seekdeep-conversation-reasoning-chevron"),
            ),
            ("icon", icon),
            ("title", JsValue::from_str("Think")),
            ("open", JsValue::from_bool(expanded)),
            ("expandable", JsValue::TRUE),
            ("expandOnRowClick", JsValue::TRUE),
            ("onToggle", on_toggle.into_js_value()),
            ("collapsedContent", collapsed),
        ])?),
        &[body],
    )?;
    let mut children = Vec::new();
    if running {
        children.push(create_element(
            react,
            &JsValue::from_str("span"),
            Some(&class_props(
                "seekdeep-conversation-accessibility-visuallyHidden",
            )?),
            &[translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("row.running"))?],
        )?);
    }
    children.push(disclosure);
    create_element(
        react,
        &JsValue::from_str("div"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-reasoning-root"),
            ),
            ("data-variant", JsValue::from_str("think")),
            (
                "data-state",
                JsValue::from_str(if running { "running" } else { "ok" }),
            ),
        ])?),
        &children,
    )
}

#[allow(clippy::too_many_lines)]
fn use_throttled_visual_update_with(
    react: &JsValue,
    update: &Function,
    interval_frames: f64,
) -> Result<Function, JsValue> {
    let update_ref = use_ref(react, update.as_ref())?;
    set_current(&update_ref, update.as_ref())?;
    let pending_ref = use_ref(react, &JsValue::NULL)?;
    let advance_ref = use_ref(react, &JsValue::NULL)?;

    let cleanup_pending = pending_ref.clone();
    let cleanup_advance = advance_ref.clone();
    let layout_effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let cleanup_pending = cleanup_pending.clone();
        let cleanup_advance = cleanup_advance.clone();
        let cleanup = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            let pending = current(&cleanup_pending)?;
            if !pending.is_null() {
                required_function(&js_sys::global(), "cancelAnimationFrame", "global")?
                    .call1(&js_sys::global(), &pending)?;
                set_current(&cleanup_pending, &JsValue::NULL)?;
            }
            set_current(&cleanup_advance, &JsValue::NULL)
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        Ok(cleanup.into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    required_function(react, "useLayoutEffect", "React")?.call2(
        react,
        &layout_effect.into_js_value(),
        &Array::new(),
    )?;

    let scheduler_pending = pending_ref;
    let scheduler_advance = advance_ref;
    let scheduler_update = update_ref;
    let scheduler = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if !current(&scheduler_pending)?.is_null() {
            return Ok(());
        }
        let remaining = Rc::new(std::cell::Cell::new(interval_frames));
        let advance_pending = scheduler_pending.clone();
        let advance_face = scheduler_advance.clone();
        let advance_update = scheduler_update.clone();
        let advance_remaining = remaining;
        let advance = Closure::wrap(Box::new(move |_timestamp: JsValue| -> Result<(), JsValue> {
            advance_remaining.set(advance_remaining.get() - 1.0);
            if advance_remaining.get() > 0.0 {
                let face = current(&advance_face)?.dyn_into::<Function>()?;
                let frame = request_animation_frame(&face)?;
                return set_current(&advance_pending, &frame);
            }
            set_current(&advance_pending, &JsValue::NULL)?;
            let update = current(&advance_update)?.dyn_into::<Function>()?;
            update.call0(&JsValue::UNDEFINED)?;
            set_current(&advance_face, &JsValue::NULL)
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value()
        .dyn_into::<Function>()?;
        set_current(&scheduler_advance, advance.as_ref())?;
        let frame = request_animation_frame(&advance)?;
        set_current(&scheduler_pending, &frame)
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let dependencies = Array::of1(&JsValue::from_f64(interval_frames));
    required_function(react, "useCallback", "React")?
        .call2(react, &scheduler.into_js_value(), &dependencies)?
        .dyn_into()
}

fn request_animation_frame(callback: &Function) -> Result<JsValue, JsValue> {
    required_function(&js_sys::global(), "requestAnimationFrame", "global")?
        .call1(&js_sys::global(), callback)
}

fn first_line(text: &str) -> &str {
    text.split_once('\n').map_or(text, |(head, _)| head)
}

fn latest_line(text: &str) -> String {
    let visible = String::from(JsString::from(text).trim_end());
    visible
        .rsplit_once('\n')
        .map_or(visible.clone(), |(_, tail)| tail.to_owned())
}

fn inject_style(name: &str, source: &str, replacements: &[(&str, &str)]) -> Result<(), JsValue> {
    let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))?;
    if document.is_null() || document.is_undefined() {
        return Ok(());
    }
    let tag = format!("@seekdeep-ai/seekdeep-client-ui-conversation/{name}.module.css");
    if let Ok(query) = Reflect::get(&document, &JsValue::from_str("querySelector"))
        .and_then(wasm_bindgen::JsCast::dyn_into::<Function>)
        && !query
            .call1(
                &document,
                &JsValue::from_str(&format!("[data-plugin-css=\"{tag}\"]")),
            )?
            .is_null()
    {
        return Ok(());
    }
    let mut css = source.to_owned();
    for (source, target) in replacements {
        css = css.replace(&format!(".{source}"), &format!(".{target}"));
    }
    let style = call_method(&document, "createElement", &[JsValue::from_str("style")])?;
    for (attribute, value) in [
        ("data-plugin-css", tag.as_str()),
        (
            "data-plugin",
            "@seekdeep-ai/seekdeep-client-ui-conversation",
        ),
    ] {
        call_method(
            &style,
            "setAttribute",
            &[JsValue::from_str(attribute), JsValue::from_str(value)],
        )?;
    }
    Reflect::set(
        &style,
        &JsValue::from_str("textContent"),
        &JsValue::from_str(&css),
    )?;
    let head = required_property(&document, "head", "document")?;
    call_method(&head, "appendChild", &[style])?;
    Ok(())
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation reasoning was not configured").into()
        })
    })
}

fn current(reference: &JsValue) -> Result<JsValue, JsValue> {
    Reflect::get(reference, &JsValue::from_str("current"))
}

fn set_current(reference: &JsValue, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(reference, &JsValue::from_str("current"), value).map(|_| ())
}

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    required_function(react, "useRef", "React")?.call1(react, initial)
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let pair = Array::from(&required_function(react, "useState", "React")?.call1(react, initial)?);
    Ok((pair.get(0), pair.get(1).dyn_into()?))
}

fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
}

fn required_bool(value: &JsValue, key: &str, owner: &str) -> Result<bool, JsValue> {
    required_property(value, key, owner)?
        .as_bool()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a boolean")).into())
}

fn required_number(value: &JsValue, key: &str, owner: &str) -> Result<f64, JsValue> {
    required_property(value, key, owner)?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a number")).into())
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
