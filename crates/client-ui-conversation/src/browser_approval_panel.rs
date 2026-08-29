//! Compiled composer-takeover approval prompt.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{browser_reasoning::inject_style, root_tool_call_browser};

const APPROVAL_CSS: &str = include_str!(
    "../../../packages/client/ui-conversation/src/client/skeleton/ApprovalPanel.module.css"
);

thread_local! {
    static COMPONENT: RefCell<Option<JsValue>> = const { RefCell::new(None) };
    static FLOW_COMPONENT: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    button: JsValue,
    pending_approval: Function,
}

/// Configures the compiled approval takeover over React, Button, and its domain constructor.
///
/// # Errors
///
/// Returns on missing dependency faces or stylesheet failure.
#[wasm_bindgen(js_name = configureClientUiConversationApprovalPanel)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_conversation_approval_panel(
    react: JsValue,
    ui_primitives: JsValue,
    pending_approval: Function,
) -> Result<(), JsValue> {
    for method in ["createElement", "useMemo", "useState"] {
        required_function(&react, method, "React")?;
    }
    let modules = BrowserModules {
        button: required_property(&ui_primitives, "Button", "ui-primitives")?,
        react,
        pending_approval,
    };
    inject_style(
        "ApprovalPanel",
        APPROVAL_CSS,
        &[
            ("actionRow", "seekdeep-conversation-approval-actionRow"),
            ("body", "seekdeep-conversation-approval-body"),
            ("card", "seekdeep-conversation-approval-card"),
            ("command", "seekdeep-conversation-approval-command"),
            ("dot", "seekdeep-conversation-approval-dot"),
            ("headline", "seekdeep-conversation-approval-headline"),
            ("reject", "seekdeep-conversation-approval-reject"),
            ("root", "seekdeep-conversation-approval-root"),
            ("strip", "seekdeep-conversation-approval-strip"),
        ],
    )?;
    let flow_modules = modules.clone();
    let flow =
        Closure::wrap(
            Box::new(move |props: JsValue| render_approval_flow(&flow_modules, &props))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        )
        .into_js_value();
    FLOW_COMPONENT.with(|configured| *configured.borrow_mut() = Some(flow.clone()));
    let outer_modules = modules;
    let outer_flow = flow;
    let component = Closure::wrap(Box::new(move |props: JsValue| {
        render_approval_panel(&outer_modules, &outer_flow, &props)
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value();
    COMPONENT.with(|configured| *configured.borrow_mut() = Some(component));
    Ok(())
}

/// Returns the compiled `ApprovalPanel` component.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = approvalPanelComponent)]
pub fn approval_panel_component() -> Result<JsValue, JsValue> {
    configured_component(&COMPONENT, "ApprovalPanel")
}

/// Returns the internal keyed flow component for same-crate live tests.
#[doc(hidden)]
pub fn approval_flow_component() -> Result<JsValue, JsValue> {
    configured_component(&FLOW_COMPONENT, "ApprovalFlow")
}

/// Extracts a shell command from one running Tool call's JSON arguments.
///
/// # Errors
///
/// Returns when a present call omits or mis-types `argsRaw`.
#[wasm_bindgen(js_name = commandOf)]
#[allow(clippy::needless_pass_by_value)]
pub fn command_of_browser(call: JsValue) -> Result<JsValue, JsValue> {
    if call.is_undefined() {
        return Ok(JsValue::UNDEFINED);
    }
    let raw = required_property(&call, "argsRaw", "running Tool call")?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("running Tool argsRaw must be a string"))?;
    let Ok(args) = js_sys::JSON::parse(&raw) else {
        return Ok(JsValue::UNDEFINED);
    };
    let command = Reflect::get(&args, &JsValue::from_str("command"))?;
    Ok(if command.is_string() {
        command
    } else {
        JsValue::UNDEFINED
    })
}

fn configured_component(
    cell: &'static std::thread::LocalKey<RefCell<Option<JsValue>>>,
    name: &str,
) -> Result<JsValue, JsValue> {
    cell.with(|component| {
        component.borrow().clone().ok_or_else(|| {
            js_sys::Error::new(&format!("client-ui-conversation {name} was not configured")).into()
        })
    })
}

fn render_approval_panel(
    modules: &BrowserModules,
    flow: &JsValue,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let matched = required_property(props, "matched", "ApprovalPanel props")?;
    let constructor = modules.pending_approval.clone();
    let constructor_match = matched.clone();
    let factory = Closure::wrap(Box::new(move || {
        Reflect::construct(&constructor, &Array::of1(&constructor_match))
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let approval = required_function(&modules.react, "useMemo", "React")?.call2(
        &modules.react,
        &factory.into_js_value(),
        &Array::of1(&matched),
    )?;
    let selector_approval = approval.clone();
    let selector = Closure::wrap(
        Box::new(move |snapshot: JsValue| -> Result<JsValue, JsValue> {
            let call_id = Reflect::get(&selector_approval, &JsValue::from_str("callId"))?;
            if call_id.is_undefined() {
                return Ok(JsValue::UNDEFINED);
            }
            let call_id_text = call_id
                .as_string()
                .ok_or_else(|| js_sys::TypeError::new("PendingApproval callId must be a string"))?;
            let root = root_tool_call_browser(snapshot, call_id_text.clone())?;
            if root.is_undefined() {
                return Ok(JsValue::UNDEFINED);
            }
            let same_call = Reflect::get(&root, &JsValue::from_str("callId"))?
                .as_string()
                .as_deref()
                == Some(call_id_text.as_str());
            if same_call && !Reflect::has(&root, &JsValue::from_str("kind"))? {
                command_of_browser(root)
            } else {
                Ok(JsValue::UNDEFINED)
            }
        }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    );
    let command = required_function(props, "useSession", "ApprovalPanel props")?
        .call1(&JsValue::UNDEFINED, &selector.into_js_value())?;
    let mut flow_props = vec![
        (
            "key",
            required_property(&approval, "key", "PendingApproval")?,
        ),
        ("pending", approval),
        ("t", required_property(props, "t", "ApprovalPanel props")?),
    ];
    if !command.is_undefined() {
        flow_props.push(("command", command));
    }
    create_element(&modules.react, flow, Some(&object(&flow_props)?), &[])
}

#[allow(clippy::too_many_lines)] // Closed takeover tree and one-shot answer lifecycle stay together.
fn render_approval_flow(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let pending = required_property(props, "pending", "ApprovalFlow props")?;
    let translate = required_function(props, "t", "ApprovalFlow props")?;
    let state = required_function(&modules.react, "useState", "React")?
        .call1(&modules.react, &JsValue::FALSE)?
        .dyn_into::<Array>()?;
    let answered = state
        .get(0)
        .as_bool()
        .ok_or_else(|| js_sys::TypeError::new("ApprovalFlow answered state must be a boolean"))?;
    let set_answered = state.get(1).dyn_into::<Function>()?;
    let rejected = answer_callback(&pending, &set_answered, "rejected");
    let allowed = answer_callback(&pending, &set_answered, "allowed-once");
    let pending_key = required_property(&pending, "key", "PendingApproval")?;
    let waiting = translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("approval.waiting"))?;
    let detail_aria = translate.call1(
        &JsValue::UNDEFINED,
        &JsValue::from_str("approval.detail.aria"),
    )?;
    let reason = Reflect::get(&pending, &JsValue::from_str("reason"))?;
    let headline = if reason.is_null() || reason.is_undefined() {
        translate.apply(
            &JsValue::UNDEFINED,
            &Array::of2(
                &JsValue::from_str("approval.escalation"),
                object(&[(
                    "toolName",
                    required_property(&pending, "toolName", "PendingApproval")?,
                )])?
                .as_ref(),
            ),
        )?
    } else {
        reason
    };
    let command = Reflect::get(props, &JsValue::from_str("command"))?;
    let reject_label =
        translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("approval.reject"))?;
    let allow_label = translate.call1(
        &JsValue::UNDEFINED,
        &JsValue::from_str("approval.allowOnce"),
    )?;
    let strip = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-approval-strip")?),
        &[
            create_element(
                &modules.react,
                &JsValue::from_str("span"),
                Some(&class_props("seekdeep-conversation-approval-dot")?),
                &[],
            )?,
            waiting,
        ],
    )?;
    let body = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-approval-body"),
            ),
            ("data-approval-scroll", JsValue::from_str("")),
            ("tabIndex", JsValue::from_f64(0.0)),
            ("role", JsValue::from_str("group")),
            ("aria-label", detail_aria),
        ])?),
        &[
            create_element(
                &modules.react,
                &JsValue::from_str("div"),
                Some(&class_props("seekdeep-conversation-approval-headline")?),
                &[headline],
            )?,
            if command.is_undefined() {
                JsValue::FALSE
            } else {
                create_element(
                    &modules.react,
                    &JsValue::from_str("div"),
                    Some(&class_props("seekdeep-conversation-approval-command")?),
                    &[command],
                )?
            },
        ],
    )?;
    let actions = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-approval-actionRow")?),
        &[
            create_element(
                &modules.react,
                &modules.button,
                Some(&object(&[
                    ("variant", JsValue::from_str("outline")),
                    (
                        "className",
                        JsValue::from_str("seekdeep-conversation-approval-reject"),
                    ),
                    ("disabled", JsValue::from_bool(answered)),
                    ("onClick", rejected),
                ])?),
                &[reject_label],
            )?,
            create_element(
                &modules.react,
                &modules.button,
                Some(&object(&[
                    ("variant", JsValue::from_str("primary")),
                    ("disabled", JsValue::from_bool(answered)),
                    ("onClick", allowed),
                ])?),
                &[allow_label],
            )?,
        ],
    )?;
    let card = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-approval-card")?),
        &[strip, body, actions],
    )?;
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-approval-root"),
            ),
            ("data-approval-key", pending_key),
        ])?),
        &[card],
    )
}

fn answer_callback(pending: &JsValue, set_answered: &Function, outcome: &'static str) -> JsValue {
    let answer_pending = pending.clone();
    let answer_setter = set_answered.clone();
    Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        answer_setter.call1(&JsValue::UNDEFINED, &JsValue::TRUE)?;
        let answer = required_function(&answer_pending, "answer", "PendingApproval")?;
        let promise = answer.call1(&answer_pending, &JsValue::from_str(outcome))?;
        let rejected_setter = answer_setter.clone();
        let on_rejected = Closure::wrap(Box::new(move |_error: JsValue| -> Result<(), JsValue> {
            rejected_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
            Ok(())
        })
            as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value();
        required_function(&promise, "catch", "PendingApproval answer Promise")?
            .call1(&promise, &on_rejected)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value()
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
