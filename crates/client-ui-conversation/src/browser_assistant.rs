//! Compiled assistant block composition and assistant-node view bridge.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{browser_reasoning::inject_style, reasoning_row_component};

const ASSISTANT_CSS: &str = include_str!(
    "../../../packages/client/ui-conversation/src/client/chat/AssistantMarkdown.module.css"
);

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    markdown_text: JsValue,
    json_block: JsValue,
    image_gallery: JsValue,
    reasoning_row: JsValue,
}

/// Configures compiled assistant composition over the shell's existing atom modules.
///
/// # Errors
///
/// Returns on missing React/module faces or stylesheet injection failures.
#[wasm_bindgen(js_name = configureClientUiConversationAssistant)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_conversation_assistant(
    react: JsValue,
    ui_primitives: JsValue,
    ui_attachment: JsValue,
) -> Result<(), JsValue> {
    for method in ["createElement", "memo", "useMemo"] {
        required_function(&react, method, "React")?;
    }
    let modules = BrowserModules {
        react,
        markdown_text: required_property(&ui_primitives, "MarkdownText", "ui-primitives")?,
        json_block: required_property(&ui_primitives, "JsonBlock", "ui-primitives")?,
        image_gallery: required_property(&ui_attachment, "ImageGallery", "ui-attachment")?,
        reasoning_row: reasoning_row_component()?,
    };
    inject_style(
        "AssistantMarkdown",
        ASSISTANT_CSS,
        &[
            ("stopped", "seekdeep-conversation-assistant-stopped"),
            ("body", "seekdeep-conversation-assistant-body"),
            ("root", "seekdeep-conversation-assistant-root"),
        ],
    )?;
    MODULES.with(|configured| *configured.borrow_mut() = Some(modules));
    Ok(())
}

/// Returns the memoized compiled `AssistantMarkdown` component.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = assistantMarkdownComponent)]
pub fn assistant_markdown_component() -> Result<JsValue, JsValue> {
    memoized_component(render_assistant_markdown)
}

/// Returns the memoized compiled `AssistantNodeView` component.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = assistantNodeViewComponent)]
pub fn assistant_node_view_component() -> Result<JsValue, JsValue> {
    memoized_component(render_assistant_node_view)
}

fn memoized_component(
    render: fn(&BrowserModules, &JsValue) -> Result<JsValue, JsValue>,
) -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    let react = modules.react.clone();
    let component = Closure::wrap(Box::new(move |props: JsValue| render(&modules, &props))
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value();
    required_function(&react, "memo", "React")?.call1(&react, &component)
}

