//! Live WASM coverage for the controlled `Menu` entry, portal, and nested-row behavior.

#![cfg(target_arch = "wasm32")]

use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Object, Reflect};
use seekdeep_client_ui_primitives::{
    POINTER_GRACE_MS, configure_client_ui_primitive_menu, menu_component,
};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hooks = []
let cursor = 0
let pendingLayout = []
let pendingEffects = []
let dirty = false
let attachedRefs = []
let timers = []
let now = 0
let nextTimer = 1
let windowListeners = new Map()
let documentListeners = new Map()
let styles = []
let anchorRect = { left: 100, right: 200, top: 40, bottom: 74, width: 100, height: 34 }
let listWidth = 218
let listHeight = 120

const depsEqual = (left, right) => left !== undefined && right !== undefined && left.length === right.length && left.every((value, index) => Object.is(value, right[index]))
function effectHook(effect, deps, queue) {
  const index = cursor++
  const previous = hooks[index]
  if (previous === undefined || !depsEqual(previous.deps, deps)) {
    queue.push(() => {
      previous?.cleanup?.()
      const cleanup = effect()
      hooks[index] = { deps: [...deps], cleanup: typeof cleanup === 'function' ? cleanup : undefined }
    })
  }
}
function callbackHook(callback, deps) {
  const index = cursor++
  const previous = hooks[index]
  if (previous !== undefined && depsEqual(previous.deps, deps)) return previous.value
  hooks[index] = { deps: [...deps], value: callback }
  return callback
}
function clearRefs() {
  for (const ref of attachedRefs.splice(0)) {
    if (typeof ref === 'function') ref(null)
    else ref.current = null
  }
}
function attachRefs(node) {
  if (!(node instanceof FakeNode)) return
  const ref = node.ref ?? node.props?.ref
  if (typeof ref === 'function') { ref(node); attachedRefs.push(ref) }
  else if (ref !== null && ref !== undefined) { ref.current = node; attachedRefs.push(ref) }
  for (const child of node.children) attachRefs(child)
}
function text(node) {
  if (node === null || node === undefined || node === false) return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  return (node.children ?? []).map(text).join('')
}
function all(node, predicate, output = []) {
  if (node === null || node === undefined || node === false || typeof node === 'string' || typeof node === 'number') return output
  if (predicate(node)) output.push(node)
  for (const child of node.children ?? []) all(child, predicate, output)
  return output
}
class FakeNode {
  constructor(kind, props, children) {
    this.kind = kind
    this.props = props ?? {}
    this.children = children.flat(Infinity).filter(value => value !== null && value !== undefined && value !== false)
    this.ref = props?.ref
    for (const child of this.children) if (child instanceof FakeNode) child.parentElement = this
  }
  contains(target) { return target === this || this.children.some(child => child instanceof FakeNode && child.contains(target)) }
  getBoundingClientRect() {
    if (String(this.props?.className ?? '').split(/\s+/).includes('seekdeep-primitive-menu-root')) return anchorRect
    return { left: 0, right: this.offsetWidth, top: 0, bottom: this.offsetHeight, width: this.offsetWidth, height: this.offsetHeight }
  }
  get offsetWidth() { return String(this.props?.className ?? '').includes('seekdeep-primitive-menu-list') ? listWidth : 0 }
  get offsetHeight() { return String(this.props?.className ?? '').includes('seekdeep-primitive-menu-list') ? listHeight : 0 }
}
function addListener(map, name, listener) {
  let bucket = map.get(name)
  if (bucket === undefined) map.set(name, bucket = new Set())
  bucket.add(listener)
}
function removeListener(map, name, listener) { map.get(name)?.delete(listener) }

