//! Compiled simple-message, retry, terminal notice, and branch adapters.

use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Date, Function, JsString, Math, Object, Promise, Reflect, RegExp};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{browser_image_labels::message_image_labels, browser_reasoning::inject_style};

const MESSAGE_CSS: &str =
    include_str!("../../../packages/client/ui-conversation/src/client/chat/MessageItem.module.css");

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
    static COMPONENTS: RefCell<Option<MessageComponents>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    fragment: JsValue,
    json_block: JsValue,
    message_text: JsValue,
    state_dot: JsValue,
    image_gallery: JsValue,
    message_actions: JsValue,
    compaction_item: JsValue,
    context_row: JsValue,
}

#[derive(Clone)]
struct MessageComponents {
    pending_steering: JsValue,
    user_node_view: JsValue,
    context_node_view: JsValue,
    compaction_node_view: JsValue,
    retry_node_view: JsValue,
    turn_error_node_view: JsValue,
    turn_max_tokens_node_view: JsValue,
    unknown_node_view: JsValue,
}

struct ContentParts {
    text: String,
    images: Array,
    rest: Array,
}

type Renderer = fn(&BrowserModules, &JsValue) -> Result<JsValue, JsValue>;

/// Configures the compiled `MessageItem` component family.
///
/// `dependencies` owns the already-compiled `MessageIconActions`, `CompactionItem`,
/// and `ContextInjectionRow` faces.
///
/// # Errors
///
/// Returns on missing React/hooks, primitives, attachment, dependency faces, or CSS failure.
#[wasm_bindgen(js_name = configureClientUiConversationMessageItem)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_conversation_message_item(
    react: JsValue,
    ui_primitives: JsValue,
    ui_attachment: JsValue,
    dependencies: JsValue,
) -> Result<(), JsValue> {
    for method in ["createElement", "memo", "useEffect", "useMemo", "useState"] {
        required_function(&react, method, "React")?;
    }
    let modules = BrowserModules {
        fragment: required_property(&react, "Fragment", "React")?,
        json_block: required_property(&ui_primitives, "JsonBlock", "ui-primitives")?,
        message_text: required_property(&ui_primitives, "MessageText", "ui-primitives")?,
        state_dot: required_property(&ui_primitives, "StateDot", "ui-primitives")?,
        image_gallery: required_property(&ui_attachment, "ImageGallery", "ui-attachment")?,
        message_actions: required_property(
            &dependencies,
            "MessageIconActions",
            "MessageItem dependencies",
        )?,
        compaction_item: required_property(
            &dependencies,
            "CompactionItem",
            "MessageItem dependencies",
        )?,
        context_row: required_property(
            &dependencies,
            "ContextInjectionRow",
            "MessageItem dependencies",
        )?,
        react,
    };
    inject_message_styles()?;
    MODULES.with(|configured| *configured.borrow_mut() = Some(modules.clone()));
    let components = MessageComponents {
        pending_steering: raw_component(render_pending_steering),
        user_node_view: memo_component(&modules.react, render_user_node_view)?,
        context_node_view: memo_component(&modules.react, render_context_node_view)?,
        compaction_node_view: memo_component(&modules.react, render_compaction_node_view)?,
        retry_node_view: memo_component(&modules.react, render_retry_node_view)?,
        turn_error_node_view: memo_component(&modules.react, render_turn_error_node_view)?,
        turn_max_tokens_node_view: memo_component(
            &modules.react,
            render_turn_max_tokens_node_view,
        )?,
        unknown_node_view: memo_component(&modules.react, render_unknown_node_view)?,
    };
    COMPONENTS.with(|configured| *configured.borrow_mut() = Some(components));
    Ok(())
}

