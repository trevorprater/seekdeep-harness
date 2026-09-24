//! Compiled copy, branch, clock, and calendar-day message actions.

use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Date, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    browser_reasoning::inject_style, format_latency_seconds_browser, format_message_clock_browser,
    format_run_duration_browser, format_tokens_per_second_browser,
    milliseconds_until_next_local_midnight_browser, start_of_local_day_browser,
};

const ACTIONS_CSS: &str = include_str!(
    "../../../packages/client/ui-conversation/src/client/chat/MessageIconActions.module.css"
);

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
    static COMPONENT: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    fragment: JsValue,
    tooltip: JsValue,
    branch_icon: JsValue,
    check_icon: JsValue,
    copy_icon: JsValue,
    write_clipboard: Function,
}

/// Configures the compiled message actions and local-calendar hook.
///
/// # Errors
///
/// Returns on missing React hooks, missing ui-primitives faces, or stylesheet failures.
#[wasm_bindgen(js_name = configureClientUiConversationMessageActions)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_conversation_message_actions(
    react: JsValue,
    ui_primitives: JsValue,
) -> Result<(), JsValue> {
    for method in [
        "createElement",
        "useCallback",
        "useEffect",
        "useId",
        "useRef",
        "useState",
    ] {
        required_function(&react, method, "React")?;
    }
    let modules = BrowserModules {
        fragment: required_property(&react, "Fragment", "React")?,
        tooltip: required_property(&ui_primitives, "Tooltip", "ui-primitives")?,
        branch_icon: required_property(&ui_primitives, "IconBranchOutline16", "ui-primitives")?,
        check_icon: required_property(&ui_primitives, "IconCheckOutline16", "ui-primitives")?,
        copy_icon: required_property(&ui_primitives, "IconCopyOutline16", "ui-primitives")?,
        write_clipboard: required_function(&ui_primitives, "writeClipboard", "ui-primitives")?,
        react,
    };
    inject_style(
        "MessageIconActions",
        ACTIONS_CSS,
        &[
            ("action", "seekdeep-conversation-messageActions-action"),
            ("actions", "seekdeep-conversation-messageActions-actions"),
            (
                "runTimeDot",
                "seekdeep-conversation-messageActions-runTimeDot",
            ),
            ("timeEnd", "seekdeep-conversation-messageActions-timeEnd"),
            (
                "timeStart",
                "seekdeep-conversation-messageActions-timeStart",
            ),
            (
                "visuallyHidden",
                "seekdeep-conversation-messageActions-visuallyHidden",
            ),
        ],
    )?;
    MODULES.with(|configured| *configured.borrow_mut() = Some(modules.clone()));
    let component =
        Closure::wrap(
            Box::new(move |props: JsValue| render_message_actions(&modules, &props))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        )
        .into_js_value();
    COMPONENT.with(|configured| *configured.borrow_mut() = Some(component));
    Ok(())
}

/// Returns the compiled `MessageIconActions` component.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = messageIconActionsComponent)]
pub fn message_icon_actions_component() -> Result<JsValue, JsValue> {
    COMPONENT.with(|component| {
        component.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation message actions were not configured").into()
        })
    })
}

/// React hook returning the current local calendar day's midnight epoch.
///
/// # Errors
///
/// Returns before configuration or when React/browser timer methods fail.
#[wasm_bindgen(js_name = useCalendarDay)]
pub fn use_calendar_day_browser() -> Result<f64, JsValue> {
    use_calendar_day(&configured_modules()?.react)
}

