//! Live JavaScript coverage for compiled modal and disclosure components.

#![cfg(target_arch = "wasm32")]

use std::{cell::Cell, rc::Rc};

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_primitives::{
    configure_client_ui_primitive_atoms, configure_client_ui_primitive_dialogs,
    configure_client_ui_primitive_icons, disclosure_row_component, modal_component,
    risk_confirmation_component,
};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hookSlots = []
let cursor = 0
let cleanups = []
let listeners = new Map()
const body = { kind: 'body' }
const styles = []

export function installDialogBench() {
  hookSlots = []
  cursor = 0
  cleanups = []
  listeners = new Map()
  styles.splice(0)
  globalThis.document = {
    body,
    head: { appendChild(node) { styles.push(node) } },
    createElement(kind) { return { kind, attributes: {}, setAttribute(k, v) { this.attributes[k] = v } } },
    querySelector(selector) {
      const match = selector.match(/data-plugin-css="([^"]+)"/)
      return match === null ? null : styles.find(style => style.attributes['data-plugin-css'] === match[1]) ?? null
    },
    addEventListener(name, listener) {
      let bucket = listeners.get(name)
      if (bucket === undefined) listeners.set(name, bucket = new Set())
      bucket.add(listener)
    },
    removeEventListener(name, listener) { listeners.get(name)?.delete(listener) },
  }
  const React = {
    Fragment: 'Fragment',
    createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } },
    useEffect(effect) {
      const index = cursor++
      if (!(index in hookSlots)) {
        hookSlots[index] = true
        const cleanup = effect()
        if (typeof cleanup === 'function') cleanups.push(cleanup)
      }
    },
  }
  const ReactDOM = { createPortal(node, container) { return { portal: node, container } } }
  return { React, ReactDOM, body, styles }
}
export function dialogRender(component, props) { cursor = 0; return component(props) }
export function dialogFresh() {
  for (const cleanup of cleanups.splice(0).reverse()) cleanup()
  hookSlots = []
  cursor = 0
}
export function dialogUnmount() { for (const cleanup of cleanups.splice(0).reverse()) cleanup() }
export function dialogDispatchKey(key) { for (const listener of listeners.get('keydown') ?? []) listener({ key }) }
export function dialogListenerCount(name) { return listeners.get(name)?.size ?? 0 }
export function dialogBody() { return body }
export function dialogStyles() { return styles }
export function dialogObject(entries) { return Object.fromEntries(entries) }
"#)]
extern "C" {
    fn installDialogBench() -> JsValue;
    fn dialogRender(component: &JsValue, props: &JsValue) -> JsValue;
    fn dialogFresh();
    fn dialogUnmount();
    fn dialogDispatchKey(key: &str);
    fn dialogListenerCount(name: &str) -> u32;
    fn dialogBody() -> JsValue;
    fn dialogStyles() -> Array;
    fn dialogObject(entries: &Array) -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn props(entries: &[(&str, JsValue)]) -> JsValue {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    dialogObject(&array)
}

fn kind(node: &JsValue) -> JsValue {
    property(node, "kind")
}

fn node_props(node: &JsValue) -> JsValue {
    property(node, "props")
}

fn children(node: &JsValue) -> Array {
    Array::from(&property(node, "children"))
}

fn invoke(node: &JsValue, name: &str, argument: &JsValue) {
    property(&node_props(node), name)
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, argument)
        .unwrap();
}

fn configure(bench: &JsValue) {
    let react = property(bench, "React");
    let react_dom = property(bench, "ReactDOM");
    configure_client_ui_primitive_atoms(react.clone(), react_dom.clone()).unwrap();
    configure_client_ui_primitive_icons(react.clone());
    configure_client_ui_primitive_dialogs(react, react_dom).unwrap();
}