fn raw_component(renderer: Renderer) -> JsValue {
    Closure::wrap(
        Box::new(move |props: JsValue| renderer(&configured_modules()?, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value()
}

fn memo_component(react: &JsValue, renderer: Renderer) -> Result<JsValue, JsValue> {
    required_function(react, "memo", "React")?.call1(react, &raw_component(renderer))
}

macro_rules! component_getter {
    ($rust:ident, $js:literal, $field:ident) => {
        #[doc = concat!("Returns the compiled `", $js, "` component.")]
        ///
        /// # Errors
        ///
        /// Returns before configuration.
        #[wasm_bindgen(js_name = $js)]
        pub fn $rust() -> Result<JsValue, JsValue> {
            Ok(configured_components()?.$field)
        }
    };
}

component_getter!(
    pending_steering_bubble_component,
    "pendingSteeringBubbleComponent",
    pending_steering
);
component_getter!(
    user_message_node_view_component,
    "userMessageNodeViewComponent",
    user_node_view
);
component_getter!(
    context_message_node_view_component,
    "contextMessageNodeViewComponent",
    context_node_view
);
component_getter!(
    compaction_node_view_component,
    "compactionNodeViewComponent",
    compaction_node_view
);
component_getter!(
    retry_node_view_component,
    "retryNodeViewComponent",
    retry_node_view
);
component_getter!(
    turn_error_node_view_component,
    "turnErrorNodeViewComponent",
    turn_error_node_view
);
component_getter!(
    turn_max_tokens_node_view_component,
    "turnMaxTokensNodeViewComponent",
    turn_max_tokens_node_view
);
component_getter!(
    unknown_node_view_component,
    "unknownNodeViewComponent",
    unknown_node_view
);

/// Resolves the retry countdown display in whole seconds.
#[wasm_bindgen(js_name = retrySeconds)]
#[must_use]
pub fn retry_seconds_browser(milliseconds: f64) -> f64 {
    Math::max(1.0, Math::ceil(milliseconds / 1_000.0))
}

fn render_pending_steering(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let content =
        required_property(props, "content", "PendingSteeringBubble props")?.dyn_into::<Array>()?;
    let translate = required_function(props, "t", "PendingSteeringBubble props")?;
    let supplied = Reflect::get(props, &JsValue::from_str("loadImage"))?;
    let load_image = if supplied.is_undefined() {
        let fallback_translate = translate.clone();
        Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
            let message = fallback_translate.call1(
                &JsValue::UNDEFINED,
                &JsValue::from_str("image.serviceUnavailable"),
            )?;
            Ok(Promise::reject(&js_sys::Error::new(&javascript_string(&message)?).into()).into())
        }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>)
        .into_js_value()
    } else {
        supplied
    };
    render_user_bubble(modules, &content, load_image, None, true, &translate)
}

fn render_user_node_view(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let node = required_property(props, "node", "UserMessageNodeView props")?;
    let data = required_property(&node, "data", "user chat node")?;
    let content = required_property(&data, "content", "user node")?.dyn_into::<Array>()?;
    let load_image = required_property(props, "loadImage", "UserMessageNodeView props")?;
    let translate = required_function(props, "t", "UserMessageNodeView props")?;
    let time = required_property(&data, "time", "user node")?;
    render_user_bubble(modules, &content, load_image, Some(time), false, &translate)
}

fn render_context_node_view(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let node = required_property(props, "node", "ContextMessageNodeView props")?;
    let data = required_property(&node, "data", "context chat node")?;
    create_element(
        &modules.react,
        &modules.context_row,
        Some(&object(&[
            (
                "content",
                required_property(&data, "content", "context node")?,
            ),
            ("source", Reflect::get(&data, &JsValue::from_str("source"))?),
            (
                "provenance",
                required_property(&data, "provenance", "context node")?,
            ),
            ("form", Reflect::get(&data, &JsValue::from_str("form"))?),
            (
                "t",
                required_property(props, "t", "ContextMessageNodeView props")?,
            ),
        ])?),
        &[],
    )
}

fn render_compaction_node_view(
    modules: &BrowserModules,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let node = required_property(props, "node", "CompactionNodeView props")?;
    create_element(
        &modules.react,
        &modules.compaction_item,
        Some(&object(&[
            (
                "node",
                required_property(&node, "data", "compaction chat node")?,
            ),
            (
                "t",
                required_property(props, "t", "CompactionNodeView props")?,
            ),
        ])?),
        &[],
    )
}

#[allow(clippy::float_cmp, clippy::too_many_lines)] // Source uses exact memoized deadline identity.
fn render_retry_node_view(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let node = required_property(props, "node", "RetryNodeView props")?;
    let data = required_property(&node, "data", "retry chat node")?;
    let retry = required_property(&data, "current", "retry chat data")?;
    let active = required_string(&retry, "retryState", "retry node")? == "scheduled";
    let delay_ms = numeric_property(&retry, "delayMs", "retry node")?;
    let seq = required_property(&retry, "seq", "retry node")?;
    let deadline_delay = delay_ms;
    let deadline_factory =
        Closure::wrap(Box::new(move || Date::now() + deadline_delay) as Box<dyn FnMut() -> f64>);
    let deadline = required_function(&modules.react, "useMemo", "React")?
        .call2(
            &modules.react,
            &deadline_factory.into_js_value(),
            &Array::of2(&JsValue::from_f64(delay_ms), &seq),
        )?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new("retry deadline must be a number"))?;
    let scheduled_seconds = retry_seconds_browser(delay_ms);
    let mode = required_string(&retry, "mode", "retry node")?;
    let maximum = if mode == "normal" {
        required_property(&retry, "maxRetries", "retry node")?
    } else {
        JsValue::from_str("∞")
    };
    let initial_deadline = deadline;
    let initializer = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        Ok(object(&[
            ("deadline", JsValue::from_f64(initial_deadline)),
            (
                "seconds",
                JsValue::from_f64(retry_seconds_browser(initial_deadline - Date::now())),
            ),
        ])?
        .into())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let state = required_function(&modules.react, "useState", "React")?
        .call1(&modules.react, &initializer.into_js_value())?
        .dyn_into::<Array>()?;
    let countdown = state.get(0);
    let set_countdown = state.get(1).dyn_into::<Function>()?;
    let countdown_deadline = numeric_property(&countdown, "deadline", "retry countdown")?;
    let remaining_seconds = if countdown_deadline == deadline {
        numeric_property(&countdown, "seconds", "retry countdown")?
    } else {
        retry_seconds_browser(deadline - Date::now())
    };
    install_retry_effect(&modules.react, active, deadline, &set_countdown)?;
    let translate = required_function(props, "t", "RetryNodeView props")?;
    let retry_state = required_string(&retry, "retryState", "retry node")?;
    let label_key = if active {
        "message.retry.active"
    } else if retry_state == "cancelled" {
        "message.retry.cancelled"
    } else if retry_state == "started" {
        "message.retry.started"
    } else {
        "message.retry.scheduled"
    };
    let label = translate.call1(&JsValue::UNDEFINED, &JsValue::from_str(label_key))?;
    let seconds = if active {
        remaining_seconds
    } else {
        scheduled_seconds
    };
    let status = translate.apply(
        &JsValue::UNDEFINED,
        &Array::of2(
            &JsValue::from_str("message.retry.status"),
            object(&[
                ("label", label),
                ("retry", required_property(&retry, "retry", "retry node")?),
                ("maximum", maximum),
                ("seconds", JsValue::from_f64(seconds)),
            ])?
            .as_ref(),
        ),
    )?;
    let summary = create_element(
        &modules.react,
        &JsValue::from_str("summary"),
        Some(&class_props("seekdeep-conversation-message-retrySummary")?),
        &[create_element(
            &modules.react,
            &JsValue::from_str("span"),
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str("seekdeep-conversation-message-retryText"),
                ),
                ("role", JsValue::from_str("status")),
            ])?),
            &[status],
        )?],
    )?;
    let failure = required_property(&retry, "failure", "retry node")?;
    let details = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-message-retryDetails")?),
        &[
            retry_detail(
                modules,
                &translate,
                "message.retry.delay",
                JsValue::from_str(&format!("{}ms", number_string(Math::round(delay_ms))?)),
            )?,
            retry_detail(
                modules,
                &translate,
                "message.retry.failure",
                required_property(&failure, "message", "retry failure")?,
            )?,
        ],
    )?;
    create_element(
        &modules.react,
        &JsValue::from_str("details"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-message-retryRow"),
            ),
            (
                "data-active",
                if active {
                    JsValue::TRUE
                } else {
                    JsValue::UNDEFINED
                },
            ),
        ])?),
        &[summary, details],
    )
}