#[allow(clippy::too_many_lines)] // Closed source component and hook order stay auditable together.
fn render_message_actions(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let day = use_calendar_day(&modules.react)?;
    let reason_id = required_function(&modules.react, "useId", "React")?
        .call0(&modules.react)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("React useId did not return a string"))?;
    let copied_state = required_function(&modules.react, "useState", "React")?
        .call1(&modules.react, &JsValue::FALSE)?
        .dyn_into::<Array>()?;
    let copied = copied_state
        .get(0)
        .as_bool()
        .ok_or_else(|| js_sys::TypeError::new("React copied state value was not a boolean"))?;
    let set_copied = copied_state.get(1).dyn_into::<Function>()?;
    let copy_pending = use_ref(&modules.react, &JsValue::FALSE)?;
    let copy_timer = use_ref(&modules.react, &JsValue::NULL)?;
    let copy_epoch = use_ref(&modules.react, &JsValue::from_f64(0.0))?;
    install_copy_cleanup(&modules.react, &copy_pending, &copy_timer, &copy_epoch)?;
    let text = required_string(props, "text", "MessageIconActions props")?;
    let on_copy = use_copy_callback(
        modules,
        copied,
        &text,
        &set_copied,
        &copy_pending,
        &copy_timer,
        &copy_epoch,
    )?;
    let translate = required_function(props, "t", "MessageIconActions props")?;
    let clock = required_string(props, "clock", "MessageIconActions props")?;
    let clock_element = render_clock(modules, props, &translate, &clock, day)?;
    let copy_tooltip = render_copy_action(modules, &translate, copied, on_copy)?;
    let extra_actions = Reflect::get(props, &JsValue::from_str("extraActions"))?;
    let on_branch = Reflect::get(props, &JsValue::from_str("onBranch"))?;
    let branch_unavailable = Reflect::get(props, &JsValue::from_str("branchUnavailable"))?
        .as_bool()
        .unwrap_or(false);
    let mut children = vec![
        if clock == "start" {
            clock_element.clone()
        } else {
            JsValue::NULL
        },
        copy_tooltip,
        extra_actions,
    ];
    if on_branch.is_undefined() {
        children.push(JsValue::FALSE);
    } else {
        children.push(render_branch_action(
            modules,
            &translate,
            &reason_id,
            &on_branch,
            branch_unavailable,
        )?);
    }
    if !on_branch.is_undefined() && branch_unavailable {
        children.push(create_element(
            &modules.react,
            &JsValue::from_str("span"),
            Some(&object(&[
                ("id", JsValue::from_str(&reason_id)),
                (
                    "className",
                    JsValue::from_str("seekdeep-conversation-messageActions-visuallyHidden"),
                ),
            ])?),
            &[translate_value(
                &translate,
                "message.branchUnavailable",
                None,
            )?],
        )?);
    } else {
        children.push(JsValue::FALSE);
    }
    children.push(if clock == "end" {
        clock_element
    } else {
        JsValue::NULL
    });
    let parent_class = Reflect::get(props, &JsValue::from_str("className"))?;
    let class_name = if parent_class.is_undefined() {
        "seekdeep-conversation-messageActions-actions".to_owned()
    } else {
        format!(
            "seekdeep-conversation-messageActions-actions {}",
            javascript_string(&parent_class)?
        )
    };
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[("className", JsValue::from_str(&class_name))])?),
        &children,
    )
}

#[allow(clippy::float_cmp, clippy::too_many_arguments)] // Exact integer epoch equality rejects post-unmount settlement.
fn use_copy_callback(
    modules: &BrowserModules,
    copied: bool,
    text: &str,
    set_copied: &Function,
    copy_pending: &JsValue,
    copy_timer: &JsValue,
    copy_epoch: &JsValue,
) -> Result<Function, JsValue> {
    let pending_ref = copy_pending.clone();
    let timer_ref = copy_timer.clone();
    let epoch_ref = copy_epoch.clone();
    let setter = set_copied.clone();
    let write_clipboard = modules.write_clipboard.clone();
    let copy_text = text.to_owned();
    let callback = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if copied || current(&pending_ref)?.as_bool().unwrap_or(false) {
            return Ok(());
        }
        let epoch = current_number(&epoch_ref, "copy epoch")?;
        set_current(&pending_ref, &JsValue::TRUE)?;
        let pending = write_clipboard
            .call1(&JsValue::UNDEFINED, &JsValue::from_str(&copy_text))?
            .dyn_into::<Promise>()?;
        let settled_pending = pending_ref.clone();
        let settled_timer = timer_ref.clone();
        let settled_epoch = epoch_ref.clone();
        let settled_setter = setter.clone();
        let settled = Closure::wrap(Box::new(move |ok: JsValue| -> Result<(), JsValue> {
            if epoch != current_number(&settled_epoch, "copy epoch")? {
                return Ok(());
            }
            set_current(&settled_pending, &JsValue::FALSE)?;
            if !ok.as_bool().unwrap_or(false) {
                return Ok(());
            }
            settled_setter.call1(&JsValue::UNDEFINED, &JsValue::TRUE)?;
            let timer_setter = settled_setter.clone();
            let timer_slot = settled_timer.clone();
            let reset = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                set_current(&timer_slot, &JsValue::NULL)?;
                timer_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
                Ok(())
            }) as Box<dyn FnMut() -> Result<(), JsValue>>)
            .into_js_value()
            .dyn_into::<Function>()?;
            let timer = window_set_timeout(&reset, 1_000.0)?;
            set_current(&settled_timer, &timer)
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value()
        .dyn_into::<Function>()?;
        required_function(pending.as_ref(), "then", "writeClipboard Promise")?
            .call1(pending.as_ref(), settled.as_ref())?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let dependencies = Array::of2(&JsValue::from_bool(copied), &JsValue::from_str(text));
    required_function(&modules.react, "useCallback", "React")?
        .call2(&modules.react, &callback.into_js_value(), &dependencies)?
        .dyn_into()
}

