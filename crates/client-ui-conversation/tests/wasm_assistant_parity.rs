//! Live WASM coverage for compiled assistant block composition and node ownership.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_ui_conversation::{
    assistant_markdown_component, assistant_node_view_component,
    configure_client_ui_conversation_assistant, configure_client_ui_conversation_reasoning,
};
use seekdeep_client_ui_primitives::{
    configure_client_ui_primitive_atoms, configure_client_ui_primitive_dialogs,
    configure_client_ui_primitive_icons, disclosure_row_component, icon_components,
};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hooks = []
let cursor = 0
let styles = []
let calls = []
let mentionOwners = []
function sameDeps(left, right) { return left?.length === right?.length && left.every((value, index) => Object.is(value, right[index])) }
function marker(name) { const component = props => ({ kind: name, props, children: [] }); component.markerName = name; return component }
const MarkdownText = marker('MarkdownText')
const JsonBlock = marker('JsonBlock')
const ImageGallery = marker('ImageGallery')
export function installAssistantBench() {
  hooks = []; cursor = 0; styles = []; calls = []; mentionOwners = []
  globalThis.requestAnimationFrame = callback => { callback(0); return 1 }
  globalThis.cancelAnimationFrame = () => {}
  globalThis.document = { head: { appendChild(node) { styles.push(node) } }, createElement(kind) { return { kind, attributes: {}, setAttribute(name, value) { this.attributes[name] = value } } }, querySelector(selector) { const match = selector.match(/data-plugin-css="([^"]+)"/); return match === null ? null : styles.find(style => style.attributes['data-plugin-css'] === match[1]) ?? null } }
  const React = {
    Fragment: Symbol('Fragment'),
    createElement(kind, props, ...children) { return { kind, props: props ?? {}, children: children.flat(Infinity).filter(value => value !== null && value !== undefined && value !== false) } },
    memo(component) { return component },
    useMemo(factory, dependencies) { const index = cursor++; if (!(index in hooks) || !sameDeps(hooks[index].dependencies, dependencies)) hooks[index] = { value: factory(), dependencies: [...dependencies] }; return hooks[index].value },
    useState(initial) { const index = cursor++; if (!(index in hooks)) hooks[index] = { value: initial }; return [hooks[index].value, update => { hooks[index].value = typeof update === 'function' ? update(hooks[index].value) : update }] },
    useRef(initial) { const index = cursor++; if (!(index in hooks)) hooks[index] = { current: initial }; return hooks[index] },
    useCallback(callback, dependencies) { const index = cursor++; if (!(index in hooks) || !sameDeps(hooks[index].dependencies, dependencies)) hooks[index] = { value: callback, dependencies: [...dependencies] }; return hooks[index].value },
    useEffect() { cursor += 1 }, useLayoutEffect() { cursor += 1 },
  }
  const ReactDOM = { createPortal(child) { return child } }
  return { React, ReactDOM, uiPrimitives: { MarkdownText, JsonBlock, DisclosureRow: 'DisclosureRow', IconThinkOutline14: 'Think' }, uiAttachment: { ImageGallery } }
}
export function assistantRender(component, props) { cursor = 0; return component(props) }
export function assistantObject(entries) { return Object.fromEntries(entries) }
export function assistantKind(node) { return typeof node?.kind === 'function' ? node.kind.markerName ?? 'ReasoningRow' : node?.kind }
export function assistantChildren(node) { return node?.children ?? [] }
export function assistantTranslate(key, vars) { calls.push([key, vars]); const map = { copy: '复制', copied: '复制成功', 'message.unknownBlock': '未知内容块', 'message.stopped': '已停止', 'image.serviceUnavailable': '图片服务不可用', 'image.label': '图片', 'image.openOriginal': '查看原图', 'image.loading': '加载', 'image.loadFailed': '失败', 'image.preview': '原图预览', 'image.closePreview': '关闭' }; if (key === 'json.truncated') return '截断:' + vars.total; if (key === 'image.openOriginalLabel') return vars.label + '，点击查看原图'; return map[key] ?? key }
export function assistantCalls() { return calls }
export function assistantMentions(owner) { mentionOwners.push(owner); return { owner } }
export function assistantMentionOwners() { return mentionOwners }
export function assistantLoad() { return Promise.resolve('blob:image') }
export function assistantRejectMessage(promise) { return Promise.resolve(promise).then(() => undefined, error => String(error)) }
export function assistantStyles() { return styles }
"#)]
extern "C" {
    fn installAssistantBench() -> JsValue;
    fn assistantRender(component: &JsValue, props: &JsValue) -> JsValue;
    fn assistantObject(entries: &Array) -> JsValue;
    fn assistantKind(node: &JsValue) -> String;
    fn assistantChildren(node: &JsValue) -> Array;
    fn assistantTranslate(key: &str, vars: &JsValue) -> String;
    fn assistantCalls() -> Array;
    fn assistantMentions(owner: &JsValue) -> JsValue;
    fn assistantMentionOwners() -> Array;
    fn assistantLoad() -> Promise;
    fn assistantRejectMessage(promise: &JsValue) -> Promise;
    fn assistantStyles() -> Array;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}
fn object(entries: &[(&str, JsValue)]) -> Object {
    let values = Array::new();
    for (key, value) in entries {
        values.push(&Array::of2(&JsValue::from_str(key), value));
    }
    assistantObject(&values).unchecked_into()
}
fn block(kind: &str, entries: &[(&str, JsValue)]) -> JsValue {
    let mut all = vec![("kind", JsValue::from_str(kind))];
    all.extend(entries.iter().cloned());
    object(&all).into()
}

fn translate() -> Function {
    Closure::wrap(
        Box::new(move |key: String, vars: JsValue| assistantTranslate(&key, &vars))
            as Box<dyn FnMut(String, JsValue) -> String>,
    )
    .into_js_value()
    .dyn_into()
    .unwrap()
}

fn setup() -> (JsValue, JsValue, JsValue) {
    let bench = installAssistantBench();
    let react = property(&bench, "React");
    let react_dom = property(&bench, "ReactDOM");
    configure_client_ui_primitive_atoms(react.clone(), react_dom.clone()).unwrap();
    configure_client_ui_primitive_dialogs(react.clone(), react_dom).unwrap();
    configure_client_ui_primitive_icons(react.clone());
    let primitives = property(&bench, "uiPrimitives");
    Reflect::set(
        &primitives,
        &JsValue::from_str("DisclosureRow"),
        &disclosure_row_component().unwrap(),
    )
    .unwrap();
    Reflect::set(
        &primitives,
        &JsValue::from_str("IconThinkOutline14"),
        &property(&icon_components().unwrap(), "IconThinkOutline14"),
    )
    .unwrap();
    configure_client_ui_conversation_reasoning(react.clone(), primitives).unwrap();
    configure_client_ui_conversation_assistant(
        react,
        property(&bench, "uiPrimitives"),
        property(&bench, "uiAttachment"),
    )
    .unwrap();
    (
        bench,
        assistant_markdown_component().unwrap(),
        assistant_node_view_component().unwrap(),
    )
}

fn assistant_props(blocks: Array, streaming: bool) -> Object {
    object(&[
        ("blocks", blocks.into()),
        ("streaming", JsValue::from_bool(streaming)),
        ("t", translate().into()),
    ])
}

#[wasm_bindgen_test]
fn text_reasoning_images_unknown_tool_heads_and_interrupted_order_match_source() {
    let (_bench, component, _node_view) = setup();
    let images = Array::new();
    images.push(&block("text", &[("text", JsValue::from_str("before"))]));
    images.push(&block(
        "image",
        &[(
            "attachment",
            object(&[("attachmentId", JsValue::from_str("a"))]).into(),
        )],
    ));
    images.push(&block(
        "image",
        &[(
            "attachment",
            object(&[("attachmentId", JsValue::from_str("b"))]).into(),
        )],
    ));
    images.push(&block(
        "reasoning",
        &[("text", JsValue::from_str("think\nlatest"))],
    ));
    images.push(&block("tool-call", &[]));
    images.push(&block(
        "other",
        &[(
            "block",
            object(&[("type", JsValue::from_str("mystery"))]).into(),
        )],
    ));
    let props = assistant_props(images, true);
    Reflect::set(&props, &JsValue::from_str("interrupted"), &JsValue::TRUE).unwrap();
    Reflect::set(
        &props,
        &JsValue::from_str("mentions"),
        &object(&[("files", JsValue::TRUE)]),
    )
    .unwrap();
    let tree = assistantRender(&component, props.as_ref());
    assert_eq!(assistantKind(&tree), "div");
    assert_eq!(
        property(&property(&tree, "props"), "data-streaming").as_bool(),
        Some(true)
    );
    let body = assistantChildren(&tree).get(0);
    let children = assistantChildren(&body);
    assert_eq!(children.length(), 5);
    assert_eq!(assistantKind(&children.get(0)), "MarkdownText");
    assert_eq!(assistantKind(&children.get(1)), "ImageGallery");
    assert_eq!(
        Array::from(&property(&property(&children.get(1), "props"), "images")).length(),
        2
    );
    assert_eq!(assistantKind(&children.get(2)), "ReasoningRow");
    assert_eq!(
        property(&property(&children.get(2), "props"), "running").as_bool(),
        Some(false)
    );
    assert_eq!(assistantKind(&children.get(3)), "JsonBlock");
    assert_eq!(assistantKind(&children.get(4)), "span");
    assert_eq!(
        assistantChildren(&children.get(4))
            .get(0)
            .as_string()
            .as_deref(),
        Some("已停止")
    );
}

#[wasm_bindgen_test(async)]
async fn visibility_labels_default_loader_and_truncated_formatter_match_source() {
    let (_bench, component, _node_view) = setup();
    let tool_only = Array::of1(&block("tool-call", &[]));
    assert!(assistantRender(&component, assistant_props(tool_only, false).as_ref()).is_null());
    assert!(assistantRender(&component, assistant_props(Array::new(), false).as_ref()).is_null());
    let streaming = assistantRender(&component, assistant_props(Array::new(), true).as_ref());
    assert!(!streaming.is_null());

    let text_blocks = Array::of1(&block("text", &[("text", JsValue::from_str("hello"))]));
    let props = assistant_props(text_blocks.clone(), false);
    let first = assistantRender(&component, props.as_ref());
    let first_labels = property(
        &property(
            &assistantChildren(&assistantChildren(&first).get(0)).get(0),
            "props",
        ),
        "codeLabels",
    );
    let second = assistantRender(&component, props.as_ref());
    let second_labels = property(
        &property(
            &assistantChildren(&assistantChildren(&second).get(0)).get(0),
            "props",
        ),
        "codeLabels",
    );
    assert!(Object::is(&first_labels, &second_labels));

    let image_blocks = Array::of1(&block(
        "image",
        &[(
            "attachment",
            object(&[("attachmentId", JsValue::from_str("a"))]).into(),
        )],
    ));
    let tree = assistantRender(&component, assistant_props(image_blocks, false).as_ref());
    let gallery = assistantChildren(&assistantChildren(&tree).get(0)).get(0);
    let loader = property(&property(&gallery, "props"), "load")
        .dyn_into::<Function>()
        .unwrap();
    let pending = loader.call1(&JsValue::UNDEFINED, &JsValue::NULL).unwrap();
    let message = JsFuture::from(assistantRejectMessage(&pending))
        .await
        .unwrap();
    assert!(message.as_string().unwrap().contains("图片服务不可用"));

    let unknown = Array::of1(&block("other", &[("block", JsValue::from_str("x"))]));
    let tree = assistantRender(&component, assistant_props(unknown, false).as_ref());
    let json = assistantChildren(&assistantChildren(&tree).get(0)).get(0);
    let footer = property(&property(&json, "props"), "truncatedLabel")
        .dyn_into::<Function>()
        .unwrap();
    assert_eq!(
        footer
            .call1(&JsValue::UNDEFINED, &JsValue::from_f64(42.0))
            .unwrap()
            .as_string()
            .as_deref(),
        Some("截断:42")
    );
}

#[wasm_bindgen_test]
fn assistant_node_view_gates_turn_tail_owner_and_mentions() {
    let (_bench, _component, node_view) = setup();
    let final_node = object(&[("seq", JsValue::from_f64(9.0))]);
    let turn = object(&[("status", JsValue::from_str("closed"))]);
    let node = object(&[
        (
            "data",
            object(&[
                ("blocks", Array::new().into()),
                ("status", JsValue::from_str("interrupted")),
                ("finalNode", final_node.clone().into()),
            ])
            .into(),
        ),
        (
            "location",
            object(&[
                ("kind", JsValue::from_str("turn")),
                ("turn", turn.clone().into()),
            ])
            .into(),
        ),
    ]);
    let use_tail = Closure::wrap(Box::new(move |_key: String| {
        object(&[(
            "closing",
            object(&[("finalNode", final_node.clone().into())]).into(),
        )])
        .into()
    }) as Box<dyn FnMut(String) -> JsValue>);
    let mentions = Closure::wrap(Box::new(move |owner: JsValue| assistantMentions(&owner))
        as Box<dyn FnMut(JsValue) -> JsValue>);
    let open_file = Closure::wrap(Box::new(move |_path: String| {}) as Box<dyn FnMut(String)>);
    let load =
        Closure::wrap(Box::new(move |_attachment: JsValue| assistantLoad())
            as Box<dyn FnMut(JsValue) -> Promise>);
    let props = object(&[
        ("node", node.into()),
        ("useTurnData", use_tail.into_js_value()),
        ("openFile", open_file.into_js_value()),
        ("loadImage", load.into_js_value()),
        ("fileMentions", mentions.into_js_value()),
        ("t", translate().into()),
    ]);
    let tree = assistantRender(&node_view, props.as_ref());
    let rendered = property(&tree, "props");
    assert_eq!(property(&rendered, "streaming").as_bool(), Some(false));
    assert_eq!(property(&rendered, "interrupted").as_bool(), Some(true));
    assert_eq!(assistantMentionOwners().length(), 1);
    let owner = assistantMentionOwners().get(0);
    assert!(Object::is(&property(&owner, "turn"), turn.as_ref()));
    assert_eq!(property(&owner, "seq").as_f64(), Some(9.0));
    assert!(!property(&rendered, "mentions").is_undefined());
}