#[allow(clippy::too_many_lines)]
fn render_assistant_markdown(
    modules: &BrowserModules,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let blocks = required_property(props, "blocks", "AssistantMarkdown props")?;
    if !Array::is_array(&blocks) {
        return Err(js_sys::TypeError::new("AssistantMarkdown blocks must be an array").into());
    }
    let blocks = Array::from(&blocks);
    let streaming = required_bool(props, "streaming", "AssistantMarkdown props")?;
    let interrupted = optional_bool(props, "interrupted")?.unwrap_or(false);
    let translate = required_function(props, "t", "AssistantMarkdown props")?;
    let load_image = Reflect::get(props, &JsValue::from_str("loadImage"))?;
    let mentions = Reflect::get(props, &JsValue::from_str("mentions"))?;
    let image_loader = if load_image.is_function() {
        load_image
    } else {
        let unavailable = translate.call1(
            &JsValue::UNDEFINED,
            &JsValue::from_str("image.serviceUnavailable"),
        )?;
        Closure::wrap(Box::new(move |_attachment: JsValue| {
            Promise::reject(&js_sys::Error::new(
                unavailable.as_string().as_deref().unwrap_or_default(),
            ))
        }) as Box<dyn FnMut(JsValue) -> Promise>)
        .into_js_value()
    };
    let label_translate = translate.clone();
    let code_label_factory = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        Ok(object(&[
            (
                "copyLabel",
                label_translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("copy"))?,
            ),
            (
                "copiedLabel",
                label_translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("copied"))?,
            ),
        ])?
        .into())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let code_labels = required_function(&modules.react, "useMemo", "React")?.call2(
        &modules.react,
        &code_label_factory.into_js_value(),
        &Array::of1(translate.as_ref()),
    )?;
    let visible = streaming
        || interrupted
        || blocks.iter().any(|block| {
            Reflect::get(&block, &JsValue::from_str("kind"))
                .ok()
                .and_then(|kind| kind.as_string())
                .as_deref()
                != Some("tool-call")
        });
    if !visible {
        return Ok(JsValue::NULL);
    }
    let mut rendered = Vec::new();
    let last = blocks.length().saturating_sub(1);
    let mut index = 0_u32;
    while index < blocks.length() {
        let block = blocks.get(index);
        let kind = required_string(&block, "kind", "assistant block")?;
        match kind.as_str() {
            "text" => rendered.push(create_element(
                &modules.react,
                &modules.markdown_text,
                Some(&object(&[
                    ("key", JsValue::from_f64(f64::from(index))),
                    (
                        "text",
                        JsValue::from_str(&required_string(&block, "text", "text block")?),
                    ),
                    ("streaming", JsValue::from_bool(streaming)),
                    ("codeLabels", code_labels.clone()),
                    ("fileMentions", mentions.clone()),
                ])?),
                &[],
            )?),
            "reasoning" => rendered.push(create_element(
                &modules.react,
                &modules.reasoning_row,
                Some(&object(&[
                    ("key", JsValue::from_f64(f64::from(index))),
                    (
                        "text",
                        JsValue::from_str(&required_string(&block, "text", "reasoning block")?),
                    ),
                    ("running", JsValue::from_bool(streaming && index == last)),
                    ("t", translate.clone().into()),
                ])?),
                &[],
            )?),
            "image" => {
                let start = index;
                let group = Array::new();
                group.push(&block);
                while index + 1 < blocks.length() {
                    let next = blocks.get(index + 1);
                    if required_string(&next, "kind", "assistant block")? != "image" {
                        break;
                    }
                    group.push(&next);
                    index += 1;
                }
                rendered.push(create_element(
                    &modules.react,
                    &modules.image_gallery,
                    Some(&object(&[
                        ("key", JsValue::from_f64(f64::from(start))),
                        ("images", group.into()),
                        ("load", image_loader.clone()),
                        ("align", JsValue::from_str("start")),
                        ("labels", message_image_labels(&translate)?),
                    ])?),
                    &[],
                )?);
            }
            "tool-call" => {}
            _ => {
                let label = translate.call1(
                    &JsValue::UNDEFINED,
                    &JsValue::from_str("message.unknownBlock"),
                )?;
                let footer_translate = translate.clone();
                let footer =
                    Closure::wrap(Box::new(move |total: JsValue| -> Result<JsValue, JsValue> {
                        footer_translate.apply(
                            &JsValue::UNDEFINED,
                            &Array::of2(
                                &JsValue::from_str("json.truncated"),
                                &object(&[("total", total)])?.into(),
                            ),
                        )
                    })
                        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
                rendered.push(create_element(
                    &modules.react,
                    &modules.json_block,
                    Some(&object(&[
                        ("key", JsValue::from_f64(f64::from(index))),
                        ("label", label),
                        (
                            "payload",
                            Reflect::get(&block, &JsValue::from_str("block"))?,
                        ),
                        ("truncatedLabel", footer.into_js_value()),
                    ])?),
                    &[],
                )?);
            }
        }
        index += 1;
    }
    if interrupted {
        rendered.push(create_element(
            &modules.react,
            &JsValue::from_str("span"),
            Some(&class_props("seekdeep-conversation-assistant-stopped")?),
            &[translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("message.stopped"))?],
        )?);
    }
    let body = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-assistant-body")?),
        &rendered,
    )?;
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-assistant-root"),
            ),
            (
                "data-streaming",
                if streaming {
                    JsValue::TRUE
                } else {
                    JsValue::UNDEFINED
                },
            ),
        ])?),
        &[body],
    )
}