fn install_copy_cleanup(
    react: &JsValue,
    copy_pending: &JsValue,
    copy_timer: &JsValue,
    copy_epoch: &JsValue,
) -> Result<(), JsValue> {
    let pending_ref = copy_pending.clone();
    let timer_ref = copy_timer.clone();
    let epoch_ref = copy_epoch.clone();
    let effect = Closure::wrap(Box::new(move || -> JsValue {
        let cleanup_pending = pending_ref.clone();
        let cleanup_timer = timer_ref.clone();
        let cleanup_epoch = epoch_ref.clone();
        Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            let epoch = current_number(&cleanup_epoch, "copy epoch")?;
            set_current(&cleanup_epoch, &JsValue::from_f64(epoch + 1.0))?;
            set_current(&cleanup_pending, &JsValue::FALSE)?;
            let timer = current(&cleanup_timer)?;
            if !timer.is_null() {
                global_clear_timeout(&timer)?;
            }
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value()
    }) as Box<dyn FnMut() -> JsValue>);
    required_function(react, "useEffect", "React")?.call2(
        react,
        &effect.into_js_value(),
        &Array::new(),
    )?;
    Ok(())
}

fn render_copy_action(
    modules: &BrowserModules,
    translate: &Function,
    copied: bool,
    on_copy: Function,
) -> Result<JsValue, JsValue> {
    let label_key = if copied { "copied" } else { "copy" };
    let tooltip_label = translate_value(translate, label_key, None)?;
    let aria_label = translate_value(translate, label_key, None)?;
    let icon = create_element(
        &modules.react,
        if copied {
            &modules.check_icon
        } else {
            &modules.copy_icon
        },
        None,
        &[],
    )?;
    let button = create_element(
        &modules.react,
        &JsValue::from_str("button"),
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str("seekdeep-conversation-messageActions-action"),
            ),
            ("aria-label", aria_label),
            ("onClick", on_copy.into()),
        ])?),
        &[icon],
    )?;
    create_element(
        &modules.react,
        &modules.tooltip,
        Some(&object(&[
            ("label", tooltip_label),
            ("side", JsValue::from_str("bottom")),
        ])?),
        &[button],
    )
}

fn render_branch_action(
    modules: &BrowserModules,
    translate: &Function,
    reason_id: &str,
    on_branch: &JsValue,
    unavailable: bool,
) -> Result<JsValue, JsValue> {
    let tooltip_label = translate_value(
        translate,
        if unavailable {
            "message.branchUnavailable"
        } else {
            "message.branch"
        },
        None,
    )?;
    let aria_label = translate_value(translate, "message.branch", None)?;
    let icon = create_element(&modules.react, &modules.branch_icon, None, &[])?;
    let button = create_element(
        &modules.react,
        &JsValue::from_str("button"),
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str("seekdeep-conversation-messageActions-action"),
            ),
            ("aria-label", aria_label),
            (
                "aria-disabled",
                if unavailable {
                    JsValue::TRUE
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "aria-describedby",
                if unavailable {
                    JsValue::from_str(reason_id)
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "data-unavailable",
                if unavailable {
                    JsValue::TRUE
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "onClick",
                if unavailable {
                    JsValue::UNDEFINED
                } else {
                    on_branch.clone()
                },
            ),
        ])?),
        &[icon],
    )?;
    create_element(
        &modules.react,
        &modules.tooltip,
        Some(&object(&[
            ("label", tooltip_label),
            ("side", JsValue::from_str("bottom")),
        ])?),
        &[button],
    )
}

