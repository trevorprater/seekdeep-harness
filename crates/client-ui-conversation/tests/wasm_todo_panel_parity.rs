//! Live WASM coverage for Todo panel, dock, glyphs, and entry metadata.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_conversation::{
    configure_client_ui_conversation_todo_panel, todo_dock_component, todo_dock_entry_browser,
    todo_panel_component, todo_status_components,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hooks = []
let cursor = 0
let nextId = 1
let projection
let injectCalls = []
let registerCalls = []
export function installTodoBench() {
  hooks = []; cursor = 0; nextId = 1; projection = undefined; injectCalls = []; registerCalls = []
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
    useId() { return `:todo:${nextId++}:` },
  }
  const uiPrimitives = {
    IconChecklistOutline14: 'IconChecklistOutline14',
    IconChevronDownOutline14: 'IconChevronDownOutline14', IconChevronUpOutline14: 'IconChevronUpOutline14',
  }
  return { React, uiPrimitives }
}
export function todoResetHooks() { hooks = []; cursor = 0 }
export function todoObject(entries) { return Object.fromEntries(entries) }
export function todoRender(component, props) { cursor = 0; return component(props) }
export function todoSetProjection(value) { projection = value }
export function makeTodoProjection() { return key => key === 'todos' ? projection : undefined }
export function makeTodoTranslate() {
  const copy = { 'todo.title': '任务' }
  return (key, vars) => {
    if (key === 'todo.progress.done') return `${vars.done} 已完成`
    if (key === 'todo.progress.active') return `${vars.active} 进行中`
    if (key === 'todo.progress.pending') return `${vars.pending} 待处理`
    return copy[key] ?? key
  }
}
export function todoText(value) {
  if (value === null || value === undefined || typeof value === 'boolean') return ''
  if (typeof value === 'string' || typeof value === 'number') return String(value)
  if (Array.isArray(value)) return value.map(todoText).join('')
  return todoText(value.children)
}
export function todoFindKind(value, kind) {
  if (value === null || value === undefined || typeof value !== 'object') return undefined
  if (value.kind === kind) return value
  for (const child of value.children ?? []) { const found = todoFindKind(child, kind); if (found) return found }
  return undefined
}
export function todoFindAllKind(value, kind) {
  if (value === null || value === undefined || typeof value !== 'object') return []
  const own = value.kind === kind ? [value] : []
  return own.concat(...(value.children ?? []).map(child => todoFindAllKind(child, kind)))
}
export function todoContext() {
  const slots = {
    inject(name, callback) { injectCalls.push({ name, callback }); return callback() },
    register(options, component) { registerCalls.push({ options, component }); return () => {} },
  }
  return { slots }
}
export function todoInjectCalls() { return injectCalls }
export function todoRegisterCalls() { return registerCalls }
"#)]
extern "C" {
    #[wasm_bindgen(js_name = installTodoBench)]
    fn install_todo_bench() -> JsValue;
    #[wasm_bindgen(js_name = todoResetHooks)]
    fn todo_reset_hooks();
    #[wasm_bindgen(js_name = todoObject)]
    fn todo_object(entries: &Array) -> JsValue;
    #[wasm_bindgen(js_name = todoRender)]
    fn todo_render(component: &JsValue, props: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = todoSetProjection)]
    fn todo_set_projection(value: &JsValue);
    #[wasm_bindgen(js_name = makeTodoProjection)]
    fn make_todo_projection() -> Function;
    #[wasm_bindgen(js_name = makeTodoTranslate)]
    fn make_todo_translate() -> Function;
    #[wasm_bindgen(js_name = todoText)]
    fn todo_text(value: &JsValue) -> String;
    #[wasm_bindgen(js_name = todoFindKind)]
    fn todo_find_kind(value: &JsValue, kind: &str) -> JsValue;
    #[wasm_bindgen(js_name = todoFindAllKind)]
    fn todo_find_all_kind(value: &JsValue, kind: &str) -> Array;
    #[wasm_bindgen(js_name = todoContext)]
    fn todo_context() -> JsValue;
    #[wasm_bindgen(js_name = todoInjectCalls)]
    fn todo_inject_calls() -> Array;
    #[wasm_bindgen(js_name = todoRegisterCalls)]
    fn todo_register_calls() -> Array;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn child(value: &JsValue, index: u32) -> JsValue {
    property(value, "children")
        .unchecked_into::<Array>()
        .get(index)
}

fn object(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    todo_object(&array).unchecked_into()
}

fn item(content: &str, status: &str) -> Object {
    object(&[
        ("content", JsValue::from_str(content)),
        ("status", JsValue::from_str(status)),
    ])
}

fn list() -> Array {
    Array::of3(
        item("搭骨架", "completed").as_ref(),
        item("写组件", "in_progress").as_ref(),
        item("补测试", "pending").as_ref(),
    )
}

fn setup() -> (JsValue, JsValue, Array, JsValue) {
    let bench = install_todo_bench();
    configure_client_ui_conversation_todo_panel(
        property(&bench, "React"),
        property(&bench, "uiPrimitives"),
    )
    .unwrap();
    (
        todo_panel_component().unwrap(),
        todo_dock_component().unwrap(),
        todo_status_components().unwrap(),
        todo_dock_entry_browser().unwrap(),
    )
}

fn panel_props(todos: &Array) -> Object {
    object(&[
        ("todos", todos.clone().into()),
        ("t", make_todo_translate().into()),
    ])
}

#[wasm_bindgen_test]
fn empty_list_is_hidden_after_the_collapsed_hook_is_allocated() {
    let (panel, _, _, _) = setup();
    assert!(todo_render(&panel, panel_props(&Array::new()).as_ref()).is_null());
}