fn render_assistant_node_view(
    modules: &BrowserModules,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let node = required_property(props, "node", "AssistantNodeView props")?;
    let data = required_property(&node, "data", "assistant node")?;
    let location = required_property(&node, "location", "assistant node")?;
    let kind = required_string(&location, "kind", "assistant location")?;
    let turn = if matches!(kind.as_str(), "turn" | "step") {
        Reflect::get(&location, &JsValue::from_str("turn"))?
    } else {
        JsValue::UNDEFINED
    };
    let use_turn_data = required_function(props, "useTurnData", "AssistantNodeView props")?;
    let tail = use_turn_data.call1(&JsValue::UNDEFINED, &JsValue::from_str("turn-tail"))?;
    let open_file = required_function(props, "openFile", "AssistantNodeView props")?;
    let final_node = Reflect::get(&data, &JsValue::from_str("finalNode"))?;
    let owner_turn = turn.clone();
    let owner_tail = tail.clone();
    let owner_open = open_file.clone();
    let owner_final = final_node.clone();
    let owner_factory = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if owner_turn.is_null()
            || owner_turn.is_undefined()
            || required_string(&owner_turn, "status", "turn")? != "closed"
            || owner_final.is_undefined()
        {
            return Ok(JsValue::UNDEFINED);
        }
        let closing = Reflect::get(&owner_tail, &JsValue::from_str("closing"))?;
        if closing.is_null() || closing.is_undefined() {
            return Ok(JsValue::UNDEFINED);
        }
        let closing_final = required_property(&closing, "finalNode", "closing turn tail")?;
        let expected = required_property(&owner_final, "seq", "assistant final node")?;
        let actual = required_property(&closing_final, "seq", "closing final node")?;
        if !Object::is(&expected, &actual) {
            return Ok(JsValue::UNDEFINED);
        }
        Ok(object(&[
            ("turn", owner_turn.clone()),
            ("seq", expected),
            ("openFile", owner_open.clone().into()),
        ])?
        .into())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let owner = required_function(&modules.react, "useMemo", "React")?.call2(
        &modules.react,
        &owner_factory.into_js_value(),
        &Array::of4(&final_node, open_file.as_ref(), &tail, &turn),
    )?;
    let file_mentions = required_function(props, "fileMentions", "AssistantNodeView props")?;
    let resolve_mentions = file_mentions.clone();
    let mention_owner = owner.clone();
    let mention_factory = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if mention_owner.is_undefined() {
            Ok(JsValue::UNDEFINED)
        } else {
            resolve_mentions.call1(&JsValue::UNDEFINED, &mention_owner)
        }
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let mentions = required_function(&modules.react, "useMemo", "React")?.call2(
        &modules.react,
        &mention_factory.into_js_value(),
        &Array::of2(&file_mentions.into(), &owner),
    )?;
    let status = required_string(&data, "status", "assistant node data")?;
    let component = assistant_markdown_component()?;
    create_element(
        &modules.react,
        &component,
        Some(&object(&[
            (
                "blocks",
                required_property(&data, "blocks", "assistant node data")?,
            ),
            ("streaming", JsValue::from_bool(status == "running")),
            ("interrupted", JsValue::from_bool(status == "interrupted")),
            (
                "loadImage",
                required_function(props, "loadImage", "AssistantNodeView props")?.into(),
            ),
            ("mentions", mentions),
            (
                "t",
                required_function(props, "t", "AssistantNodeView props")?.into(),
            ),
        ])?),
        &[],
    )
}

fn message_image_labels(translate: &Function) -> Result<JsValue, JsValue> {
    let open_named_translate = translate.clone();
    let open_named = Closure::wrap(Box::new(move |label: JsValue| {
        open_named_translate.apply(
            &JsValue::UNDEFINED,
            &Array::of2(
                &JsValue::from_str("image.openNamed"),
                &object(&[("label", label)])?.into(),
            ),
        )
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    Ok(object(&[
        (
            "image",
            translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("image.label"))?,
        ),
        (
            "open",
            translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("image.open"))?,
        ),
        ("openNamed", open_named.into_js_value()),
        (
            "loading",
            translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("image.loading"))?,
        ),
        (
            "loadFailed",
            translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("image.loadFailed"))?,
        ),
        (
            "lightbox",
            object(&[
                (
                    "dialog",
                    translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("image.dialog"))?,
                ),
                (
                    "close",
                    translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("image.close"))?,
                ),
            ])?
            .into(),
        ),
    ])?
    .into())
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|configured| {
        configured.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation assistant was not configured").into()
        })
    })
}

fn optional_bool(value: &JsValue, key: &str) -> Result<Option<bool>, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Ok(None)
    } else {
        property
            .as_bool()
            .map(Some)
            .ok_or_else(|| js_sys::TypeError::new(&format!("{key} must be a boolean")).into())
    }
}

fn required_bool(value: &JsValue, key: &str, owner: &str) -> Result<bool, JsValue> {
    required_property(value, key, owner)?
        .as_bool()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a boolean")).into())
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

fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
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