#[wasm_bindgen_test]
fn modal_preserves_closed_headless_and_full_portal_lifecycle() {
    let bench = installDialogBench();
    configure(&bench);
    assert_eq!(dialogStyles().length(), 10);
    let calls = Rc::new(Cell::new(0_u32));
    let count = calls.clone();
    let close = Closure::wrap(Box::new(move || count.set(count.get() + 1)) as Box<dyn FnMut()>)
        .into_js_value();
    let component = modal_component().unwrap();
    let closed = dialogRender(
        &component,
        &props(&[
            ("open", JsValue::FALSE),
            ("onClose", close.clone()),
            ("title", JsValue::from_str("Create new workspace")),
        ]),
    );
    assert!(closed.is_null());
    assert_eq!(dialogListenerCount("keydown"), 0);

    dialogFresh();
    let footer = JsValue::from_str("Create");
    let portal = dialogRender(
        &component,
        &props(&[
            ("open", JsValue::TRUE),
            ("onClose", close.clone()),
            ("title", JsValue::from_str("Create new workspace")),
            ("closeLabel", JsValue::from_str("Configure later")),
            ("description", JsValue::from_str("Name it.")),
            ("contentClassName", JsValue::from_str("scrolling-content")),
            ("footer", footer),
            ("children", JsValue::from_str("Name field")),
        ]),
    );
    assert!(Object::is(&property(&portal, "container"), &dialogBody()));
    assert_eq!(dialogListenerCount("keydown"), 1);
    let root = property(&portal, "portal");
    let mask = children(&root).get(0);
    let dialog = children(&root).get(1);
    assert_eq!(
        property(&node_props(&dialog), "role")
            .as_string()
            .as_deref(),
        Some("dialog")
    );
    assert_eq!(
        property(&node_props(&dialog), "aria-label")
            .as_string()
            .as_deref(),
        Some("Create new workspace")
    );
    let content = children(&dialog).get(0);
    assert!(
        property(&node_props(&content), "className")
            .as_string()
            .unwrap()
            .contains("scrolling-content")
    );
    let header = children(&content).get(0);
    let close_button = children(&header).get(1);
    assert_eq!(
        property(&node_props(&close_button), "aria-label")
            .as_string()
            .as_deref(),
        Some("Configure later")
    );
    assert_eq!(children(&dialog).length(), 2);
    invoke(&mask, "onClick", &JsValue::UNDEFINED);
    dialogDispatchKey("Enter");
    dialogDispatchKey("Escape");
    assert_eq!(calls.get(), 2);
    dialogUnmount();
    assert_eq!(dialogListenerCount("keydown"), 0);

    dialogFresh();
    let headless = dialogRender(
        &component,
        &props(&[
            ("open", JsValue::TRUE),
            ("onClose", close),
            ("title", JsValue::from_str("Headless")),
            ("headless", JsValue::TRUE),
            ("children", JsValue::from_str("Owned chrome")),
        ]),
    );
    let dialog = children(&property(&headless, "portal")).get(1);
    assert_eq!(
        children(&dialog).to_vec(),
        [JsValue::from_str("Owned chrome")]
    );
    dialogUnmount();
}

#[wasm_bindgen_test]
fn disclosure_preserves_row_and_leading_interaction_policies() {
    let bench = installDialogBench();
    configure(&bench);
    let calls = Rc::new(Cell::new(0_u32));
    let count = calls.clone();
    let toggle = Closure::wrap(Box::new(move || count.set(count.get() + 1)) as Box<dyn FnMut()>)
        .into_js_value();
    let component = disclosure_row_component().unwrap();
    let row_target = dialogRender(
        &component,
        &props(&[
            ("icon", JsValue::from_str("icon")),
            ("title", JsValue::from_str("Details")),
            ("open", JsValue::FALSE),
            ("expandable", JsValue::TRUE),
            ("expandOnRowClick", JsValue::TRUE),
            ("onToggle", toggle.clone()),
            ("collapsedContent", JsValue::from_str("summary")),
        ]),
    );
    let row = children(&row_target).get(0);
    assert_eq!(
        property(&node_props(&row), "role").as_string().as_deref(),
        Some("button")
    );
    assert_eq!(property(&node_props(&row), "tabIndex").as_f64(), Some(0.0));
    invoke(&row, "onClick", &JsValue::UNDEFINED);
    let prevented = Rc::new(Cell::new(0_u32));
    let prevented_count = prevented.clone();
    let prevent = Closure::wrap(
        Box::new(move || prevented_count.set(prevented_count.get() + 1)) as Box<dyn FnMut()>,
    );
    let key = props(&[
        ("key", JsValue::from_str(" ")),
        ("preventDefault", prevent.into_js_value()),
    ]);
    invoke(&row, "onKeyDown", &key);
    assert_eq!(calls.get(), 2);
    assert_eq!(prevented.get(), 1);

    let leading_target = dialogRender(
        &component,
        &props(&[
            ("icon", JsValue::from_str("icon")),
            ("title", JsValue::from_str("Details")),
            ("open", JsValue::TRUE),
            ("expandable", JsValue::TRUE),
            ("onToggle", toggle),
            ("children", JsValue::from_str("expanded")),
        ]),
    );
    let row = children(&leading_target).get(0);
    let leading = children(&row).get(0);
    assert_eq!(kind(&leading).as_string().as_deref(), Some("button"));
    let stopped = Rc::new(Cell::new(0_u32));
    let stopped_count = stopped.clone();
    let stop = Closure::wrap(
        Box::new(move || stopped_count.set(stopped_count.get() + 1)) as Box<dyn FnMut()>
    );
    invoke(
        &leading,
        "onClick",
        &props(&[("stopPropagation", stop.into_js_value())]),
    );
    assert_eq!(calls.get(), 3);
    assert_eq!(stopped.get(), 1);
    assert_eq!(
        children(&leading_target).get(1).as_string().as_deref(),
        Some("expanded")
    );
    assert_eq!(children(&row).length(), 2);
}