fn render_clock(
    modules: &BrowserModules,
    props: &JsValue,
    translate: &Function,
    clock: &str,
    day: f64,
) -> Result<JsValue, JsValue> {
    let time = Reflect::get(props, &JsValue::from_str("time"))?;
    if time.is_undefined() {
        return Ok(JsValue::NULL);
    }
    let time = javascript_number(&time)?;
    let mut children = vec![JsValue::from_str(&format_message_clock_browser(
        time,
        translate.clone(),
        Some(day),
    )?)];
    append_run_metric(modules, props, translate, &mut children)?;
    append_ttft_metric(modules, props, translate, &mut children)?;
    append_throughput_metric(modules, props, translate, &mut children)?;
    create_element(
        &modules.react,
        &JsValue::from_str("span"),
        Some(&object(&[(
            "className",
            JsValue::from_str(if clock == "start" {
                "seekdeep-conversation-messageActions-timeStart"
            } else {
                "seekdeep-conversation-messageActions-timeEnd"
            }),
        )])?),
        &children,
    )
}

fn append_run_metric(
    modules: &BrowserModules,
    props: &JsValue,
    translate: &Function,
    children: &mut Vec<JsValue>,
) -> Result<(), JsValue> {
    let value = Reflect::get(props, &JsValue::from_str("runMs"))?;
    if value.is_undefined() {
        children.push(JsValue::FALSE);
        return Ok(());
    }
    let duration = format_run_duration_browser(javascript_number(&value)?, translate.clone())?;
    append_metric(
        modules,
        children,
        translate_value(
            translate,
            "message.ranFor",
            Some(&object(&[("duration", duration)])?),
        )?,
    )
}

fn append_ttft_metric(
    modules: &BrowserModules,
    props: &JsValue,
    translate: &Function,
    children: &mut Vec<JsValue>,
) -> Result<(), JsValue> {
    let value = Reflect::get(props, &JsValue::from_str("ttftMs"))?;
    if value.is_undefined() {
        children.push(JsValue::FALSE);
        return Ok(());
    }
    let seconds = format_latency_seconds_browser(javascript_number(&value)?)?;
    append_metric(
        modules,
        children,
        translate_value(
            translate,
            "message.ttft",
            Some(&object(&[("seconds", JsValue::from_str(&seconds))])?),
        )?,
    )
}

fn append_throughput_metric(
    modules: &BrowserModules,
    props: &JsValue,
    translate: &Function,
    children: &mut Vec<JsValue>,
) -> Result<(), JsValue> {
    let value = Reflect::get(props, &JsValue::from_str("tokensPerSecond"))?;
    if value.is_undefined() {
        children.push(JsValue::FALSE);
        return Ok(());
    }
    let tokens = format_tokens_per_second_browser(javascript_number(&value)?)?;
    append_metric(
        modules,
        children,
        translate_value(
            translate,
            "message.tokensPerSecond",
            Some(&object(&[("tps", JsValue::from_str(&tokens))])?),
        )?,
    )
}

fn append_metric(
    modules: &BrowserModules,
    children: &mut Vec<JsValue>,
    label: JsValue,
) -> Result<(), JsValue> {
    children.push(create_element(
        &modules.react,
        &modules.fragment,
        None,
        &[
            JsValue::from_str(" "),
            create_element(
                &modules.react,
                &JsValue::from_str("span"),
                Some(&object(&[
                    (
                        "className",
                        JsValue::from_str("seekdeep-conversation-messageActions-runTimeDot"),
                    ),
                    ("aria-hidden", JsValue::TRUE),
                ])?),
                &[JsValue::from_str("·")],
            )?,
            JsValue::from_str(" "),
            label,
        ],
    )?);
    Ok(())
}

fn use_calendar_day(react: &JsValue) -> Result<f64, JsValue> {
    let initializer = Closure::wrap(Box::new(move || start_of_local_day_browser(Date::now()))
        as Box<dyn FnMut() -> Result<f64, JsValue>>);
    let state = required_function(react, "useState", "React")?
        .call1(react, &initializer.into_js_value())?
        .dyn_into::<Array>()?;
    let day = required_number(&state.get(0), "React calendar-day state")?;
    let set_day = state.get(1).dyn_into::<Function>()?;
    install_calendar_effect(react, set_day)?;
    Ok(day)
}

