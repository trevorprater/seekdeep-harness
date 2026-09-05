//! Live WASM coverage for the attachment rail and drop overlay.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Object, Reflect};
use seekdeep_client_ui_attachment::{
    attachment_rail_component, configure_client_ui_attachment, drop_overlay_component,
};
use seekdeep_client_ui_primitives::{configure_client_ui_primitive_icons, icon_components};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hooks = []
let cursor = 0
let layoutEffects = []
let effects = []
let tree = null
let styles = []
let observers = []
let reducedMotion = false
let openCalls = []
let removeCalls = []

function sameDeps(left, right) { return left?.length === right?.length && left.every((value, index) => Object.is(value, right[index])) }
function text(node) { if (node === null || node === undefined || node === false) return ''; if (typeof node === 'string' || typeof node === 'number') return String(node); return (node.children ?? []).map(text).join('') }
function all(node, predicate, output = []) { if (node === null || node === undefined || node === false || typeof node === 'string' || typeof node === 'number') return output; if (predicate(node)) output.push(node); for (const child of node.children ?? []) all(child, predicate, output); return output }
function resolve(node) { if (node === null || node === undefined || node === false || typeof node === 'string' || typeof node === 'number') return node; if (typeof node.kind === 'function') { const children = node.children.length === 0 ? undefined : node.children.length === 1 ? node.children[0] : node.children; return resolve(node.kind({ ...node.props, children })) } node.children = node.children.map(resolve); return node }
function reconcile(previous, next) { if (previous === null || next === null || typeof previous !== 'object' || typeof next !== 'object' || previous.kind !== next.kind) return next; previous.props = next.props; previous.children = next.children.map((child, index) => reconcile(previous.children[index], child)); return previous }
function attachRefs(node) { if (node === null || node === undefined || typeof node !== 'object') return; if (node.props?.ref) node.props.ref.current = node; for (const child of node.children ?? []) attachRefs(child) }
function run(queue) { for (const item of queue.splice(0)) { const cleanup = item.effect(); if (typeof cleanup === 'function') hooks[item.index].cleanup = cleanup } }
function effectHook(effect, deps, queue) { const index = cursor++; if (!(index in hooks) || !sameDeps(hooks[index].deps, deps)) { hooks[index]?.cleanup?.(); hooks[index] = { deps: [...deps] }; queue.push({ index, effect }) } }