fn retry_detail(
    modules: &BrowserModules,
    translate: &Function,
    key: &str,
    value: JsValue,
) -> Result<JsValue, JsValue> {
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        None,
        &[
            create_element(
                &modules.react,
                &JsValue::from_str("span"),
                Some(&class_props(
                    "seekdeep-conversation-message-retryDetailLabel",
                )?),
                &[translate.call1(&JsValue::UNDEFINED, &JsValue::from_str(key))?],
            )?,
            value,
        ],
    )
}

#[allow(clippy::float_cmp)] // retrySeconds always returns an integer-valued JavaScript number.
fn install_retry_effect(
    react: &JsValue,
    active: bool,
    deadline: f64,
    set_countdown: &Function,
) -> Result<(), JsValue> {
    let setter = set_countdown.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if !active {
            return Ok(JsValue::UNDEFINED);
        }
        let timer_slot = Rc::new(RefCell::new(JsValue::UNDEFINED));
        let update = countdown_updater(deadline, &setter)?;
        let next = update.call0(&JsValue::UNDEFINED)?.as_f64().unwrap_or(1.0);
        if next == 1.0 {
            return Ok(JsValue::UNDEFINED);
        }
        let tick_update = update;
        let tick_timer = Rc::clone(&timer_slot);
        let tick = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            if tick_update
                .call0(&JsValue::UNDEFINED)?
                .as_f64()
                .unwrap_or(1.0)
                == 1.0
            {
                clear_interval(&tick_timer.borrow())?;
            }
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value();
        let timer = set_interval(&tick, 250.0)?;
        *timer_slot.borrow_mut() = timer.clone();
        let cleanup = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            clear_interval(&timer)?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value();
        Ok(cleanup)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    required_function(react, "useEffect", "React")?.call2(
        react,
        &effect.into_js_value(),
        &Array::of2(&JsValue::from_bool(active), &JsValue::from_f64(deadline)),
    )?;
    Ok(())
}

