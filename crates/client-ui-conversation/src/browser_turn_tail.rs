//! Compiled finalized-turn tail and action composition.

use std::cell::RefCell;

use js_sys::{Array, Function, Math, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{browser_reasoning::inject_style, message_icon_actions_component};

const TURN_TAIL_CSS: &str = include_str!(
    "../../../packages/client/ui-conversation/src/client/chat/TurnTailNodeView.module.css"
);

thread_local! {
    static COMPONENT: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    message_actions: JsValue,
}

/// Configures the compiled finalized-turn tail renderer.
///
/// # Errors
///
/// Returns on missing React faces, unconfigured message actions, or stylesheet failure.
#[wasm_bindgen(js_name = configureClientUiConversationTurnTail)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_conversation_turn_tail(react: JsValue) -> Result<(), JsValue> {
    for method in ["createElement", "memo"] {
        required_function(&react, method, "React")?;
    }
    let modules = BrowserModules {
        react,
        message_actions: message_icon_actions_component()?,
    };
    inject_style(
        "TurnTailNodeView",
        TURN_TAIL_CSS,
        &[
            ("actions", "seekdeep-conversation-turnTail-actions"),
            ("root", "seekdeep-conversation-turnTail-root"),
        ],
    )?;
    let render_modules = modules.clone();
    let raw =
        Closure::wrap(
            Box::new(move |props: JsValue| render_turn_tail(&render_modules, &props))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        )
        .into_js_value();
    let component =
        required_function(&modules.react, "memo", "React")?.call1(&modules.react, &raw)?;
    COMPONENT.with(|configured| *configured.borrow_mut() = Some(component));
    Ok(())
}

/// Returns the memoized compiled `TurnTailNodeView` component.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = turnTailNodeViewComponent)]
pub fn turn_tail_node_view_component() -> Result<JsValue, JsValue> {
    COMPONENT.with(|component| {
        component.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation TurnTailNodeView was not configured").into()
        })
    })
}