export function installRailBench() {
  hooks = []; cursor = 0; layoutEffects = []; effects = []; tree = null; styles = []; observers = []; reducedMotion = false; openCalls = []; removeCalls = []
  globalThis.window = { matchMedia() { return { matches: reducedMotion } } }
  globalThis.ResizeObserver = class { constructor(callback) { this.callback = callback; this.observed = []; observers.push(this) } observe(element) { this.observed.push(element) } disconnect() { this.observed = [] } }
  globalThis.document = { body: { kind: 'body' }, head: { appendChild(node) { styles.push(node) } }, createElement(kind) { return { kind, attributes: {}, setAttribute(name, value) { this.attributes[name] = value } } }, querySelector(selector) { const match = selector.match(/data-plugin-css="([^"]+)"/); return match === null ? null : styles.find(style => style.attributes['data-plugin-css'] === match[1]) ?? null } }
  const React = {
    Fragment: Symbol('Fragment'),
    createElement(kind, props, ...children) { return { kind, props: props ?? {}, children: children.flat(Infinity).filter(value => value !== null && value !== undefined && value !== false), scrollLeft: 0, scrollWidth: 0, clientWidth: 0, listeners: new Map(), scrollCalls: [], addEventListener(name, listener, options) { this.listeners.set(name, { listener, options }) }, removeEventListener(name, listener) { if (this.listeners.get(name)?.listener === listener) this.listeners.delete(name) }, scrollBy(options) { this.scrollCalls.push(options); this.scrollLeft = Math.max(0, Math.min(this.scrollWidth - this.clientWidth, this.scrollLeft + options.left)) } } },
    useState(initial) { const index = cursor++; if (!(index in hooks)) hooks[index] = { value: initial }; return [hooks[index].value, update => { hooks[index].value = typeof update === 'function' ? update(hooks[index].value) : update }] },
    useRef(initial) { const index = cursor++; if (!(index in hooks)) hooks[index] = { current: initial }; return hooks[index] },
    useCallback(callback, deps) { const index = cursor++; if (!(index in hooks) || !sameDeps(hooks[index].deps, deps)) hooks[index] = { value: callback, deps: [...deps] }; return hooks[index].value },
    useLayoutEffect(effect, deps) { effectHook(effect, deps, layoutEffects) },
    useEffect(effect, deps) { effectHook(effect, deps, effects) },
    useMemo(factory, deps) { const index = cursor++; if (!(index in hooks) || !sameDeps(hooks[index].deps, deps)) hooks[index] = { value: factory(), deps: [...deps] }; return hooks[index].value },
  }
  const ReactDOM = { createPortal(child, parent) { child.parentElement = parent; return child } }
  return { React, ReactDOM }
}
export function railRender(component, props) { cursor = 0; layoutEffects = []; effects = []; const next = resolve(component(props)); tree = reconcile(tree, next); attachRefs(tree); run(layoutEffects); run(effects); return tree }
export function railUnmount() { for (const hook of hooks) hook?.cleanup?.(); hooks = []; tree = null }
export function railObject(entries) { return Object.fromEntries(entries) }
export function railItem(id) { return { id, previewUrl: 'blob:' + id, alt: id + '.png', removeLabel: '移除图片 ' + id + '.png' } }
export function railLabels() { return { group: '待发送图片', open: '查看原图', scrollLeft: '向左滚动图片', scrollRight: '向右滚动图片' } }
export function railOpen(item) { openCalls.push(item) }
export function railRemove(item) { removeCalls.push(item) }
export function railOpenCalls() { return openCalls }
export function railRemoveCalls() { return removeCalls }
export function railFindRole(root, role) { return all(root, node => node.props?.role === role)[0] }
export function railFindLabel(root, label) { return all(root, node => node.props?.['aria-label'] === label)[0] }
export function railFindTitle(root, title) { return all(root, node => node.props?.title === title)[0] }
export function railFindKinds(root, kind) { return all(root, node => node.kind === kind) }
export function railText(root) { return text(root) }
export function railClick(node, eventName = 'onClick') { node.props?.[eventName]?.() }
export function railSetGeometry(node, scrollWidth, clientWidth, scrollLeft = 0) { node.scrollWidth = scrollWidth; node.clientWidth = clientWidth; node.scrollLeft = scrollLeft }
export function railScrollCalls(node) { return node.scrollCalls }
export function railScroll(node) { node.props.onScroll() }
export function railResize() { observers.at(-1)?.callback([], undefined) }
export function railObserved() { return observers.at(-1)?.observed ?? [] }
export function railWheel(node, deltaX, deltaY, deltaMode) { let prevented = false; const event = { deltaX, deltaY, deltaMode, preventDefault() { prevented = true } }; node.listeners.get('wheel')?.listener(event); return prevented }
export function railWheelPassive(node) { return node.listeners.get('wheel')?.options?.passive }
export function railSetReducedMotion(value) { reducedMotion = value }
export function railStyles() { return styles }
export function overlaySvgInner(root) { return all(root, node => node.kind === 'svg')[0]?.props?.dangerouslySetInnerHTML?.__html }
"#)]
extern "C" {
    fn installRailBench() -> JsValue;
    fn railRender(component: &JsValue, props: &JsValue) -> JsValue;
    fn railUnmount();
    fn railObject(entries: &Array) -> JsValue;
    fn railItem(id: &str) -> JsValue;
    fn railLabels() -> JsValue;
    fn railOpen(item: &JsValue);
    fn railRemove(item: &JsValue);
    fn railOpenCalls() -> Array;
    fn railRemoveCalls() -> Array;
    fn railFindRole(root: &JsValue, role: &str) -> JsValue;
    fn railFindLabel(root: &JsValue, label: &str) -> JsValue;
    fn railFindTitle(root: &JsValue, title: &str) -> JsValue;
    fn railFindKinds(root: &JsValue, kind: &str) -> Array;
    fn railText(root: &JsValue) -> String;
    fn railClick(node: &JsValue, event_name: &str);
    fn railSetGeometry(node: &JsValue, scroll_width: f64, client_width: f64, scroll_left: f64);
    fn railScrollCalls(node: &JsValue) -> Array;
    fn railScroll(node: &JsValue);
    fn railResize();
    fn railObserved() -> Array;
    fn railWheel(node: &JsValue, delta_x: f64, delta_y: f64, delta_mode: f64) -> bool;
    fn railWheelPassive(node: &JsValue) -> JsValue;
    fn railSetReducedMotion(value: bool);
    fn railStyles() -> Array;
    fn overlaySvgInner(root: &JsValue) -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}
fn props(entries: &[(&str, JsValue)]) -> Object {
    let values = Array::new();
    for (key, value) in entries {
        values.push(&Array::of2(&JsValue::from_str(key), value));
    }
    railObject(&values).unchecked_into()
}

fn setup() -> (JsValue, JsValue) {
    let bench = installRailBench();
    let react = property(&bench, "React");
    configure_client_ui_primitive_icons(react.clone());
    configure_client_ui_attachment(
        react,
        property(&bench, "ReactDOM"),
        icon_components().unwrap().into(),
    )
    .unwrap();
    (
        attachment_rail_component().unwrap(),
        drop_overlay_component().unwrap(),
    )
}

fn rail_props(items: Array) -> Object {
    let open =
        Closure::wrap(Box::new(move |item: JsValue| railOpen(&item)) as Box<dyn FnMut(JsValue)>);
    let remove =
        Closure::wrap(Box::new(move |item: JsValue| railRemove(&item)) as Box<dyn FnMut(JsValue)>);
    props(&[
        ("items", items.into()),
        ("labels", railLabels()),
        ("onOpen", open.into_js_value()),
        ("onRemove", remove.into_js_value()),
    ])
}

#[wasm_bindgen_test]
fn overlay_portal_copy_and_disabled_illustration_match_source() {
    let (_rail, overlay) = setup();
    assert_eq!(railStyles().length(), 4);
    let enabled = props(&[
        ("disabled", JsValue::FALSE),
        (
            "labels",
            props(&[
                ("title", JsValue::from_str("拖入")),
                ("desc", JsValue::from_str("限制")),
            ])
            .into(),
        ),
    ]);
    let tree = railRender(&overlay, enabled.as_ref());
    assert_eq!(property(&tree, "parentElement").as_string(), None);
    assert_eq!(railText(&tree), "拖入限制");
    let enabled_inner = overlaySvgInner(&tree).as_string().unwrap();
    railUnmount();
    let (_rail, overlay) = setup();
    let disabled = props(&[
        ("disabled", JsValue::TRUE),
        (
            "labels",
            props(&[
                ("title", JsValue::from_str("当前无法添加图片")),
                ("desc", JsValue::from_str("限制")),
            ])
            .into(),
        ),
    ]);
    let tree = railRender(&overlay, disabled.as_ref());
    assert_eq!(railText(&tree), "当前无法添加图片");
    assert_ne!(overlaySvgInner(&tree).as_string().unwrap(), enabled_inner);
    railUnmount();
}

#[wasm_bindgen_test]
fn thumbnails_callbacks_edges_paging_resize_and_growth_match_source() {
    let (rail, _overlay) = setup();
    let first = Array::of2(&railItem("a"), &railItem("b"));
    let props = rail_props(first.clone());
    let tree = railRender(&rail, props.as_ref());
    let group = railFindRole(&tree, "group");
    let images = railFindKinds(&group, "img");
    assert_eq!(
        property(&property(&images.get(0), "props"), "alt")
            .as_string()
            .as_deref(),
        Some("a.png")
    );
    railClick(&railFindTitle(&tree, "查看原图"), "onClick");
    railClick(&railFindLabel(&tree, "移除图片 b.png"), "onClick");
    assert!(Object::is(&railOpenCalls().get(0), &first.get(0)));
    assert!(Object::is(&railRemoveCalls().get(0), &first.get(1)));
    railSetGeometry(&group, 400.0, 200.0, 0.0);
    railScroll(&group);
    let tree = railRender(&rail, props.as_ref());
    assert!(railFindLabel(&tree, "向左滚动图片").is_undefined());
    railClick(&railFindLabel(&tree, "向右滚动图片"), "onClick");
    let call = railScrollCalls(&group).get(0);
    assert_eq!(property(&call, "left").as_f64(), Some(200.0));
    assert_eq!(
        property(&call, "behavior").as_string().as_deref(),
        Some("smooth")
    );
    railSetGeometry(&group, 400.0, 200.0, 100.0);
    railResize();
    let tree = railRender(&rail, props.as_ref());
    assert!(!railFindLabel(&tree, "向左滚动图片").is_undefined());
    assert!(!railFindLabel(&tree, "向右滚动图片").is_undefined());
    assert!(
        railObserved()
            .iter()
            .any(|value| Object::is(&value, &group))
    );
    let current_group = railFindRole(&tree, "group");
    let grown = Array::of3(&railItem("a"), &railItem("b"), &railItem("c"));
    let grown_props = rail_props(grown);
    railSetGeometry(&current_group, 400.0, 200.0, 0.0);
    let tree = railRender(&rail, grown_props.as_ref());
    let grown_group = railFindRole(&tree, "group");
    assert_eq!(property(&grown_group, "scrollLeft").as_f64(), Some(200.0));
    let tree = railRender(&rail, props.as_ref());
    let reduced_group = railFindRole(&tree, "group");
    assert_eq!(property(&reduced_group, "scrollLeft").as_f64(), Some(200.0));
    railUnmount();
}

#[wasm_bindgen_test]
fn vertical_wheel_is_exclusive_normalized_and_reduced_motion_pages_instantly() {
    let (rail, _overlay) = setup();
    let props = rail_props(Array::of2(&railItem("a"), &railItem("b")));
    let tree = railRender(&rail, props.as_ref());
    let group = railFindRole(&tree, "group");
    railSetGeometry(&group, 400.0, 200.0, 0.0);
    assert_eq!(railWheelPassive(&group).as_bool(), Some(false));
    assert!(railWheel(&group, 0.0, 30.0, 0.0));
    assert!(railWheel(&group, 0.0, 500.0, 0.0));
    assert!(railWheel(&group, 0.0, -500.0, 0.0));
    assert!(railWheel(&group, 0.0, 2.0, 1.0));
    assert!(railWheel(&group, 0.0, -1.0, 2.0));
    assert!(railWheel(&group, 12.0, 30.0, 0.0));
    assert!(!railWheel(&group, 12.0, 0.0, 0.0));
    let calls = railScrollCalls(&group);
    assert_eq!(calls.length(), 6);
    for (index, expected) in [30.0, 60.0, -60.0, 32.0, -60.0, 12.0]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            property(&calls.get(u32::try_from(index).unwrap()), "left").as_f64(),
            Some(expected)
        );
    }
    railSetReducedMotion(true);
    railScroll(&group);
    let tree = railRender(&rail, props.as_ref());
    let current_group = railFindRole(&tree, "group");
    railClick(&railFindLabel(&tree, "向右滚动图片"), "onClick");
    let last = railScrollCalls(&current_group).get(0);
    assert_eq!(
        property(&last, "behavior").as_string().as_deref(),
        Some("auto")
    );
    railUnmount();
}