export function installMenuBench() {
  for (const hook of hooks) hook?.cleanup?.()
  hooks = []
  cursor = 0
  pendingLayout = []
  pendingEffects = []
  dirty = false
  attachedRefs = []
  timers = []
  now = 0
  nextTimer = 1
  windowListeners = new Map()
  documentListeners = new Map()
  styles = []
  anchorRect = { left: 100, right: 200, top: 40, bottom: 74, width: 100, height: 34 }
  listWidth = 218
  listHeight = 120
  globalThis.Node = FakeNode
  globalThis.window = globalThis
  globalThis.innerWidth = 1024
  globalThis.innerHeight = 768
  globalThis.setTimeout = (callback, delay) => {
    const timer = { id: nextTimer++, callback, at: now + Number(delay), active: true }
    timers.push(timer)
    return timer.id
  }
  globalThis.clearTimeout = id => {
    const timer = timers.find(candidate => candidate.id === id)
    if (timer !== undefined) timer.active = false
  }
  globalThis.addEventListener = (name, listener) => addListener(windowListeners, name, listener)
  globalThis.removeEventListener = (name, listener) => removeListener(windowListeners, name, listener)
  const body = new FakeNode('body', {}, [])
  globalThis.document = {
    body,
    head: { appendChild(node) { styles.push(node) } },
    createElement(kind) { return { kind, attributes: {}, textContent: '', setAttribute(key, value) { this.attributes[key] = value } } },
    querySelector(selector) {
      const match = selector.match(/data-plugin-css="([^"]+)"/)
      return match === null ? null : styles.find(style => style.attributes['data-plugin-css'] === match[1]) ?? null
    },
    addEventListener(name, listener) { addListener(documentListeners, name, listener) },
    removeEventListener(name, listener) { removeListener(documentListeners, name, listener) },
  }
  const React = {
    createElement(kind, props, ...children) { return new FakeNode(kind, props, children) },
    useState(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { value: initial }
      return [hooks[index].value, update => {
        const value = typeof update === 'function' ? update(hooks[index].value) : update
        if (!Object.is(value, hooks[index].value)) { hooks[index].value = value; dirty = true }
      }]
    },
    useRef(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { current: initial }
      return hooks[index]
    },
    useCallback: callbackHook,
    useLayoutEffect(effect, deps) { effectHook(effect, deps, pendingLayout) },
    useEffect(effect, deps) { effectHook(effect, deps, pendingEffects) },
  }
  const ReactDOM = { createPortal(child, container) { return new FakeNode('Portal', { container }, [child]) } }
  return { React, ReactDOM, body, styles }
}