#[wasm_bindgen_test]
fn risk_confirmation_builds_controlled_modal_acknowledgement_and_gated_actions() {
    let bench = installDialogBench();
    configure(&bench);
    let cancel_count = Rc::new(Cell::new(0_u32));
    let cancel_seen = cancel_count.clone();
    let cancel =
        Closure::wrap(Box::new(move || cancel_seen.set(cancel_seen.get() + 1)) as Box<dyn FnMut()>)
            .into_js_value();
    let confirm_count = Rc::new(Cell::new(0_u32));
    let confirm_seen = confirm_count.clone();
    let confirm = Closure::wrap(
        Box::new(move || confirm_seen.set(confirm_seen.get() + 1)) as Box<dyn FnMut()>
    )
    .into_js_value();
    let changed = Rc::new(Cell::new(false));
    let changed_seen = changed.clone();
    let change = Closure::wrap(Box::new(move |value: JsValue| {
        changed_seen.set(value.as_bool().unwrap_or(false));
    }) as Box<dyn FnMut(JsValue)>)
    .into_js_value();
    let component = risk_confirmation_component().unwrap();
    let risk = dialogRender(
        &component,
        &props(&[
            ("open", JsValue::TRUE),
            ("title", JsValue::from_str("Danger")),
            ("description", JsValue::from_str("Understand the risk")),
            ("acknowledgeLabel", JsValue::from_str("I understand")),
            ("cancelLabel", JsValue::from_str("Cancel")),
            ("confirmLabel", JsValue::from_str("Continue")),
            ("acknowledged", JsValue::FALSE),
            ("onAcknowledgedChange", change),
            ("onCancel", cancel.clone()),
            ("onConfirm", confirm.clone()),
        ]),
    );
    assert!(kind(&risk).is_function());
    let modal_props = node_props(&risk);
    assert_eq!(
        property(&modal_props, "title").as_string().as_deref(),
        Some("Danger")
    );
    assert!(Object::is(&property(&modal_props, "onClose"), &cancel));
    let footer = property(&modal_props, "footer");
    assert_eq!(kind(&footer).as_string().as_deref(), Some("Fragment"));
    let confirm_node = children(&footer).get(1);
    assert_eq!(
        property(&node_props(&confirm_node), "disabled").as_bool(),
        Some(true)
    );
    let label = children(&risk).get(1);
    let checkbox = children(&label).get(0);
    assert_eq!(
        property(&node_props(&checkbox), "autoFocus").as_bool(),
        Some(true)
    );
    invoke(
        &checkbox,
        "onChange",
        &props(&[("currentTarget", props(&[("checked", JsValue::TRUE)]))]),
    );
    assert!(changed.get());

    let acknowledged = dialogRender(
        &component,
        &props(&[
            ("open", JsValue::TRUE),
            ("title", JsValue::from_str("Danger")),
            ("description", JsValue::from_str("Understand the risk")),
            ("acknowledgeLabel", JsValue::from_str("I understand")),
            ("cancelLabel", JsValue::from_str("Cancel")),
            ("confirmLabel", JsValue::from_str("Continue")),
            ("acknowledged", JsValue::TRUE),
            (
                "onAcknowledgedChange",
                Closure::wrap(Box::new(|_: JsValue| {}) as Box<dyn FnMut(JsValue)>).into_js_value(),
            ),
            ("onCancel", cancel),
            ("onConfirm", confirm),
        ]),
    );
    let confirm_node = children(&property(&node_props(&acknowledged), "footer")).get(1);
    assert_eq!(
        property(&node_props(&confirm_node), "disabled").as_bool(),
        Some(false)
    );
}
