//! Compiled per-node subscription and keyed slot dispatch seat.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::browser_reasoning::inject_style;

const CHAT_CSS: &str =
    include_str!("../../../packages/client/ui-conversation/src/client/chat/ChatView.module.css");

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    json_block: JsValue,
}

/// Configures the compiled keyed Chat Node seat.
///
/// # Errors
///
/// Returns on missing React or ui-primitives faces and stylesheet failures.
#[wasm_bindgen(js_name = configureClientUiConversationChatSeat)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_conversation_chat_seat(
    react: JsValue,
    ui_primitives: JsValue,
) -> Result<(), JsValue> {
    for method in ["createElement", "memo", "useMemo"] {
        required_function(&react, method, "React")?;
    }
    let json_block = required_property(&ui_primitives, "JsonBlock", "ui-primitives")?;
    inject_style(
        "ChatView",
        CHAT_CSS,
        &[
            ("callRow", "seekdeep-conversation-chat-callRow"),
            ("column", "seekdeep-conversation-chat-column"),
            ("flowItem", "seekdeep-conversation-chat-flowItem"),
            ("hint", "seekdeep-conversation-chat-hint"),
            ("older", "seekdeep-conversation-chat-older"),
            ("openError", "seekdeep-conversation-chat-openError"),
            ("root", "seekdeep-conversation-chat-root"),
            ("scroll", "seekdeep-conversation-chat-scroll"),
            ("toBottom", "seekdeep-conversation-chat-toBottom"),
            ("toBottomSlot", "seekdeep-conversation-chat-toBottomSlot"),
            ("turnStatus", "seekdeep-conversation-chat-turnStatus"),
            (
                "turnStatusClock",
                "seekdeep-conversation-chat-turnStatusClock",
            ),
        ],
    )?;
    MODULES.with(|modules| *modules.borrow_mut() = Some(BrowserModules { react, json_block }));
    Ok(())
}

/// Returns the memoized compiled `ChatNodeSeat` component.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = chatNodeSeatComponent)]
pub fn chat_node_seat_component() -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    let react = modules.react.clone();
    let component = Closure::wrap(Box::new(move |props: JsValue| render(&modules, &props))
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value();
    required_function(&react, "memo", "React")?.call1(&react, &component)
}

#[allow(clippy::too_many_lines)]
fn render(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let node_key = required_string(props, "nodeKey", "ChatNodeSeat props")?;
    let use_session = required_function(props, "useSession", "ChatNodeSeat props")?;
    let selector_key = node_key.clone();
    let selector = Closure::wrap(
        Box::new(move |snapshot: JsValue| -> Result<JsValue, JsValue> {
            let chat = required_property(&snapshot, "chat", "session snapshot")?;
            let nodes = required_property(&chat, "nodes", "chat snapshot")?;
            call_method(&nodes, "get", &[JsValue::from_str(&selector_key)])
        }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    );
    let node = use_session.call1(&JsValue::UNDEFINED, &selector.into_js_value())?;
    let fields = [
        "selectedCallId",
        "cwd",
        "openFile",
        "inspectCall",
        "forkAt",
        "loadImage",
        "fileMentions",
    ];
    let mut owner_values = Vec::new();
    for field in fields {
        owner_values.push((field, Reflect::get(props, &JsValue::from_str(field))?));
    }
    let owner_node = node.clone();
    let owner_fields = owner_values.clone();
    let owner_factory = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if owner_node.is_null() || owner_node.is_undefined() {
            return Ok(JsValue::NULL);
        }
        Ok(object(&owner_fields)?.into())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let dependencies = Array::new();
    dependencies.push(&node);
    for (_, value) in &owner_values {
        dependencies.push(value);
    }
    let owner = required_function(&modules.react, "useMemo", "React")?.call2(
        &modules.react,
        &owner_factory.into_js_value(),
        &dependencies,
    )?;
    if node.is_null() || node.is_undefined() || owner.is_null() {
        return Ok(JsValue::NULL);
    }
    let owner_object = owner.dyn_into::<Object>()?;
    let routed_owner = Object::assign(&Object::new(), &owner_object);
    Reflect::set(&routed_owner, &JsValue::from_str("node"), &node)?;
    let kind = required_string(&node, "kind", "chat node")?;
    let key = required_string(&node, "key", "chat node")?;
    let translate = required_function(props, "t", "ChatNodeSeat props")?;
    let label = translate.apply(
        &JsValue::UNDEFINED,
        &Array::of2(
            &JsValue::from_str("message.unknownSurface"),
            &object(&[("type", JsValue::from_str(&kind))])?.into(),
        ),
    )?;
    let footer_translate = translate.clone();
    let footer = Closure::wrap(Box::new(move |total: JsValue| {
        footer_translate.apply(
            &JsValue::UNDEFINED,
            &Array::of2(
                &JsValue::from_str("json.truncated"),
                &object(&[("total", total)])?.into(),
            ),
        )
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    let fallback = create_element(
        &modules.react,
        &modules.json_block,
        Some(&object(&[
            ("label", label),
            ("payload", required_property(&node, "data", "chat node")?),
            ("truncatedLabel", footer.into_js_value()),
        ])?),
        &[],
    )?;
    let options = object(&[
        ("entryKey", JsValue::from_str(&kind)),
        ("hookContext", JsValue::from_str(&node_key)),
        ("fallback", fallback),
    ])?;
    let render_slot = required_function(props, "renderSlot", "ChatNodeSeat props")?;
    let content = render_slot.apply(
        &JsValue::UNDEFINED,
        &Array::of3(
            &JsValue::from_str("conversation.chat.node"),
            routed_owner.as_ref(),
            options.as_ref(),
        ),
    )?;
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-chat-flowItem"),
            ),
            ("data-chat-anchor-key", JsValue::from_str(&key)),
            ("data-chat-flow-key", JsValue::from_str(&key)),
            ("data-chat-flow-kind", JsValue::from_str(&kind)),
        ])?),
        &[content],
    )
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation ChatNodeSeat was not configured").into()
        })
    })
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