export function menuObject(entries) { return Object.fromEntries(entries) }
export function menuAnchor() { return new FakeNode('span', {}, ['trigger']) }
export function menuRender(component, props) {
  let tree
  for (let attempt = 0; attempt < 12; attempt++) {
    clearRefs()
    cursor = 0
    pendingLayout = []
    pendingEffects = []
    dirty = false
    tree = component(props)
    attachRefs(tree)
    for (const run of pendingLayout) run()
    for (const run of pendingEffects) run()
    if (!dirty) return tree
  }
  throw new Error('Menu hook runtime did not settle')
}
export function menuUnmount() {
  clearRefs()
  for (const hook of hooks) hook?.cleanup?.()
  hooks = []
}
export function menuAdvance(milliseconds) {
  const target = now + milliseconds
  while (true) {
    const timer = timers.filter(candidate => candidate.active && candidate.at <= target).sort((left, right) => left.at - right.at || left.id - right.id)[0]
    if (timer === undefined) break
    now = timer.at
    timer.active = false
    timer.callback()
  }
  now = target
}
export function menuFindRole(tree, role, label) { return all(tree, node => node.props?.role === role && (label === undefined || text(node) === label))[0] }
export function menuAllRole(tree, role) { return all(tree, node => node.props?.role === role) }
export function menuFindClass(tree, className) { return all(tree, node => String(node.props?.className ?? '').split(/\s+/).includes(className))[0] }
export function menuWithinClass(tree, className, role, label) { const root = menuFindClass(tree, className); return root === undefined ? undefined : menuFindRole(root, role, label) }
export function menuText(tree) { return text(tree) }
export function menuClick(node) { if (node.props?.disabled) return; const event = { stopped: false, stopPropagation() { this.stopped = true } }; node.props?.onClick?.(event); return event.stopped }
export function menuFocus(node) { node.props?.onFocus?.() }
export function menuMouseEnter(node) { node.props?.onMouseEnter?.() }
export function menuMouseLeave(node) { node.props?.onMouseLeave?.() }
export function menuPointerEnter(tree) { tree.props?.onPointerEnter?.() }
export function menuPointerLeave(tree) { tree.props?.onPointerLeave?.() }
export function menuDispatchDocument(name, key, target) { for (const listener of documentListeners.get(name) ?? []) listener({ key, target }) }
export function menuDispatchWindow(name) { for (const listener of windowListeners.get(name) ?? []) listener() }
export function menuListenerCount(owner, name) { return (owner === 'document' ? documentListeners : windowListeners).get(name)?.size ?? 0 }
export function menuSetAnchorRect(left, right, top, bottom) { anchorRect = { left, right, top, bottom, width: right - left, height: bottom - top } }
export function menuSetListSize(width, height) { listWidth = width; listHeight = height }
export function menuSetViewport(width, height) { globalThis.innerWidth = width; globalThis.innerHeight = height }
export function menuBody() { return document.body }
export function menuWindow() { return globalThis }
export function menuStyles() { return styles }
export function menuHasKind(node, kind) { return all(node, candidate => candidate.kind === kind).length > 0 }
export function menuPortalContainer(tree) { return menuFindClass(tree, 'seekdeep-primitive-menu-list')?.parentElement?.props?.container }
export function menuActiveTimers() { return timers.filter(timer => timer.active).length }
"#)]
extern "C" {
    fn installMenuBench() -> JsValue;
    fn menuObject(entries: &Array) -> JsValue;
    fn menuAnchor() -> JsValue;
    fn menuRender(component: &JsValue, props: &JsValue) -> JsValue;
    fn menuUnmount();
    fn menuAdvance(milliseconds: f64);
    fn menuFindRole(tree: &JsValue, role: &str, label: &JsValue) -> JsValue;
    fn menuAllRole(tree: &JsValue, role: &str) -> Array;
    fn menuFindClass(tree: &JsValue, class_name: &str) -> JsValue;
    fn menuWithinClass(tree: &JsValue, class_name: &str, role: &str, label: &str) -> JsValue;
    fn menuText(tree: &JsValue) -> String;
    fn menuClick(node: &JsValue) -> bool;
    fn menuFocus(node: &JsValue);
    fn menuMouseEnter(node: &JsValue);
    fn menuMouseLeave(node: &JsValue);
    fn menuPointerEnter(tree: &JsValue);
    fn menuPointerLeave(tree: &JsValue);
    fn menuDispatchDocument(name: &str, key: &str, target: &JsValue);
    fn menuDispatchWindow(name: &str);
    fn menuListenerCount(owner: &str, name: &str) -> u32;
    fn menuSetAnchorRect(left: f64, right: f64, top: f64, bottom: f64);
    fn menuSetListSize(width: f64, height: f64);
    fn menuSetViewport(width: f64, height: f64);
    fn menuBody() -> JsValue;
    fn menuWindow() -> JsValue;
    fn menuStyles() -> Array;
    fn menuHasKind(node: &JsValue, kind: &str) -> bool;
    fn menuPortalContainer(tree: &JsValue) -> JsValue;
    fn menuActiveTimers() -> u32;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn object(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    menuObject(&array).unchecked_into()
}

fn item(id: &str, label: &str) -> JsValue {
    object(&[
        ("id", JsValue::from_str(id)),
        ("label", JsValue::from_str(label)),
    ])
    .into()
}

fn items(values: &[JsValue]) -> Array {
    values.iter().collect()
}

struct Callbacks {
    selected: Rc<RefCell<Vec<String>>>,
    closes: Rc<RefCell<u32>>,
    on_select: JsValue,
    on_close: JsValue,
}

fn callbacks() -> Callbacks {
    let selected = Rc::new(RefCell::new(Vec::new()));
    let selected_calls = selected.clone();
    let on_select = Closure::wrap(Box::new(move |id: String| {
        selected_calls.borrow_mut().push(id);
    }) as Box<dyn FnMut(String)>)
    .into_js_value();
    let closes = Rc::new(RefCell::new(0));
    let close_calls = closes.clone();
    let on_close = Closure::wrap(Box::new(move || {
        *close_calls.borrow_mut() += 1;
    }) as Box<dyn FnMut()>)
    .into_js_value();
    Callbacks {
        selected,
        closes,
        on_select,
        on_close,
    }
}

fn base_props(open: bool, entries: &Array, callbacks: &Callbacks) -> Object {
    object(&[
        ("open", JsValue::from_bool(open)),
        ("anchor", menuAnchor()),
        ("items", entries.clone().into()),
        ("onSelect", callbacks.on_select.clone()),
        ("onClose", callbacks.on_close.clone()),
    ])
}

fn set(props: &Object, key: &str, value: &JsValue) {
    Reflect::set(props, &JsValue::from_str(key), value).unwrap();
}

fn setup() -> (JsValue, JsValue) {
    let bench = installMenuBench();
    configure_client_ui_primitive_menu(property(&bench, "React"), property(&bench, "ReactDOM"))
        .unwrap();
    (bench, menu_component().unwrap())
}

fn render(component: &JsValue, props: &Object) -> JsValue {
    menuRender(component, props.as_ref())
}

fn role(tree: &JsValue, name: &str, label: Option<&str>) -> JsValue {
    menuFindRole(
        tree,
        name,
        &label.map_or(JsValue::UNDEFINED, JsValue::from_str),
    )
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)]
fn controlled_rows_selection_classes_and_footer_match_source() {
    let (bench, component) = setup();
    assert_eq!(menuStyles().length(), 1);
    configure_client_ui_primitive_menu(property(&bench, "React"), property(&bench, "ReactDOM"))
        .unwrap();
    assert_eq!(menuStyles().length(), 1);
    let stylesheet = property(&menuStyles().get(0), "textContent")
        .as_string()
        .unwrap();
    for class_name in [
        "seekdeep-primitive-menu-list",
        "seekdeep-primitive-menu-itemWrap",
        "seekdeep-primitive-menu-submenu",
    ] {
        assert!(stylesheet.contains(class_name), "{class_name}");
    }
    let calls = callbacks();
    let beta = object(&[
        ("id", JsValue::from_str("b")),
        ("label", JsValue::from_str("Beta")),
        ("disabled", JsValue::TRUE),
    ]);
    let entries = items(&[item("a", "Alpha"), beta.into()]);
    let closed = base_props(false, &entries, &calls);
    assert!(role(&render(&component, &closed), "menu", None).is_undefined());

    let open = base_props(true, &entries, &calls);
    set(&open, "selectedId", &JsValue::from_str("a"));
    set(&open, "align", &JsValue::from_str("end"));
    set(&open, "side", &JsValue::from_str("top"));
    set(&open, "className", &JsValue::from_str("caller"));
    let tree = render(&component, &open);
    assert!(
        property(&tree, "props")
            .pipe(|props| property(&props, "className"))
            .as_string()
            .unwrap()
            .contains("caller")
    );
    let menu = role(&tree, "menu", None);
    let classes = property(&property(&menu, "props"), "className")
        .as_string()
        .unwrap();
    assert!(classes.contains("sideTop"));
    assert!(classes.contains("alignEnd"));
    let alpha = role(&tree, "menuitem", Some("Alpha"));
    assert!(menuHasKind(&alpha, "svg"));
    menuClick(&alpha);
    assert_eq!(calls.selected.borrow().as_slice(), ["a"]);
    let beta = role(&tree, "menuitem", Some("Beta"));
    assert!(!menuHasKind(&beta, "svg"));
    menuClick(&beta);
    assert_eq!(calls.selected.borrow().as_slice(), ["a"]);

    let icon = JsValue::from_str("leading-icon");
    let icon_item = object(&[
        ("id", JsValue::from_str("i")),
        ("label", JsValue::from_str("Icon")),
        ("icon", icon),
    ]);
    let separator = object(&[
        ("type", JsValue::from_str("separator")),
        ("id", JsValue::from_str("sep")),
    ]);
    let label = object(&[
        ("type", JsValue::from_str("label")),
        ("id", JsValue::from_str("heading")),
        ("text", JsValue::from_str("Group by")),
    ]);
    let danger = object(&[
        ("id", JsValue::from_str("del")),
        ("label", JsValue::from_str("Delete")),
        ("danger", JsValue::TRUE),
    ]);
    let mixed = items(&[
        icon_item.into(),
        separator.into(),
        label.into(),
        danger.into(),
    ]);
    let mixed_props = base_props(true, &mixed, &calls);
    set(&mixed_props, "compact", &JsValue::TRUE);
    set(&mixed_props, "dense", &JsValue::TRUE);
    set(
        &mixed_props,
        "selectedIds",
        &Array::of1(&JsValue::from_str("i")).into(),
    );
    let mixed_tree = render(&component, &mixed_props);
    let mixed_menu = role(&mixed_tree, "menu", None);
    let mixed_classes = property(&property(&mixed_menu, "props"), "className")
        .as_string()
        .unwrap();
    assert!(mixed_classes.contains("compactList"));
    assert!(mixed_classes.contains("denseList"));
    assert!(!menuFindClass(&mixed_tree, "seekdeep-primitive-menu-itemIcon").is_undefined());
    let heading = menuFindClass(&mixed_tree, "seekdeep-primitive-menu-label");
    assert_eq!(
        property(&property(&heading, "props"), "role")
            .as_string()
            .as_deref(),
        Some("presentation")
    );
    assert_eq!(menuAllRole(&mixed_tree, "separator").length(), 1);
    assert_eq!(menuAllRole(&mixed_tree, "menuitem").length(), 2);
    assert!(menuText(&mixed_tree).contains("Group by"));
    let danger = role(&mixed_tree, "menuitem", Some("Delete"));
    assert!(
        property(&property(&danger, "props"), "className")
            .as_string()
            .unwrap()
            .contains("danger")
    );
    menuClick(&danger);
    assert_eq!(
        calls.selected.borrow().last().map(String::as_str),
        Some("del")
    );

    let footer = Array::of1(&item("new", "Create new"));
    let footer_props = base_props(true, &entries, &calls);
    set(&footer_props, "footer", &footer.into());
    let footer_tree = render(&component, &footer_props);
    let footer_item = menuWithinClass(
        &footer_tree,
        "seekdeep-primitive-menu-footer",
        "menuitem",
        "Create new",
    );
    assert!(!footer_item.is_undefined());
    menuClick(&footer_item);
    assert_eq!(
        calls.selected.borrow().last().map(String::as_str),
        Some("new")
    );
    assert!(
        property(
            &property(&role(&footer_tree, "menu", None), "props"),
            "className"
        )
        .as_string()
        .unwrap()
        .contains("scrollable")
    );

    let submenu = Array::of1(&item("child", "Child"));
    let parent = object(&[
        ("id", JsValue::from_str("parent")),
        ("label", JsValue::from_str("Parent")),
        ("submenu", submenu.into()),
    ]);
    let submenu_props = base_props(true, &items(&[parent.into()]), &calls);
    assert!(
        !property(
            &property(
                &role(&render(&component, &submenu_props), "menu", None),
                "props"
            ),
            "className"
        )
        .as_string()
        .unwrap()
        .contains("scrollable")
    );
    menuUnmount();
}