#[allow(clippy::float_cmp)] // Functional state reuse matches source `===` comparisons.
fn countdown_updater(deadline: f64, setter: &Function) -> Result<Function, JsValue> {
    let setter = setter.clone();
    Closure::wrap(Box::new(move || -> Result<f64, JsValue> {
        let next = retry_seconds_browser(deadline - Date::now());
        let state_update = Closure::wrap(Box::new(
            move |current: JsValue| -> Result<JsValue, JsValue> {
                let current_deadline = numeric_property(&current, "deadline", "retry countdown")?;
                let current_seconds = numeric_property(&current, "seconds", "retry countdown")?;
                if current_deadline == deadline && current_seconds == next {
                    Ok(current)
                } else {
                    Ok(object(&[
                        ("deadline", JsValue::from_f64(deadline)),
                        ("seconds", JsValue::from_f64(next)),
                    ])?
                    .into())
                }
            },
        )
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
        .into_js_value();
        setter.call1(&JsValue::UNDEFINED, &state_update)?;
        Ok(next)
    }) as Box<dyn FnMut() -> Result<f64, JsValue>>)
    .into_js_value()
    .dyn_into()
}

fn render_turn_error_node_view(
    modules: &BrowserModules,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let node = required_property(props, "node", "TurnErrorNodeView props")?;
    let data = required_property(&node, "data", "turn error chat node")?;
    let translate = required_function(props, "t", "TurnErrorNodeView props")?;
    let code = Reflect::get(&data, &JsValue::from_str("code"))?;
    let mut children = vec![
        create_element(
            &modules.react,
            &modules.state_dot,
            Some(&object(&[
                ("state", JsValue::from_str("error")),
                (
                    "className",
                    JsValue::from_str("seekdeep-conversation-message-turnErrorDot"),
                ),
            ])?),
            &[],
        )?,
        create_element(
            &modules.react,
            &JsValue::from_str("div"),
            Some(&class_props("seekdeep-conversation-message-turnErrorCopy")?),
            &[
                span(
                    modules,
                    "seekdeep-conversation-message-turnErrorTitle",
                    translate
                        .call1(&JsValue::UNDEFINED, &JsValue::from_str("message.turnError"))?,
                )?,
                span(
                    modules,
                    "seekdeep-conversation-message-turnErrorMessage",
                    required_property(&data, "message", "turn error node")?,
                )?,
            ],
        )?,
    ];
    if !code.is_undefined() {
        children.push(create_element(
            &modules.react,
            &JsValue::from_str("code"),
            Some(&class_props("seekdeep-conversation-message-turnErrorCode")?),
            &[code],
        )?);
    }
    status_row(modules, &children)
}

fn render_turn_max_tokens_node_view(
    modules: &BrowserModules,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let translate = required_function(props, "t", "TurnMaxTokensNodeView props")?;
    let dot = create_element(
        &modules.react,
        &modules.state_dot,
        Some(&object(&[
            ("state", JsValue::from_str("warning")),
            (
                "className",
                JsValue::from_str("seekdeep-conversation-message-turnErrorDot"),
            ),
        ])?),
        &[],
    )?;
    let copy = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-message-turnErrorCopy")?),
        &[
            span(
                modules,
                "seekdeep-conversation-message-maxTokensTitle",
                translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("message.maxTokens"))?,
            )?,
            span(
                modules,
                "seekdeep-conversation-message-turnErrorMessage",
                translate.call1(
                    &JsValue::UNDEFINED,
                    &JsValue::from_str("message.maxTokens.hint"),
                )?,
            )?,
        ],
    )?;
    status_row(modules, &[dot, copy])
}