fn install_calendar_effect(react: &JsValue, set_day: Function) -> Result<(), JsValue> {
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let timer = Rc::new(RefCell::new(JsValue::UNDEFINED));
        let arm = Rc::new(RefCell::<Option<Function>>::new(None));
        let arm_timer = timer.clone();
        let arm_face = arm.clone();
        let arm_set_day = set_day.clone();
        let callback = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            let now = Date::now();
            arm_set_day.call1(
                &JsValue::UNDEFINED,
                &JsValue::from_f64(start_of_local_day_browser(now)?),
            )?;
            let face = arm_face
                .borrow()
                .clone()
                .ok_or_else(|| js_sys::Error::new("calendar-day timer fired after cleanup"))?;
            let delay = milliseconds_until_next_local_midnight_browser(now)?;
            *arm_timer.borrow_mut() = global_set_timeout(&face, delay)?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value()
        .dyn_into::<Function>()?;
        *arm.borrow_mut() = Some(callback.clone());
        *timer.borrow_mut() = global_set_timeout(
            &callback,
            milliseconds_until_next_local_midnight_browser(Date::now())?,
        )?;
        let cleanup_timer = timer;
        let cleanup_arm = arm;
        Ok(Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            global_clear_timeout(&cleanup_timer.borrow())?;
            cleanup_arm.borrow_mut().take();
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    required_function(react, "useEffect", "React")?.call2(
        react,
        &effect.into_js_value(),
        &Array::new(),
    )?;
    Ok(())
}

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    required_function(react, "useRef", "React")?.call1(react, initial)
}

fn current(reference: &JsValue) -> Result<JsValue, JsValue> {
    Reflect::get(reference, &JsValue::from_str("current"))
}

fn current_number(reference: &JsValue, owner: &str) -> Result<f64, JsValue> {
    required_number(&current(reference)?, owner)
}

fn set_current(reference: &JsValue, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(reference, &JsValue::from_str("current"), value)?;
    Ok(())
}

fn global_set_timeout(callback: &Function, delay: f64) -> Result<JsValue, JsValue> {
    required_function(&js_sys::global(), "setTimeout", "global")?.call2(
        &js_sys::global(),
        callback,
        &JsValue::from_f64(delay),
    )
}

fn window_set_timeout(callback: &Function, delay: f64) -> Result<JsValue, JsValue> {
    let window = required_property(&js_sys::global(), "window", "global")?;
    required_function(&window, "setTimeout", "window")?.call2(
        &window,
        callback,
        &JsValue::from_f64(delay),
    )
}

fn global_clear_timeout(timer: &JsValue) -> Result<(), JsValue> {
    required_function(&js_sys::global(), "clearTimeout", "global")?
        .call1(&js_sys::global(), timer)?;
    Ok(())
}

fn translate_value(
    translate: &Function,
    key: &str,
    parameters: Option<&Object>,
) -> Result<JsValue, JsValue> {
    let arguments = Array::new();
    arguments.push(&JsValue::from_str(key));
    if let Some(parameters) = parameters {
        arguments.push(parameters);
    }
    translate.apply(&JsValue::UNDEFINED, &arguments)
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation message actions were not configured").into()
        })
    })
}

fn required_number(value: &JsValue, owner: &str) -> Result<f64, JsValue> {
    value
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} must be a number")).into())
}

fn javascript_number(value: &JsValue) -> Result<f64, JsValue> {
    let constructor =
        Reflect::get(&js_sys::global(), &JsValue::from_str("Number"))?.dyn_into::<Function>()?;
    constructor
        .call1(&JsValue::UNDEFINED, value)?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new("Number() did not return a number").into())
}

fn javascript_string(value: &JsValue) -> Result<String, JsValue> {
    let constructor =
        Reflect::get(&js_sys::global(), &JsValue::from_str("String"))?.dyn_into::<Function>()?;
    constructor
        .call1(&JsValue::UNDEFINED, value)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("String() did not return a string").into())
}

fn required_js_string(value: &JsValue, owner: &str) -> Result<String, JsValue> {
    value
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} must be a string")).into())
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required_js_string(
        &required_property(value, key, owner)?,
        &format!("{owner} {key}"),
    )
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