#[wasm_bindgen_test]
fn document_dismissal_inside_filter_and_bubble_stop_match_source() {
    let (_bench, component) = setup();
    let calls = callbacks();
    let props = base_props(true, &items(&[item("a", "Alpha")]), &calls);
    let tree = render(&component, &props);
    let alpha = role(&tree, "menuitem", Some("Alpha"));
    menuDispatchDocument("pointerdown", "", &alpha);
    assert_eq!(*calls.closes.borrow(), 0);
    menuDispatchDocument("pointerdown", "", &menuWindow());
    assert_eq!(*calls.closes.borrow(), 0);
    menuDispatchDocument("keydown", "a", &menuBody());
    assert_eq!(*calls.closes.borrow(), 0);
    menuDispatchDocument("keydown", "Escape", &menuBody());
    assert_eq!(*calls.closes.borrow(), 1);
    menuDispatchDocument("pointerdown", "", &menuBody());
    assert_eq!(*calls.closes.borrow(), 2);
    assert_eq!(menuListenerCount("document", "pointerdown"), 1);
    assert!(menuClick(&role(&tree, "menu", None)));
    menuUnmount();
    assert_eq!(menuListenerCount("document", "pointerdown"), 0);
}

#[wasm_bindgen_test]
fn pointer_grace_default_return_disarm_and_closed_paths_match_source() {
    let (_bench, component) = setup();
    let calls = callbacks();
    let entries = items(&[item("a", "Alpha")]);
    let props = base_props(true, &entries, &calls);
    set(&props, "closeOnPointerLeave", &JsValue::TRUE);
    let tree = render(&component, &props);
    menuPointerLeave(&tree);
    menuAdvance(f64::from(POINTER_GRACE_MS - 1));
    assert_eq!(*calls.closes.borrow(), 0);
    menuAdvance(1.0);
    assert_eq!(*calls.closes.borrow(), 1);

    let calls = callbacks();
    let props = base_props(true, &entries, &calls);
    set(&props, "closeOnPointerLeave", &JsValue::TRUE);
    let tree = render(&component, &props);
    menuPointerLeave(&tree);
    menuAdvance(f64::from(POINTER_GRACE_MS - 50));
    menuPointerEnter(&tree);
    menuAdvance(f64::from(POINTER_GRACE_MS * 10));
    assert_eq!(*calls.closes.borrow(), 0);

    let closed = base_props(false, &entries, &calls);
    set(&closed, "closeOnPointerLeave", &JsValue::TRUE);
    let closed_tree = render(&component, &closed);
    assert_eq!(menuActiveTimers(), 0);
    menuPointerLeave(&closed_tree);
    menuAdvance(f64::from(POINTER_GRACE_MS * 10));
    assert_eq!(*calls.closes.borrow(), 0);

    let default_calls = callbacks();
    let default_props = base_props(true, &entries, &default_calls);
    let default_tree = render(&component, &default_props);
    menuPointerLeave(&default_tree);
    menuAdvance(f64::from(POINTER_GRACE_MS * 10));
    assert_eq!(*default_calls.closes.borrow(), 0);

    let disarm_calls = callbacks();
    let open = base_props(true, &entries, &disarm_calls);
    set(&open, "closeOnPointerLeave", &JsValue::TRUE);
    let tree = render(&component, &open);
    menuPointerLeave(&tree);
    let closed = base_props(false, &entries, &disarm_calls);
    set(&closed, "closeOnPointerLeave", &JsValue::TRUE);
    render(&component, &closed);
    menuAdvance(f64::from(POINTER_GRACE_MS * 10));
    assert_eq!(*disarm_calls.closes.borrow(), 0);
    menuUnmount();
}

