//! Live WASM coverage for message images, galleries, and the original-image lightbox.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Object, Promise, Reflect};
use seekdeep_client_ui_attachment::{
    configure_client_ui_attachment, image_gallery_component, image_lightbox_component,
    message_image_component,
};
use seekdeep_client_ui_primitives::configure_client_ui_primitive_icons;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hooks = []
let cursor = 0
let effects = []
let tree = null
let styles = []
let listeners = new Map()
let activeElement = null
let closeCalls = 0
let loadMode = 'resolve'
let loadCalls = []
let pendingLoads = []

class FakeHTMLElement {
  constructor(kind, props, children) { this.kind = kind; this.props = props ?? {}; this.children = children; this.parentElement = null }
  focus() { activeElement = this }
}
function sameDeps(left, right) { return left?.length === right?.length && left.every((value, index) => Object.is(value, right[index])) }
function text(node) { if (node === null || node === undefined || node === false) return ''; if (typeof node === 'string' || typeof node === 'number') return String(node); return (node.children ?? []).map(text).join('') }
function all(node, predicate, output = []) { if (node === null || node === undefined || node === false || typeof node === 'string' || typeof node === 'number') return output; if (predicate(node)) output.push(node); for (const child of node.children ?? []) all(child, predicate, output); return output }
function resolve(node) {
  if (node === null || node === undefined || node === false || typeof node === 'string' || typeof node === 'number') return node
  if (typeof node.kind === 'function') { const children = node.children.length === 0 ? undefined : node.children.length === 1 ? node.children[0] : node.children; return resolve(node.kind({ ...node.props, children })) }
  node.children = node.children.map(resolve); for (const child of node.children) if (child && typeof child === 'object') child.parentElement = node; return node
}
function reconcile(previous, next) { if (previous === null || next === null || typeof previous !== 'object' || typeof next !== 'object' || previous.kind !== next.kind) return next; previous.props = next.props; previous.children = next.children.map((child, index) => reconcile(previous.children[index], child)); for (const child of previous.children) if (child && typeof child === 'object') child.parentElement = previous; return previous }
function attachRefs(node) { if (node === null || node === undefined || typeof node !== 'object') return; if (node.props?.ref) node.props.ref.current = node; for (const child of node.children ?? []) attachRefs(child) }
function runEffects() { for (const item of effects.splice(0)) { const cleanup = item.effect(); if (typeof cleanup === 'function') hooks[item.index].cleanup = cleanup } }

