//! Live WASM coverage for the busy-state Enter preference row.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_conversation::{
    configure_client_ui_conversation_enter_behavior, enter_behavior_row_component,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hooks = []
let cursor = 0
let behavior = 'queue'
let selected = []
let selectedOpen = []
export function installEnterBench() {
  hooks = []; cursor = 0; behavior = 'queue'; selected = []; selectedOpen = []
  globalThis.document = {
    head: { appendChild() {} }, createElement() { return { setAttribute() {} } }, querySelector() { return null },
  }
  const React = {
    createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } },
    useState(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { value: initial }
      const set = update => { hooks[index].value = typeof update === 'function' ? update(hooks[index].value) : update }
      return [hooks[index].value, set]
    },
  }
  return { React, uiPrimitives: { Menu: 'Menu', IconChevronDownOutline14: 'IconChevronDownOutline14' } }
}
export function enterObject(entries) { return Object.fromEntries(entries) }
export function makeUseBusyEnter() { return selector => selector(behavior) }
export function makeSetBusyEnter() { return id => { selected.push(id); selectedOpen.push(hooks[0]?.value); behavior = id } }
export function makeEnterTranslate() {
  const copy = {
    'settings.enter.title': 'Enter behavior while busy',
    'settings.enter.description': 'Busy only; Cmd/Ctrl+Enter uses the other behavior',
    'settings.enter.queue': 'Queue', 'settings.enter.steer': 'Steer',
  }
  return key => copy[key] ?? key
}
export function enterRender(component, props) { cursor = 0; return component(props) }
export function enterSetBehavior(value) { behavior = value }
export function enterSelected() { return selected }
export function enterSelectedOpen() { return selectedOpen }
export function enterFindKind(value, kind) {
  if (value === null || value === undefined || typeof value !== 'object') return undefined
  if (value.kind === kind) return value
  for (const child of value.children ?? []) { const found = enterFindKind(child, kind); if (found) return found }
  return undefined
}
export function enterText(value) {
  if (value === null || value === undefined || typeof value === 'boolean') return ''
  if (typeof value === 'string' || typeof value === 'number') return String(value)
  if (Array.isArray(value)) return value.map(enterText).join('')
  return enterText(value.children)
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = installEnterBench)]
    fn install_enter_bench() -> JsValue;
    #[wasm_bindgen(js_name = enterObject)]
    fn enter_object(entries: &Array) -> JsValue;
    #[wasm_bindgen(js_name = makeUseBusyEnter)]
    fn make_use_busy_enter() -> Function;
    #[wasm_bindgen(js_name = makeSetBusyEnter)]
    fn make_set_busy_enter() -> Function;
    #[wasm_bindgen(js_name = makeEnterTranslate)]
    fn make_enter_translate() -> Function;
    #[wasm_bindgen(js_name = enterRender)]
    fn enter_render(component: &JsValue, props: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = enterSetBehavior)]
    fn enter_set_behavior(value: &str);
    #[wasm_bindgen(js_name = enterSelected)]
    fn enter_selected() -> Array;
    #[wasm_bindgen(js_name = enterSelectedOpen)]
    fn enter_selected_open() -> Array;
    #[wasm_bindgen(js_name = enterFindKind)]
    fn enter_find_kind(value: &JsValue, kind: &str) -> JsValue;
    #[wasm_bindgen(js_name = enterText)]
    fn enter_text(value: &JsValue) -> String;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn object(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    enter_object(&array).unchecked_into()
}

fn setup() -> (JsValue, Object) {
    let bench = install_enter_bench();
    configure_client_ui_conversation_enter_behavior(
        property(&bench, "React"),
        property(&bench, "uiPrimitives"),
    )
    .unwrap();
    let props = object(&[
        ("useBusyEnter", make_use_busy_enter().into()),
        ("setBusyEnter", make_set_busy_enter().into()),
        ("t", make_enter_translate().into()),
    ]);
    (enter_behavior_row_component().unwrap(), props)
}

#[wasm_bindgen_test]
fn default_queue_copy_and_menu_contract_match_the_source() {
    let (component, props) = setup();
    let tree = enter_render(&component, props.as_ref());
    let text = enter_text(&tree);
    assert!(text.contains("Enter behavior while busy"));
    assert!(text.contains("Busy only; Cmd/Ctrl+Enter uses the other behavior"));
    let menu = enter_find_kind(&tree, "Menu");
    let menu_props = property(&menu, "props");
    assert_eq!(property(&menu_props, "open").as_bool(), Some(false));
    assert_eq!(
        property(&menu_props, "selectedId").as_string().as_deref(),
        Some("queue")
    );
    let items = property(&menu_props, "items").unchecked_into::<Array>();
    assert_eq!(
        property(&items.get(0), "id").as_string().as_deref(),
        Some("queue")
    );
    assert_eq!(
        property(&items.get(1), "id").as_string().as_deref(),
        Some("steer")
    );
    assert_eq!(enter_text(&property(&menu_props, "anchor")), "Queue");
    assert_eq!(
        property(&menu_props, "align").as_string().as_deref(),
        Some("end")
    );
    assert_eq!(property(&menu_props, "portal").as_bool(), Some(true));
}

#[wasm_bindgen_test]
fn trigger_toggle_and_menu_close_update_only_local_open_state() {
    let (component, props) = setup();
    let closed = enter_render(&component, props.as_ref());
    let menu = enter_find_kind(&closed, "Menu");
    let anchor = property(&property(&menu, "props"), "anchor");
    property(&property(&anchor, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    let open = enter_render(&component, props.as_ref());
    let menu = enter_find_kind(&open, "Menu");
    assert_eq!(
        property(&property(&menu, "props"), "open").as_bool(),
        Some(true)
    );
    let open_anchor = property(&property(&menu, "props"), "anchor");
    assert_eq!(
        property(&property(&open_anchor, "props"), "aria-expanded").as_bool(),
        Some(true)
    );
    property(&property(&menu, "props"), "onClose")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    let closed = enter_render(&component, props.as_ref());
    assert_eq!(
        property(
            &property(&enter_find_kind(&closed, "Menu"), "props"),
            "open"
        )
        .as_bool(),
        Some(false)
    );
    assert_eq!(enter_selected().length(), 0);
}

#[wasm_bindgen_test]
fn selecting_steer_closes_before_persist_and_external_changes_drive_the_label() {
    let (component, props) = setup();
    let tree = enter_render(&component, props.as_ref());
    let menu = enter_find_kind(&tree, "Menu");
    property(&property(&menu, "props"), "onSelect")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &JsValue::from_str("steer"))
        .unwrap();
    assert_eq!(
        enter_selected().get(0).as_string().as_deref(),
        Some("steer")
    );
    assert_eq!(enter_selected_open().get(0).as_bool(), Some(false));
    let steer = enter_render(&component, props.as_ref());
    let menu = enter_find_kind(&steer, "Menu");
    assert_eq!(
        property(&property(&menu, "props"), "open").as_bool(),
        Some(false)
    );
    assert_eq!(
        enter_text(&property(&property(&menu, "props"), "anchor")),
        "Steer"
    );

    enter_set_behavior("queue");
    let queue = enter_render(&component, props.as_ref());
    let menu = enter_find_kind(&queue, "Menu");
    assert_eq!(
        enter_text(&property(&property(&menu, "props"), "anchor")),
        "Queue"
    );
}