#[wasm_bindgen_test]
fn submenu_hover_focus_parent_and_nested_selection_match_source() {
    let (_bench, component) = setup();
    let calls = callbacks();
    let child = object(&[
        ("id", JsValue::from_str("child")),
        ("label", JsValue::from_str("Create ok")),
        (
            "icon",
            object(&[
                ("kind", JsValue::from_str("svg")),
                ("children", Array::new().into()),
            ])
            .into(),
        ),
    ]);
    let parent = object(&[
        ("id", JsValue::from_str("parent")),
        ("label", JsValue::from_str("New Workspace")),
        ("submenu", Array::of1(&child).into()),
    ]);
    let props = base_props(
        true,
        &items(&[item("plain", "Plain"), parent.into()]),
        &calls,
    );
    set(&props, "compact", &JsValue::TRUE);
    let tree = render(&component, &props);
    let plain = role(&tree, "menuitem", Some("Plain"));
    let plain_wrap = property(&plain, "parentElement");
    menuMouseEnter(&plain_wrap);
    menuFocus(&plain);
    let parent = role(
        &render(&component, &props),
        "menuitem",
        Some("New Workspace"),
    );
    menuClick(&parent);
    assert!(calls.selected.borrow().is_empty());
    let tree = render(&component, &props);
    let parent = role(&tree, "menuitem", Some("New Workspace"));
    assert_eq!(
        property(&property(&parent, "props"), "aria-haspopup")
            .as_string()
            .as_deref(),
        Some("menu")
    );
    assert_eq!(
        property(&property(&parent, "props"), "aria-expanded").as_bool(),
        Some(true)
    );
    let child = role(&tree, "menuitem", Some("Create ok"));
    assert!(!child.is_undefined());
    menuClick(&child);
    assert_eq!(calls.selected.borrow().as_slice(), ["child"]);
    let parent_wrap = property(&parent, "parentElement");
    menuMouseLeave(&parent_wrap);
    assert!(role(&render(&component, &props), "menuitem", Some("Create ok")).is_undefined());
    menuUnmount();
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)]
fn portal_measurement_null_frame_clamping_reflow_and_cleanup_match_source() {
    let (bench, component) = setup();
    let calls = callbacks();
    let entries = items(&[item("a", "Alpha")]);
    let supplied = Closure::wrap(Box::new(move || {
        object(&[
            ("left", JsValue::from_f64(40.0)),
            ("right", JsValue::from_f64(72.0)),
            ("top", JsValue::from_f64(100.0)),
            ("bottom", JsValue::from_f64(128.0)),
        ])
    }) as Box<dyn FnMut() -> Object>)
    .into_js_value();
    let props = base_props(true, &entries, &calls);
    set(&props, "portal", &JsValue::TRUE);
    set(&props, "anchor", &JsValue::NULL);
    set(&props, "getAnchorRect", &supplied);
    let tree = render(&component, &props);
    let menu = role(&tree, "menu", None);
    let style = property(&property(&menu, "props"), "style");
    assert_eq!(property(&style, "left").as_f64(), Some(40.0));
    assert_eq!(property(&style, "top").as_f64(), Some(132.0));
    assert!(Object::is(
        &menuPortalContainer(&tree),
        &property(&bench, "body")
    ));
    assert!(
        property(&property(&menu, "props"), "className")
            .as_string()
            .unwrap()
            .contains("portal")
    );
    menuClick(&role(&tree, "menuitem", Some("Alpha")));
    assert_eq!(calls.selected.borrow().as_slice(), ["a"]);
    menuDispatchDocument("pointerdown", "", &menu);
    assert_eq!(*calls.closes.borrow(), 0);
    menuDispatchDocument("pointerdown", "", &menuWindow());
    assert_eq!(*calls.closes.borrow(), 0);
    menuDispatchDocument("pointerdown", "", &menuBody());
    assert_eq!(*calls.closes.borrow(), 1);
    assert_eq!(menuListenerCount("window", "scroll"), 1);
    assert_eq!(menuListenerCount("window", "resize"), 1);
    menuUnmount();

    let (_bench, component) = setup();
    let calls = callbacks();
    let entries = items(&[item("a", "Alpha")]);
    let null_rect = Closure::wrap(Box::new(move || JsValue::NULL) as Box<dyn FnMut() -> JsValue>)
        .into_js_value();
    let null_props = base_props(true, &entries, &calls);
    set(&null_props, "portal", &JsValue::TRUE);
    set(&null_props, "anchor", &JsValue::NULL);
    set(&null_props, "getAnchorRect", &null_rect);
    let hidden = render(&component, &null_props);
    assert_eq!(
        property(
            &property(&property(&role(&hidden, "menu", None), "props"), "style"),
            "visibility"
        )
        .as_string()
        .as_deref(),
        Some("hidden")
    );
    menuUnmount();

    let (_bench, component) = setup();
    let calls = callbacks();
    let entries = items(&[item("a", "Alpha")]);
    menuSetViewport(300.0, 240.0);
    menuSetListSize(100.0, 50.0);
    let clamped = base_props(true, &entries, &calls);
    set(&clamped, "portal", &JsValue::TRUE);
    set(&clamped, "align", &JsValue::from_str("end"));
    set(&clamped, "side", &JsValue::from_str("top"));
    set(&clamped, "getAnchorRect", &supplied);
    let tree = render(&component, &clamped);
    let style = property(&property(&role(&tree, "menu", None), "props"), "style");
    assert_eq!(property(&style, "left").as_f64(), Some(12.0));
    assert_eq!(property(&style, "top").as_f64(), Some(46.0));
    menuUnmount();

    let (_bench, component) = setup();
    let calls = callbacks();
    let entries = items(&[item("a", "Alpha")]);
    menuSetViewport(300.0, 240.0);
    menuSetListSize(100.0, 50.0);
    menuSetAnchorRect(260.0, 290.0, 200.0, 228.0);
    let measured = base_props(true, &entries, &calls);
    set(&measured, "portal", &JsValue::TRUE);
    let tree = render(&component, &measured);
    let before = property(&property(&role(&tree, "menu", None), "props"), "style");
    assert_eq!(property(&before, "left").as_f64(), Some(188.0));
    menuSetAnchorRect(20.0, 50.0, 30.0, 58.0);
    menuDispatchWindow("scroll");
    let tree = render(&component, &measured);
    let after = property(&property(&role(&tree, "menu", None), "props"), "style");
    assert_eq!(property(&after, "left").as_f64(), Some(20.0));
    assert_eq!(property(&after, "top").as_f64(), Some(62.0));

    let closed = base_props(false, &entries, &calls);
    set(&closed, "portal", &JsValue::TRUE);
    render(&component, &closed);
    assert_eq!(menuListenerCount("window", "scroll"), 0);
    assert_eq!(menuListenerCount("window", "resize"), 0);
    menuUnmount();
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}

impl<T> Pipe for T {}