fn status_row(modules: &BrowserModules, children: &[JsValue]) -> Result<JsValue, JsValue> {
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-message-turnErrorRow"),
            ),
            ("role", JsValue::from_str("status")),
        ])?),
        children,
    )
}

fn render_unknown_node_view(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let node = required_property(props, "node", "UnknownNodeView props")?;
    let data = required_property(&node, "data", "unknown chat node")?;
    let translate = required_function(props, "t", "UnknownNodeView props")?;
    let payload = Reflect::get(&data, &JsValue::from_str("data"))?;
    let node_type = required_property(&data, "type", "unknown node")?;
    let label = translate.apply(
        &JsValue::UNDEFINED,
        &Array::of2(
            &JsValue::from_str("message.unknownSurface"),
            object(&[("type", node_type)])?.as_ref(),
        ),
    )?;
    let truncated_translate = translate;
    let truncated = Closure::wrap(Box::new(move |total: JsValue| {
        truncated_translate.apply(
            &JsValue::UNDEFINED,
            &Array::of2(
                &JsValue::from_str("json.truncated"),
                object(&[("total", total)])?.as_ref(),
            ),
        )
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value();
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-message-contextRow")?),
        &[create_element(
            &modules.react,
            &modules.json_block,
            Some(&object(&[
                ("label", label),
                ("payload", payload),
                ("truncatedLabel", truncated),
            ])?),
            &[],
        )?],
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn render_user_bubble(
    modules: &BrowserModules,
    content: &Array,
    image_loader: JsValue,
    time: Option<JsValue>,
    pending: bool,
    translate: &Function,
) -> Result<JsValue, JsValue> {
    let parts = content_parts(content)?;
    let labels = message_image_labels(translate)?;
    let gallery = create_element(
        &modules.react,
        &modules.image_gallery,
        Some(&object(&[
            ("images", parts.images.clone().into()),
            ("load", image_loader),
            ("align", JsValue::from_str("end")),
            ("labels", labels),
        ])?),
        &[],
    )?;
    let mut stack_children = vec![gallery];
    if !parts.text.is_empty() || parts.rest.length() > 0 {
        let mut bubble_children = vec![project_user_text(modules, &parts.text)?];
        for index in 0..parts.rest.length() {
            let truncated_translate = translate.clone();
            let truncated = Closure::wrap(Box::new(move |total: JsValue| {
                truncated_translate.apply(
                    &JsValue::UNDEFINED,
                    &Array::of2(
                        &JsValue::from_str("json.truncated"),
                        object(&[("total", total)])?.as_ref(),
                    ),
                )
            })
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
            .into_js_value();
            bubble_children.push(create_element(
                &modules.react,
                &modules.json_block,
                Some(&object(&[
                    ("key", JsValue::from_f64(f64::from(index))),
                    (
                        "label",
                        translate.call1(
                            &JsValue::UNDEFINED,
                            &JsValue::from_str("message.extraBlock"),
                        )?,
                    ),
                    ("payload", parts.rest.get(index)),
                    ("truncatedLabel", truncated),
                ])?),
                &[],
            )?);
        }
        stack_children.push(create_element(
            &modules.react,
            &JsValue::from_str("div"),
            Some(&class_props("seekdeep-conversation-message-bubble")?),
            &bubble_children,
        )?);
    }
    let stack = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-message-userStack")?),
        &stack_children,
    )?;
    let mut action_props = vec![
        ("text", JsValue::from_str(&parts.text)),
        ("clock", JsValue::from_str("start")),
        ("className", JsValue::UNDEFINED),
        ("t", translate.clone().into()),
    ];
    if let Some(time) = time {
        action_props.push(("time", time));
    }
    let actions = create_element(
        &modules.react,
        &modules.message_actions,
        Some(&object(&action_props)?),
        &[],
    )?;
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-message-userRow"),
            ),
            (
                "data-pending-steering",
                if pending {
                    JsValue::TRUE
                } else {
                    JsValue::UNDEFINED
                },
            ),
            ("data-time-hover-root", JsValue::TRUE),
        ])?),
        &[stack, actions],
    )
}