export function installMessageBench() {
  hooks = []; cursor = 0; effects = []; tree = null; styles = []; listeners = new Map(); activeElement = null; closeCalls = 0; loadMode = 'resolve'; loadCalls = []; pendingLoads = []
  globalThis.HTMLElement = FakeHTMLElement
  const body = new FakeHTMLElement('body', {}, [])
  globalThis.document = {
    body,
    get activeElement() { return activeElement },
    head: { appendChild(node) { styles.push(node) } },
    createElement(kind) { return { kind, attributes: {}, setAttribute(name, value) { this.attributes[name] = value } } },
    querySelector(selector) { const match = selector.match(/data-plugin-css="([^"]+)"/); return match === null ? null : styles.find(style => style.attributes['data-plugin-css'] === match[1]) ?? null },
  }
  globalThis.window = { addEventListener(name, listener) { listeners.set(name, listener) }, removeEventListener(name, listener) { if (listeners.get(name) === listener) listeners.delete(name) } }
  const React = {
    Fragment: Symbol('Fragment'),
    createElement(kind, props, ...children) { return new FakeHTMLElement(kind, props, children.flat(Infinity).filter(value => value !== null && value !== undefined && value !== false)) },
    useState(initial) { const index = cursor++; if (!(index in hooks)) hooks[index] = { value: initial }; return [hooks[index].value, update => { hooks[index].value = typeof update === 'function' ? update(hooks[index].value) : update }] },
    useRef(initial) { const index = cursor++; if (!(index in hooks)) hooks[index] = { current: initial }; return hooks[index] },
    useCallback(callback, dependencies) { const index = cursor++; if (!(index in hooks) || !sameDeps(hooks[index].dependencies, dependencies)) hooks[index] = { value: callback, dependencies: [...dependencies] }; return hooks[index].value },
    useMemo(factory, dependencies) { const index = cursor++; if (!(index in hooks) || !sameDeps(hooks[index].dependencies, dependencies)) hooks[index] = { value: factory(), dependencies: [...dependencies] }; return hooks[index].value },
    useEffect(effect, dependencies) { const index = cursor++; if (!(index in hooks) || !sameDeps(hooks[index].dependencies, dependencies)) { hooks[index]?.cleanup?.(); hooks[index] = { dependencies: [...dependencies], cleanup: undefined }; effects.push({ index, effect }) } },
    useLayoutEffect(effect, dependencies) { this.useEffect(effect, dependencies) },
  }
  const ReactDOM = { createPortal(child, parent) { child.parentElement = parent; return child } }
  return { React, ReactDOM }
}
export function messageRender(component, props) { cursor = 0; effects = []; const next = resolve(component(props)); tree = reconcile(tree, next); attachRefs(tree); runEffects(); return tree }
export function messageUnmount() { for (const hook of hooks) hook?.cleanup?.(); hooks = []; tree = null }
export function messageObject(entries) { return Object.fromEntries(entries) }
export function messageAttachment(width = 640, height = 320, named = true) { return { attachmentId: 'sha256:' + 'a'.repeat(64), mediaType: 'image/png', bytes: 68, width, height, ...(named ? { name: 'history.png' } : {}) } }
export function messageLabels() { return { image: '图片', open: '查看原图', openNamed: label => label + '，点击查看原图', loading: '图片加载中…', loadFailed: '图片加载失败，点击重试', lightbox: { dialog: '原图预览', close: '关闭原图预览' } } }
export function messageLoader(attachment) {
  loadCalls.push(attachment)
  if (loadMode === 'resolve') return Promise.resolve('blob:history')
  if (loadMode === 'reject') return Promise.reject(new Error('offline'))
  return new Promise((resolve, reject) => pendingLoads.push({ resolve, reject }))
}
export function messageSetLoadMode(mode) { loadMode = mode }
export function messageResolveLoads(value = 'blob:late') { for (const pending of pendingLoads.splice(0)) pending.resolve(value) }
export function messageRejectLoads() { for (const pending of pendingLoads.splice(0)) pending.reject(new Error('late')) }
export function messageLoadCalls() { return loadCalls }
export function messageClose() { closeCalls += 1 }
export function messageCloseCalls() { return closeCalls }
export function messageOpener() { const opener = new FakeHTMLElement('button', {}, []); opener.focus(); return opener }
export function messageClearActive() { activeElement = null }
export function messageActive() { return activeElement }
export function messageDispatchKey(key) { listeners.get('keydown')?.({ key }) }
export function messageText(node) { return text(node) }
export function messageFindKind(root, kind) { return all(root, node => node.kind === kind)[0] }
export function messageFindKinds(root, kind) { return all(root, node => node.kind === kind) }
export function messageFindLabel(root, label) { return all(root, node => node.props?.['aria-label'] === label)[0] }
export function messageFindButtonText(root, label) { return all(root, node => node.kind === 'button' && text(node) === label)[0] }
export function messageFindClass(root, className) { return all(root, node => String(node.props?.className ?? '').split(/\s+/).includes(className))[0] }
export function messageClick(node, eventName = 'onClick') { node.props?.[eventName]?.() }
export function messageStyles() { return styles }
export function messageTick() { return Promise.resolve().then(() => Promise.resolve()).then(() => Promise.resolve()) }
"#)]
extern "C" {
    fn installMessageBench() -> JsValue;
    fn messageRender(component: &JsValue, props: &JsValue) -> JsValue;
    fn messageUnmount();
    fn messageObject(entries: &Array) -> JsValue;
    fn messageAttachment(width: f64, height: f64, named: bool) -> JsValue;
    fn messageLabels() -> JsValue;
    fn messageLoader(attachment: &JsValue) -> Promise;
    fn messageSetLoadMode(mode: &str);
    fn messageResolveLoads(value: &str);
    fn messageRejectLoads();
    fn messageLoadCalls() -> Array;
    fn messageClose();
    fn messageCloseCalls() -> u32;
    fn messageOpener() -> JsValue;
    fn messageClearActive();
    fn messageActive() -> JsValue;
    fn messageDispatchKey(key: &str);
    fn messageText(node: &JsValue) -> String;
    fn messageFindKind(root: &JsValue, kind: &str) -> JsValue;
    fn messageFindKinds(root: &JsValue, kind: &str) -> Array;
    fn messageFindLabel(root: &JsValue, label: &str) -> JsValue;
    fn messageFindButtonText(root: &JsValue, label: &str) -> JsValue;
    fn messageFindClass(root: &JsValue, class_name: &str) -> JsValue;
    fn messageClick(node: &JsValue, event_name: &str);
    fn messageStyles() -> Array;
    fn messageTick() -> Promise;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn props(entries: &[(&str, JsValue)]) -> Object {
    let values = Array::new();
    for (key, value) in entries {
        values.push(&Array::of2(&JsValue::from_str(key), value));
    }
    messageObject(&values).unchecked_into()
}

fn setup() -> (JsValue, JsValue, JsValue) {
    let bench = installMessageBench();
    let react = property(&bench, "React");
    configure_client_ui_primitive_icons(react.clone());
    configure_client_ui_attachment(react, property(&bench, "ReactDOM")).unwrap();
    (
        image_lightbox_component().unwrap(),
        message_image_component().unwrap(),
        image_gallery_component().unwrap(),
    )
}

async fn tick() {
    JsFuture::from(messageTick()).await.unwrap();
}

fn message_props(attachment: JsValue, variant: &str) -> Object {
    let loader = Closure::wrap(
        Box::new(move |attachment: JsValue| messageLoader(&attachment))
            as Box<dyn FnMut(JsValue) -> Promise>,
    );
    props(&[
        ("attachment", attachment),
        ("load", loader.into_js_value()),
        ("variant", JsValue::from_str(variant)),
        ("labels", messageLabels()),
    ])
}

#[wasm_bindgen_test]
fn lightbox_focus_escape_mask_and_restore_match_source() {
    let (lightbox, _message, _gallery) = setup();
    assert_eq!(messageStyles().length(), 2);
    let opener = messageOpener();
    let close = Closure::wrap(Box::new(messageClose) as Box<dyn FnMut()>);
    let lightbox_props = props(&[
        ("src", JsValue::from_str("blob:original")),
        ("alt", JsValue::from_str("原图")),
        ("labels", property(&messageLabels(), "lightbox")),
        ("onClose", close.into_js_value()),
    ]);
    let tree = messageRender(&lightbox, lightbox_props.as_ref());
    let close = messageFindLabel(&tree, "关闭原图预览");
    assert!(Object::is(&messageActive(), &close));
    messageDispatchKey("a");
    assert_eq!(messageCloseCalls(), 0);
    messageDispatchKey("Escape");
    messageClick(&close, "onClick");
    assert_eq!(messageCloseCalls(), 2);
    messageClick(&messageFindKind(&tree, "img"), "onMouseDown");
    assert_eq!(messageCloseCalls(), 2);
    messageClick(
        &messageFindClass(&tree, "seekdeep-attachment-image-lightbox-mask"),
        "onMouseDown",
    );
    assert_eq!(messageCloseCalls(), 3);
    messageUnmount();
    assert!(Object::is(&messageActive(), &opener));

    let (lightbox, _message, _gallery) = setup();
    messageClearActive();
    let close = Closure::wrap(Box::new(messageClose) as Box<dyn FnMut()>);
    let no_owner_props = props(&[
        ("src", JsValue::from_str("blob:original")),
        ("alt", JsValue::from_str("原图")),
        ("labels", property(&messageLabels(), "lightbox")),
        ("onClose", close.into_js_value()),
    ]);
    let _ = messageRender(&lightbox, no_owner_props.as_ref());
    messageUnmount();
}

#[wasm_bindgen_test(async)]
async fn message_load_bounds_open_close_and_unnamed_fallback_match_source() {
    let (_lightbox, message, _gallery) = setup();
    let attachment = messageAttachment(640.0, 320.0, true);
    let props = message_props(attachment.clone(), "single");
    let tree = messageRender(&message, props.as_ref());
    let frame = messageFindLabel(&tree, "history.png，点击查看原图");
    let style = property(&property(&frame, "props"), "style");
    assert_eq!(property(&style, "width").as_f64(), Some(240.0));
    assert_eq!(property(&style, "height").as_f64(), Some(120.0));
    assert!(messageText(&tree).contains("图片加载中…"));
    messageClick(&frame, "onClick");
    assert!(messageFindKind(&tree, "dialog").is_undefined());
    tick().await;
    let tree = messageRender(&message, props.as_ref());
    let image = messageFindKind(&tree, "img");
    assert_eq!(
        property(&property(&image, "props"), "src")
            .as_string()
            .as_deref(),
        Some("blob:history")
    );
    assert!(Object::is(&messageLoadCalls().get(0), &attachment));
    messageClick(
        &messageFindLabel(&tree, "history.png，点击查看原图"),
        "onClick",
    );
    let tree = messageRender(&message, props.as_ref());
    assert!(!messageFindLabel(&tree, "原图预览").is_undefined());
    messageClick(&messageFindLabel(&tree, "关闭原图预览"), "onClick");
    let tree = messageRender(&message, props.as_ref());
    assert!(messageFindLabel(&tree, "原图预览").is_undefined());
    messageUnmount();

    let (_lightbox, message, _gallery) = setup();
    let props = message_props(messageAttachment(100.0, 100.0, false), "single");
    let _ = messageRender(&message, props.as_ref());
    tick().await;
    let tree = messageRender(&message, props.as_ref());
    assert!(!messageFindLabel(&tree, "图片，点击查看原图").is_undefined());
    assert_eq!(
        property(&property(&messageFindKind(&tree, "img"), "props"), "alt")
            .as_string()
            .as_deref(),
        Some("图片")
    );
    messageUnmount();
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)] // Closed source cases share one async loader harness.
async fn retry_aspect_ratio_tiles_gallery_and_late_settlement_match_source() {
    let (_lightbox, message, _gallery) = setup();
    messageSetLoadMode("reject");
    let tile_props = message_props(messageAttachment(640.0, 320.0, true), "tile");
    let _ = messageRender(&message, tile_props.as_ref());
    tick().await;
    let tree = messageRender(&message, tile_props.as_ref());
    let retry = messageFindButtonText(&tree, "图片加载失败，点击重试");
    assert!(
        !retry.is_undefined(),
        "retry control missing: {}",
        messageText(&tree)
    );
    assert_eq!(
        property(&property(&retry, "props"), "data-variant")
            .as_string()
            .as_deref(),
        Some("tile")
    );
    messageSetLoadMode("resolve");
    messageClick(&retry, "onClick");
    let _ = messageRender(&message, tile_props.as_ref());
    tick().await;
    let tree = messageRender(&message, tile_props.as_ref());
    assert!(!messageFindKind(&tree, "img").is_undefined());
    assert_eq!(messageLoadCalls().length(), 2);
    messageUnmount();

    for (width, height, expected_width, expected_height, position) in [
        (100.0, 2000.0, 60.0, 240.0, "center top"),
        (4000.0, 100.0, 240.0, 60.0, "left center"),
        (100.0, 100.0, 100.0, 100.0, "center"),
    ] {
        let (_lightbox, message, _gallery) = setup();
        let props = message_props(messageAttachment(width, height, true), "single");
        let tree = messageRender(&message, props.as_ref());
        let frame = messageFindLabel(&tree, "history.png，点击查看原图");
        assert!(
            !frame.is_undefined(),
            "image frame missing: {}",
            messageText(&tree)
        );
        let style = property(&property(&frame, "props"), "style");
        assert_eq!(property(&style, "width").as_f64(), Some(expected_width));
        assert_eq!(property(&style, "height").as_f64(), Some(expected_height));
        tick().await;
        let tree = messageRender(&message, props.as_ref());
        let image = messageFindKind(&tree, "img");
        assert!(
            !image.is_undefined(),
            "loaded image missing: {}",
            messageText(&tree)
        );
        let image_style = property(&property(&image, "props"), "style");
        assert_eq!(
            property(&image_style, "objectPosition")
                .as_string()
                .as_deref(),
            Some(position)
        );
        messageUnmount();
    }

    let (_lightbox, _message, gallery) = setup();
    let empty = props(&[
        ("images", Array::new().into()),
        (
            "load",
            property(
                message_props(messageAttachment(1.0, 1.0, true), "tile").as_ref(),
                "load",
            ),
        ),
        ("align", JsValue::from_str("start")),
        ("labels", messageLabels()),
    ]);
    assert!(messageRender(&gallery, empty.as_ref()).is_null());
    messageUnmount();

    let (_lightbox, _message, gallery) = setup();
    let images = Array::new();
    for _ in 0..3 {
        images.push(&props(&[(
            "attachment",
            messageAttachment(640.0, 320.0, true),
        )]));
    }
    let loader = Closure::wrap(
        Box::new(move |attachment: JsValue| messageLoader(&attachment))
            as Box<dyn FnMut(JsValue) -> Promise>,
    );
    let gallery_props = props(&[
        ("images", images.into()),
        ("load", loader.into_js_value()),
        ("align", JsValue::from_str("end")),
        ("labels", messageLabels()),
    ]);
    let tree = messageRender(&gallery, gallery_props.as_ref());
    assert!(!tree.is_null(), "non-empty gallery returned null");
    assert_eq!(
        property(&property(&tree, "props"), "data-align")
            .as_string()
            .as_deref(),
        Some("end")
    );
    let variants = messageFindKinds(&tree, "button");
    assert_eq!(variants.length(), 3);
    for frame in variants.iter() {
        assert_eq!(
            property(&property(&frame, "props"), "data-variant")
                .as_string()
                .as_deref(),
            Some("tile")
        );
        assert!(property(&property(&frame, "props"), "style").is_undefined());
    }
    messageUnmount();

    let (_lightbox, message, _gallery) = setup();
    messageSetLoadMode("defer");
    let props = message_props(messageAttachment(640.0, 320.0, true), "single");
    let _ = messageRender(&message, props.as_ref());
    messageUnmount();
    messageResolveLoads("blob:late");
    tick().await;
    messageRejectLoads();
}