#[wasm_bindgen_test]
fn collapsed_summary_omits_zero_counts_and_toggles_the_exact_chevron() {
    let (panel, _, _, _) = setup();
    let todos = list();
    let tree = todo_render(&panel, panel_props(&todos).as_ref());
    assert!(
        todo_text(&tree).contains("1 已完成\u{2002}·\u{2002}1 进行中\u{2002}·\u{2002}1 待处理")
    );
    let button = todo_find_kind(&tree, "button");
    assert_eq!(
        property(&property(&button, "props"), "aria-expanded").as_bool(),
        Some(false)
    );
    assert!(!todo_find_kind(&tree, "IconChevronUpOutline14").is_undefined());

    todo_reset_hooks();
    let no_done = Array::of2(
        item("写组件", "in_progress").as_ref(),
        item("补测试", "pending").as_ref(),
    );
    let tree = todo_render(&panel, panel_props(&no_done).as_ref());
    assert!(todo_text(&tree).contains("1 进行中\u{2002}·\u{2002}1 待处理"));
    assert!(!todo_text(&tree).contains("已完成"));
}

#[wasm_bindgen_test]
fn expanded_rows_preserve_order_status_and_status_component_identity() {
    let (panel, _, status_components, _) = setup();
    let todos = list();
    let closed = todo_render(&panel, panel_props(&todos).as_ref());
    let button = todo_find_kind(&closed, "button");
    property(&property(&button, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    let open = todo_render(&panel, panel_props(&todos).as_ref());
    let rows = todo_find_all_kind(&open, "li");
    assert_eq!(rows.length(), 3);
    for (index, status) in [
        (0_u32, "completed"),
        (1_u32, "in_progress"),
        (2_u32, "pending"),
    ] {
        assert_eq!(
            property(&property(&rows.get(index), "props"), "data-status")
                .as_string()
                .as_deref(),
            Some(status)
        );
        let glyph = todo_find_kind(&rows.get(index), "span");
        let status_element = child(&glyph, 0);
        assert!(Object::is(
            &property(&status_element, "kind"),
            &status_components.get(0)
        ));
    }
    assert!(!todo_find_kind(&open, "IconChevronDownOutline14").is_undefined());
}

#[wasm_bindgen_test]
fn status_glyphs_pin_svg_shapes_gradient_id_and_unknown_failure() {
    let (_, _, components, _) = setup();
    let status = components.get(0);
    for (value, component_index) in [("completed", 1), ("in_progress", 2), ("pending", 3)] {
        let routed = todo_render(
            &status,
            object(&[("status", JsValue::from_str(value))]).as_ref(),
        );
        assert!(Object::is(
            &property(&routed, "kind"),
            &components.get(component_index)
        ));
    }
    let progress = todo_render(&components.get(2), Object::new().as_ref());
    let gradient = todo_find_kind(&progress, "linearGradient");
    let id = property(&property(&gradient, "props"), "id")
        .as_string()
        .unwrap();
    let circle = todo_find_kind(&progress, "circle");
    assert_eq!(
        property(&property(&circle, "props"), "stroke")
            .as_string()
            .as_deref(),
        Some(format!("url(#{id})").as_str())
    );
    let pending = todo_render(&components.get(3), Object::new().as_ref());
    assert_eq!(
        property(
            &property(&todo_find_kind(&pending, "circle"), "props"),
            "strokeDasharray"
        )
        .as_string()
        .as_deref(),
        Some("2.4 2.4")
    );
    assert!(
        Reflect::apply(
            &status.dyn_into::<Function>().unwrap(),
            &JsValue::UNDEFINED,
            &Array::of1(object(&[("status", JsValue::from_str("forged"))]).as_ref())
        )
        .is_err()
    );
}

#[wasm_bindgen_test]
fn dock_projection_and_registration_entry_preserve_nullish_and_metadata_contracts() {
    let (_, dock, _, entry) = setup();
    todo_set_projection(&JsValue::UNDEFINED);
    let props = object(&[
        ("useProjection", make_todo_projection().into()),
        ("t", make_todo_translate().into()),
    ]);
    let empty = todo_render(&dock, props.as_ref());
    assert_eq!(
        property(&property(&empty, "props"), "todos")
            .unchecked_into::<Array>()
            .length(),
        0
    );
    let todos = list();
    todo_set_projection(todos.as_ref());
    let filled = todo_render(&dock, props.as_ref());
    assert!(Object::is(
        &property(&property(&filled, "props"), "todos"),
        todos.as_ref()
    ));

    assert_eq!(
        property(&entry, "name").as_string().as_deref(),
        Some("conversation-todo-dock")
    );
    assert_eq!(
        property(&entry, "inject")
            .unchecked_into::<Array>()
            .get(0)
            .as_string()
            .as_deref(),
        Some("slots")
    );
    property(&entry, "apply")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &todo_context())
        .unwrap();
    assert_eq!(
        property(&todo_inject_calls().get(0), "name")
            .as_string()
            .as_deref(),
        Some("conversation.input.dock")
    );
    let register = todo_register_calls().get(0);
    let options = property(&register, "options");
    assert_eq!(
        property(&options, "id").as_string().as_deref(),
        Some("todo")
    );
    assert_eq!(property(&options, "order").as_f64(), Some(0.0));
    assert_eq!(
        property(&options, "locale").as_string().as_deref(),
        Some("conversation")
    );
    assert!(Object::is(&property(&register, "component"), &dock));
}