fn content_parts(content: &Array) -> Result<ContentParts, JsValue> {
    let mut texts = Vec::new();
    let images = Array::new();
    let rest = Array::new();
    for index in 0..content.length() {
        let block = content.get(index);
        let block_type = Reflect::get(&block, &JsValue::from_str("type"))?;
        let text = Reflect::get(&block, &JsValue::from_str("text"))?;
        if block_type.as_string().as_deref() == Some("text") && text.is_string() {
            texts.push(text.as_string().unwrap_or_default());
            continue;
        }
        let attachment = Reflect::get(&block, &JsValue::from_str("attachment"))?;
        if block_type.as_string().as_deref() == Some("image") && !attachment.is_undefined() {
            images.push(object(&[("attachment", attachment)])?.as_ref());
        } else {
            rest.push(&block);
        }
    }
    Ok(ContentParts {
        text: texts.concat(),
        images,
        rest,
    })
}

fn project_user_text(modules: &BrowserModules, text: &str) -> Result<JsValue, JsValue> {
    let expression = RegExp::new(r"(^|\s)([/@][\w-]+)(?=\s|$)", "g");
    let source = JsString::from(text);
    let mut children = Vec::new();
    let mut cursor = 0_u32;
    while let Some(found) = expression.exec(text) {
        let index = javascript_u32(
            &Reflect::get(found.as_ref(), &JsValue::from_str("index"))?,
            "reference match index",
        )?;
        let prefix_length = found.get(1).length();
        let token_start = index + prefix_length;
        let label = found.get(2);
        let label_length = label.length();
        if token_start > cursor {
            children.push(message_text(
                modules,
                source.slice(cursor, token_start).into(),
                Some(cursor),
            )?);
        }
        let label_text = label.as_string().unwrap_or_default();
        children.push(create_element(
            &modules.react,
            &JsValue::from_str("span"),
            Some(&object(&[
                ("key", JsValue::from_f64(f64::from(token_start))),
                (
                    "className",
                    JsValue::from_str("seekdeep-conversation-message-refChip"),
                ),
                (
                    "data-ref-chip",
                    JsValue::from_str(if label_text.starts_with('@') {
                        "subagent"
                    } else {
                        "skill"
                    }),
                ),
            ])?),
            &[label.into()],
        )?);
        cursor = token_start + label_length;
    }
    if children.is_empty() {
        return message_text(modules, JsValue::from_str(text), None);
    }
    if cursor < source.length() {
        children.push(message_text(
            modules,
            source.slice(cursor, source.length()).into(),
            Some(cursor),
        )?);
    }
    create_element(&modules.react, &modules.fragment, None, &children)
}

fn message_text(
    modules: &BrowserModules,
    text: JsValue,
    key: Option<u32>,
) -> Result<JsValue, JsValue> {
    let mut props = vec![("text", text)];
    if let Some(key) = key {
        props.push(("key", JsValue::from_f64(f64::from(key))));
    }
    create_element(
        &modules.react,
        &modules.message_text,
        Some(&object(&props)?),
        &[],
    )
}