#[allow(clippy::too_many_lines)] // Closed source consumer tree and owner derivation stay together.
fn render_turn_tail(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let node = required_property(props, "node", "TurnTailNodeView props")?;
    let data = required_property(&node, "data", "turn-tail chat node")?;
    let data_turn = Reflect::get(&data, &JsValue::from_str("turn"))?;
    let node_key = required_string(&node, "key", "turn-tail chat node")?;
    let selector_turn = data_turn.clone();
    let selector_key = node_key.clone();
    let selector = Closure::wrap(Box::new(move |snapshot: JsValue| -> Result<bool, JsValue> {
        let chat = required_property(&snapshot, "chat", "session snapshot")?;
        let locations = required_property(&chat, "locations", "chat snapshot")?;
        let turn_keys = call_method(&locations, "getTurn", std::slice::from_ref(&selector_turn))?;
        let last = call_method(&turn_keys, "at", &[JsValue::from_f64(-1.0)])?;
        Ok(last.as_string().as_deref() != Some(selector_key.as_str()))
    }) as Box<dyn FnMut(JsValue) -> Result<bool, JsValue>>);
    let has_later_chat_node = required_function(props, "useSession", "TurnTailNodeView props")?
        .call1(&JsValue::UNDEFINED, &selector.into_js_value())?
        .as_bool()
        .ok_or_else(|| js_sys::TypeError::new("useSession selector did not return a boolean"))?;
    let location = required_property(&node, "location", "turn-tail chat node")?;
    let location_kind = required_string(&location, "kind", "turn-tail location")?;
    if !matches!(location_kind.as_str(), "turn" | "step") {
        return Ok(JsValue::NULL);
    }
    let turn = required_property(&location, "turn", "turn-tail location")?;
    let closing = Reflect::get(&data, &JsValue::from_str("closing"))?;
    let seq = if closing.is_null() || closing.is_undefined() {
        required_property(&data, "seq", "turn-tail data")?
    } else {
        let final_node = required_property(&closing, "finalNode", "turn-tail closing")?;
        let final_seq = Reflect::get(&final_node, &JsValue::from_str("seq"))?;
        if final_seq.is_null() || final_seq.is_undefined() {
            required_property(&data, "seq", "turn-tail data")?
        } else {
            final_seq
        }
    };
    let owner = object(&[
        ("turn", turn.clone()),
        ("seq", seq),
        (
            "openFile",
            Reflect::get(props, &JsValue::from_str("openFile"))?,
        ),
    ])?;
    let tail = required_function(props, "renderSlotChain", "TurnTailNodeView props")?.apply(
        &JsValue::UNDEFINED,
        &Array::of2(
            &JsValue::from_str("conversation.chat.turnTail"),
            owner.as_ref(),
        ),
    )?;
    if closing.is_null() {
        return if tail.is_null() {
            Ok(JsValue::NULL)
        } else {
            root(modules, &[tail], None)
        };
    }
    let final_node = required_property(&closing, "finalNode", "turn-tail closing")?;
    let final_seq = Reflect::get(&final_node, &JsValue::from_str("seq"))?;
    let start = Reflect::get(&turn, &JsValue::from_str("start"))?;
    let end = Reflect::get(&turn, &JsValue::from_str("end"))?;
    let run_ms = if start.is_undefined() || end.is_undefined() {
        JsValue::UNDEFINED
    } else {
        let start_time = javascript_number(&Reflect::get(&start, &JsValue::from_str("time"))?)?;
        let end_time = javascript_number(&Reflect::get(&end, &JsValue::from_str("time"))?)?;
        JsValue::from_f64(Math::max(0.0, end_time - start_time))
    };
    let message_id = Reflect::get(&final_node, &JsValue::from_str("messageId"))?;
    let assistant_actions = if message_id.is_undefined() {
        JsValue::NULL
    } else {
        required_function(props, "renderSlot", "TurnTailNodeView props")?.apply(
            &JsValue::UNDEFINED,
            &Array::of2(
                &JsValue::from_str("conversation.chat.assistant-actions"),
                object(&[("messageId", message_id)])?.as_ref(),
            ),
        )?
    };
    let visible_text =
        assistant_text(&required_property(&closing, "blocks", "turn-tail closing")?)?;
    let closing_time = Reflect::get(&closing, &JsValue::from_str("time"))?;
    let ttft_ms = Reflect::get(&data, &JsValue::from_str("ttftMs"))?;
    let tokens_per_second = Reflect::get(&data, &JsValue::from_str("tokensPerSecond"))?;
    let fork_at = required_function(props, "forkAt", "TurnTailNodeView props")?;
    let branch_seq = final_seq.clone();
    let on_branch = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        fork_at.call1(&JsValue::UNDEFINED, &branch_seq)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    let branch_unavailable = Reflect::get(&data, &JsValue::from_str("branchUnavailable"))?
        .as_bool()
        .unwrap_or(false)
        || has_later_chat_node;
    let action_props = object(&[
        ("text", JsValue::from_str(&visible_text)),
        ("time", closing_time),
        ("runMs", run_ms),
        ("ttftMs", ttft_ms),
        ("tokensPerSecond", tokens_per_second),
        ("clock", JsValue::from_str("end")),
        ("onBranch", on_branch),
        ("branchUnavailable", JsValue::from_bool(branch_unavailable)),
        (
            "className",
            JsValue::from_str("seekdeep-conversation-turnTail-actions"),
        ),
        ("extraActions", assistant_actions),
        (
            "t",
            required_property(props, "t", "TurnTailNodeView props")?,
        ),
    ])?;
    let actions = create_element(
        &modules.react,
        &modules.message_actions,
        Some(&action_props),
        &[],
    )?;
    root(
        modules,
        &[tail, actions],
        Some(&[
            ("data-turn-tail", data_turn),
            ("data-time-hover-root", JsValue::TRUE),
        ]),
    )
}

fn assistant_text(blocks: &JsValue) -> Result<String, JsValue> {
    let blocks = blocks.clone().dyn_into::<Array>()?;
    let mut text = String::new();
    for index in 0..blocks.length() {
        if !Reflect::has(blocks.as_ref(), &JsValue::from_f64(f64::from(index)))? {
            continue;
        }
        let block = blocks.get(index);
        if required_string(&block, "kind", "assistant block")? == "text" {
            text.push_str(&required_string(&block, "text", "assistant text block")?);
        }
    }
    Ok(text)
}

fn root(
    modules: &BrowserModules,
    children: &[JsValue],
    attributes: Option<&[(&str, JsValue)]>,
) -> Result<JsValue, JsValue> {
    let mut entries = vec![(
        "className",
        JsValue::from_str("seekdeep-conversation-turnTail-root"),
    )];
    if let Some(attributes) = attributes {
        entries.extend(attributes.iter().cloned());
    }
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&entries)?),
        children,
    )
}

fn javascript_number(value: &JsValue) -> Result<f64, JsValue> {
    let constructor =
        Reflect::get(&js_sys::global(), &JsValue::from_str("Number"))?.dyn_into::<Function>()?;
    constructor
        .call1(&JsValue::UNDEFINED, value)?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new("Number() did not return a number").into())
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
