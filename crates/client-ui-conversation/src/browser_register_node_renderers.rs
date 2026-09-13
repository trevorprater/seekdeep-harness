//! Compiled registration of the keyed conversation chat renderer family.

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

const CHAT_NODE_SLOT: &str = "conversation.chat.node";
const LOCALE_NAMESPACE: &str = "conversation";

struct RendererEntry {
    key: &'static str,
    component: JsValue,
    children: Option<Object>,
}

/// Registers every built-in business renderer behind the keyed Chat Node seat.
///
/// `components` contains the compiled renderer faces by their source export names.
///
/// # Errors
///
/// Returns before mutation when a required component is missing, or when slot
/// injection/registration throws.
#[wasm_bindgen(js_name = registerChatNodeRenderers)]
#[allow(clippy::needless_pass_by_value)]
pub fn register_chat_node_renderers_browser(
    context: JsValue,
    components: JsValue,
) -> Result<(), JsValue> {
    let slots = required_property(&context, "slots", "renderer context")?;
    let user = required_property(&components, "UserMessageNodeView", "renderer components")?;
    let entries = vec![
        RendererEntry {
            key: "user",
            component: user.clone(),
            children: None,
        },
        RendererEntry {
            key: "steering",
            component: user,
            children: None,
        },
        entry(&components, "context", "ContextMessageNodeView")?,
        entry(&components, "assistant-step", "AssistantNodeView")?,
        RendererEntry {
            key: "command",
            component: required_property(&components, "CommandNodeView", "renderer components")?,
            children: Some(object(&[(
                "conversation.chat.commandview",
                child_spec("keyed")?.into(),
            )])?),
        },
        entry(&components, "manual-compaction", "ManualCompactionNodeView")?,
        entry(&components, "compaction", "CompactionNodeView")?,
        entry(&components, "model-retry", "RetryNodeView")?,
        entry(&components, "turn-error", "TurnErrorNodeView")?,
        entry(&components, "turn-max-tokens", "TurnMaxTokensNodeView")?,
        RendererEntry {
            key: "turn-tail",
            component: required_property(&components, "TurnTailNodeView", "renderer components")?,
            children: Some(object(&[
                ("conversation.chat.turnTail", child_spec("chain")?.into()),
                (
                    "conversation.chat.assistant-actions",
                    child_spec("list")?.into(),
                ),
            ])?),
        },
        entry(&components, "unknown", "UnknownNodeView")?,
    ];
    for entry in entries {
        register_entry(&slots, entry)?;
    }
    Ok(())
}

fn entry(components: &JsValue, key: &'static str, name: &str) -> Result<RendererEntry, JsValue> {
    Ok(RendererEntry {
        key,
        component: required_property(components, name, "renderer components")?,
        children: None,
    })
}

fn register_entry(slots: &JsValue, entry: RendererEntry) -> Result<(), JsValue> {
    let inject_slots = slots.clone();
    let component = entry.component;
    let mut option_entries = vec![
        ("name", JsValue::from_str(CHAT_NODE_SLOT)),
        ("key", JsValue::from_str(entry.key)),
        ("locale", JsValue::from_str(LOCALE_NAMESPACE)),
    ];
    if let Some(children) = entry.children {
        option_entries.push(("children", children.into()));
    }
    let options = object(&option_entries)?;
    let callback = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        required_function(&inject_slots, "register", "slots")?
            .apply(&inject_slots, &Array::of2(options.as_ref(), &component))
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>)
    .into_js_value();
    required_function(slots, "inject", "slots")?.apply(
        slots,
        &Array::of2(&JsValue::from_str(CHAT_NODE_SLOT), &callback),
    )?;
    Ok(())
}

fn child_spec(kind: &str) -> Result<Object, JsValue> {
    object(&[
        ("kind", JsValue::from_str(kind)),
        ("scope", JsValue::from_str("session")),
    ])
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