fn inject_message_styles() -> Result<(), JsValue> {
    inject_style(
        "MessageItem",
        MESSAGE_CSS,
        &[
            ("bubble", "seekdeep-conversation-message-bubble"),
            (
                "compactionBody",
                "seekdeep-conversation-message-compactionBody",
            ),
            (
                "compactionButton",
                "seekdeep-conversation-message-compactionButton",
            ),
            (
                "compactionContextIcon",
                "seekdeep-conversation-message-compactionContextIcon",
            ),
            (
                "compactionDisclosureIcon",
                "seekdeep-conversation-message-compactionDisclosureIcon",
            ),
            (
                "compactionLeading",
                "seekdeep-conversation-message-compactionLeading",
            ),
            (
                "compactionRow",
                "seekdeep-conversation-message-compactionRow",
            ),
            (
                "compactionSep",
                "seekdeep-conversation-message-compactionSep",
            ),
            (
                "compactionSummary",
                "seekdeep-conversation-message-compactionSummary",
            ),
            (
                "compactionTitle",
                "seekdeep-conversation-message-compactionTitle",
            ),
            ("contextRow", "seekdeep-conversation-message-contextRow"),
            (
                "maxTokensTitle",
                "seekdeep-conversation-message-maxTokensTitle",
            ),
            ("refChip", "seekdeep-conversation-message-refChip"),
            (
                "retryDetailLabel",
                "seekdeep-conversation-message-retryDetailLabel",
            ),
            ("retryDetails", "seekdeep-conversation-message-retryDetails"),
            ("retryRow", "seekdeep-conversation-message-retryRow"),
            ("retrySummary", "seekdeep-conversation-message-retrySummary"),
            ("retryText", "seekdeep-conversation-message-retryText"),
            (
                "turnErrorCode",
                "seekdeep-conversation-message-turnErrorCode",
            ),
            (
                "turnErrorCopy",
                "seekdeep-conversation-message-turnErrorCopy",
            ),
            ("turnErrorDot", "seekdeep-conversation-message-turnErrorDot"),
            (
                "turnErrorMessage",
                "seekdeep-conversation-message-turnErrorMessage",
            ),
            ("turnErrorRow", "seekdeep-conversation-message-turnErrorRow"),
            (
                "turnErrorTitle",
                "seekdeep-conversation-message-turnErrorTitle",
            ),
            ("userRow", "seekdeep-conversation-message-userRow"),
            ("userStack", "seekdeep-conversation-message-userStack"),
        ],
    )
}

fn set_interval(callback: &JsValue, delay: f64) -> Result<JsValue, JsValue> {
    let window = browser_window()?;
    required_function(&window, "setInterval", "window")?
        .apply(&window, &Array::of2(callback, &JsValue::from_f64(delay)))
}

fn clear_interval(timer: &JsValue) -> Result<(), JsValue> {
    if timer.is_undefined() {
        return Ok(());
    }
    let window = browser_window()?;
    required_function(&window, "clearInterval", "window")?.call1(&window, timer)?;
    Ok(())
}

fn browser_window() -> Result<JsValue, JsValue> {
    let global = js_sys::global();
    let window = Reflect::get(&global, &JsValue::from_str("window"))?;
    Ok(if window.is_undefined() {
        global.into()
    } else {
        window
    })
}

fn span(modules: &BrowserModules, class_name: &str, text: JsValue) -> Result<JsValue, JsValue> {
    create_element(
        &modules.react,
        &JsValue::from_str("span"),
        Some(&class_props(class_name)?),
        &[text],
    )
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation MessageItem was not configured").into()
        })
    })
}

fn configured_components() -> Result<MessageComponents, JsValue> {
    COMPONENTS.with(|components| {
        components.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation MessageItem components were not configured")
                .into()
        })
    })
}

fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required_property(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be string")).into())
}

fn numeric_property(value: &JsValue, key: &str, owner: &str) -> Result<f64, JsValue> {
    required_property(value, key, owner)?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be number")).into())
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

fn number_string(value: f64) -> Result<String, JsValue> {
    js_sys::Number::from(value)
        .to_string_with_radix(10)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("Number.toString() returned non-string").into())
}

fn javascript_string(value: &JsValue) -> Result<String, JsValue> {
    required_function(&js_sys::global(), "String", "global")?
        .call1(&JsValue::UNDEFINED, value)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("String() returned non-string").into())
}

fn javascript_u32(value: &JsValue, owner: &str) -> Result<u32, JsValue> {
    let number = value
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} must be number")))?;
    number_string(number)?
        .parse::<u32>()
        .map_err(|_| js_sys::RangeError::new(&format!("{owner} must be a u32")).into())
}
