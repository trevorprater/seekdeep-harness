//! Live Rust/WASM React coverage for simple trajectory rows and headers.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_trajectory::{
    configure_client_ui_trajectory, configure_client_ui_trajectory_modules,
    trajectory_cell_component, trajectory_group_header_component, trajectory_toolbar_component,
    trajectory_turn_component, trajectory_turn_header_component,
};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function makeTrajectorySimpleBench() {
  const styles = []
  globalThis.document = {
    head: { appendChild(node) { styles.push(node) } },
    createElement(kind) {
      return { kind, attrs: {}, setAttribute(key, value) { this.attrs[key] = value } }
    },
    querySelector(selector) {
      const match = /^style\[data-plugin-css=(.+)\]$/.exec(selector)
      if (match === null) return null
      const id = JSON.parse(match[1])
      return styles.find(node => node.attrs['data-plugin-css'] === id) ?? null
    },
  }
  const React = {
    createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } },
  }
  return { React, styles, primitives: { IconSearchOutline16: 'IconSearchOutline16' } }
}

export function trajectoryNodeText(node) {
  if (node === null || node === undefined || node === false) return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(trajectoryNodeText).join('')
  return (node.children ?? []).map(trajectoryNodeText).join('')
}

export function trajectoryFindClass(node, className) {
  if (node === null || node === undefined || node === false) return undefined
  if (typeof node === 'string' || typeof node === 'number') return undefined
  if (Array.isArray(node)) {
    for (const child of node) {
      const found = trajectoryFindClass(child, className)
      if (found !== undefined) return found
    }
    return undefined
  }
  if (String(node.props?.className ?? '').split(/\s+/).includes(className)) return node
  for (const child of node.children ?? []) {
    const found = trajectoryFindClass(child, className)
    if (found !== undefined) return found
  }
  return undefined
}

export function trajectoryCountClass(node, className) {
  if (node === null || node === undefined || node === false) return 0
  if (typeof node === 'string' || typeof node === 'number') return 0
  if (Array.isArray(node)) return node.reduce((sum, child) => sum + trajectoryCountClass(child, className), 0)
  let count = String(node.props?.className ?? '').split(/\s+/).includes(className) ? 1 : 0
  for (const child of node.children ?? []) count += trajectoryCountClass(child, className)
  return count
}

export function trajectoryFindProp(node, property, value) {
  if (node === null || node === undefined || node === false) return undefined
  if (typeof node === 'string' || typeof node === 'number') return undefined
  if (Array.isArray(node)) {
    for (const child of node) {
      const found = trajectoryFindProp(child, property, value)
      if (found !== undefined) return found
    }
    return undefined
  }
  if (node.props?.[property] === value) return node
  for (const child of node.children ?? []) {
    const found = trajectoryFindProp(child, property, value)
    if (found !== undefined) return found
  }
  return undefined
}

export function trajectoryStyles(bench) { return bench.styles.length }
export function trajectoryProperty(value, key) { return value?.[key] }
export function trajectoryChildren(node) { return node?.children ?? [] }
"#)]
extern "C" {
    fn makeTrajectorySimpleBench() -> JsValue;
    fn trajectoryNodeText(node: &JsValue) -> String;
    fn trajectoryFindClass(node: &JsValue, class_name: &str) -> JsValue;
    fn trajectoryCountClass(node: &JsValue, class_name: &str) -> u32;
    fn trajectoryFindProp(node: &JsValue, property: &str, value: &JsValue) -> JsValue;
    fn trajectoryStyles(bench: &JsValue) -> u32;
    fn trajectoryProperty(value: &JsValue, key: &str) -> JsValue;
    fn trajectoryChildren(node: &JsValue) -> Array;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn props(entries: &[(&str, JsValue)]) -> JsValue {
    let value = Object::new();
    for (key, entry) in entries {
        Reflect::set(&value, &JsValue::from_str(key), entry).unwrap();
    }
    value.into()
}

fn render(component: JsValue, props: &JsValue) -> JsValue {
    component
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, props)
        .unwrap()
}

#[wasm_bindgen_test]
fn cell_preserves_source_labels_metrics_duration_selection_and_rest_props() {
    let bench = makeTrajectorySimpleBench();
    configure_client_ui_trajectory(property(&bench, "React")).unwrap();
    let cell = trajectory_cell_component().unwrap();
    let tool = render(
        cell.clone(),
        &props(&[
            ("index", JsValue::from_f64(6.0)),
            ("kind", JsValue::from_str("tool")),
            ("text", JsValue::from_str("bash · Read src/index.ts")),
            ("timeSeconds", JsValue::from_f64(5.0)),
            ("input", JsValue::from_f64(1.0)),
            ("output", JsValue::from_f64(2.0)),
            ("think", JsValue::from_f64(3.0)),
        ]),
    );
    assert!(trajectoryNodeText(&tool).contains("#6Toolbash · Read src/index.ts5,000 ms"));
    assert_eq!(
        trajectoryProperty(&property(&tool, "props"), "data-kind")
            .as_string()
            .as_deref(),
        Some("tool")
    );
    assert_eq!(trajectoryCountClass(&tool, "seekdeep-trajectory-metric"), 0);

    let message = render(
        cell,
        &props(&[
            ("index", JsValue::from_f64(3.0)),
            ("kind", JsValue::from_str("message")),
            ("text", JsValue::from_str("answer")),
            ("timeSeconds", JsValue::from_f64(235.2)),
            ("input", JsValue::from_f64(136.0)),
            ("output", JsValue::from_f64(381.0)),
            ("think", JsValue::from_f64(155.0)),
            ("selected", JsValue::TRUE),
            ("className", JsValue::from_str("custom")),
            ("data-probe", JsValue::from_str("kept")),
            ("inputDetail", JsValue::from_str("private")),
            (
                "previewMarkdown",
                JsValue::from_str("forwarded-like-source"),
            ),
        ]),
    );
    let text = trajectoryNodeText(&message);
    let input = text.find("136").unwrap();
    let output = text.find("381").unwrap();
    let think = text.find("155").unwrap();
    let time = text.find("235,200 ms").unwrap();
    assert!(input < output && output < think && think < time);
    assert_eq!(
        trajectoryCountClass(&message, "seekdeep-trajectory-metric"),
        3
    );
    let root_props = property(&message, "props");
    assert_eq!(
        trajectoryProperty(&root_props, "data-selected").as_bool(),
        Some(true)
    );
    assert_eq!(
        trajectoryProperty(&root_props, "data-probe")
            .as_string()
            .as_deref(),
        Some("kept")
    );
    assert!(trajectoryProperty(&root_props, "inputDetail").is_undefined());
    assert_eq!(
        trajectoryProperty(&root_props, "previewMarkdown")
            .as_string()
            .as_deref(),
        Some("forwarded-like-source")
    );
    assert!(
        trajectoryProperty(&root_props, "className")
            .as_string()
            .unwrap()
            .contains("custom")
    );
}

#[wasm_bindgen_test]
fn headers_turn_wrapper_and_style_injection_match_source_structure() {
    let bench = makeTrajectorySimpleBench();
    let react = property(&bench, "React");
    configure_client_ui_trajectory(react.clone()).unwrap();
    configure_client_ui_trajectory(react).unwrap();
    assert_eq!(trajectoryStyles(&bench), 1);

    let group = trajectory_group_header_component().unwrap();
    let described = render(
        group.clone(),
        &props(&[
            ("title", JsValue::from_str("Step 1")),
            ("description", JsValue::from_str("2.2s skill")),
        ]),
    );
    assert_eq!(trajectoryNodeText(&described), "Step 12.2s skill");
    let plain = render(group, &props(&[("title", JsValue::from_str("Message"))]));
    assert!(trajectoryFindClass(&plain, "seekdeep-trajectory-group-description").is_undefined());

    let header = render(
        trajectory_turn_header_component().unwrap(),
        &props(&[("turn", JsValue::from_f64(1.0))]),
    );
    assert_eq!(trajectoryNodeText(&header), "Turn 1InputOutputThinkTime");
    let columns = trajectoryFindClass(&header, "seekdeep-trajectory-turn-columns");
    assert_eq!(
        trajectoryProperty(&property(&columns, "props"), "aria-hidden").as_bool(),
        Some(true)
    );

    let child = props(&[("kind", JsValue::from_str("child"))]);
    let turn = render(
        trajectory_turn_component().unwrap(),
        &props(&[
            ("turn", JsValue::from_f64(3.0)),
            ("children", child.clone()),
        ]),
    );
    assert_eq!(
        trajectoryProperty(&property(&turn, "props"), "data-turn").as_f64(),
        Some(3.0)
    );
    assert!(trajectoryNodeText(&turn).starts_with("Turn 3InputOutputThinkTime"));
    let body = trajectoryFindClass(&turn, "seekdeep-trajectory-turn-body");
    assert!(Object::is(&trajectoryChildren(&body).get(0), &child));
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)] // One render retains every callback for live invocation.
fn toolbar_toggles_hidden_time_control_and_search_callback_match_source() {
    let bench = makeTrajectorySimpleBench();
    configure_client_ui_trajectory_modules(
        property(&bench, "React"),
        property(&bench, "primitives"),
    )
    .unwrap();
    let calls = Rc::new(RefCell::new(Vec::<String>::new()));
    let duration_calls = calls.clone();
    let duration = Closure::wrap(Box::new(move |value: bool| {
        duration_calls
            .borrow_mut()
            .push(format!("duration:{value}"));
    }) as Box<dyn FnMut(bool)>);
    let time_calls = calls.clone();
    let time = Closure::wrap(Box::new(move |value: bool| {
        time_calls.borrow_mut().push(format!("time:{value}"));
    }) as Box<dyn FnMut(bool)>);
    let turn_calls = calls.clone();
    let turns = Closure::wrap(Box::new(move || {
        turn_calls.borrow_mut().push("turns".to_owned());
    }) as Box<dyn FnMut()>);
    let assistant_calls = calls.clone();
    let assistants = Closure::wrap(Box::new(move || {
        assistant_calls.borrow_mut().push("assistants".to_owned());
    }) as Box<dyn FnMut()>);
    let search_calls = calls.clone();
    let search = Closure::wrap(Box::new(move |value: String| {
        search_calls.borrow_mut().push(format!("search:{value}"));
    }) as Box<dyn FnMut(String)>);
    let translate = Closure::wrap(Box::new(move |key: String| -> String {
        match key.as_str() {
            "toolbar.aria" => "Trajectory toolbar",
            "toolbar.duration" => "Duration",
            "toolbar.useActualDuration" => "Use actual duration",
            "toolbar.useEqualWidth" => "Use equal-width operations",
            "toolbar.actualTime" => "Actual time",
            "toolbar.turns" => "Turns",
            "toolbar.expandTurns" => "Expand turns",
            "toolbar.collapseTurns" => "Collapse turns",
            "toolbar.calls" => "Calls",
            "toolbar.expandCalls" => "Expand calls",
            "toolbar.collapseCalls" => "Collapse calls",
            "toolbar.search" => "Search trajectory",
            "toolbar.searchPlaceholder" => "Search",
            other => other,
        }
        .to_owned()
    }) as Box<dyn FnMut(String) -> String>);
    let tree = render(
        trajectory_toolbar_component().unwrap(),
        &props(&[
            ("actualDuration", JsValue::FALSE),
            ("onActualDurationChange", duration.into_js_value()),
            ("actualTime", JsValue::FALSE),
            ("onActualTimeChange", time.into_js_value()),
            ("allTurnsCollapsed", JsValue::FALSE),
            ("onToggleAllTurns", turns.into_js_value()),
            ("allAssistantsCollapsed", JsValue::TRUE),
            ("onToggleAllAssistants", assistants.into_js_value()),
            ("searchQuery", JsValue::from_str("needle")),
            ("onSearchQueryChange", search.into_js_value()),
            ("t", translate.into_js_value()),
        ]),
    );
    let root_props = property(&tree, "props");
    assert_eq!(
        trajectoryProperty(&root_props, "role")
            .as_string()
            .as_deref(),
        Some("toolbar")
    );
    assert_eq!(
        trajectoryProperty(&root_props, "aria-label")
            .as_string()
            .as_deref(),
        Some("Trajectory toolbar")
    );
    let duration_button = trajectoryFindProp(
        &tree,
        "aria-label",
        &JsValue::from_str("Use actual duration"),
    );
    property(&property(&duration_button, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    let turn_button = trajectoryFindProp(&tree, "aria-label", &JsValue::from_str("Collapse turns"));
    property(&property(&turn_button, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    let calls_button = trajectoryFindProp(&tree, "aria-label", &JsValue::from_str("Expand calls"));
    assert_eq!(
        trajectoryProperty(&property(&calls_button, "props"), "aria-pressed").as_bool(),
        Some(true)
    );
    property(&property(&calls_button, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    let hidden = trajectoryFindProp(&tree, "role", &JsValue::from_str("switch"));
    assert_eq!(
        trajectoryProperty(&property(&hidden, "props"), "hidden").as_bool(),
        Some(true)
    );
    let input = trajectoryFindProp(&tree, "aria-label", &JsValue::from_str("Search trajectory"));
    assert_eq!(
        trajectoryProperty(&property(&input, "props"), "value")
            .as_string()
            .as_deref(),
        Some("needle")
    );
    let current = props(&[("value", JsValue::from_str("updated"))]);
    let event = props(&[("currentTarget", current)]);
    property(&property(&input, "props"), "onChange")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &event)
        .unwrap();
    assert_eq!(
        calls.borrow().as_slice(),
        ["duration:true", "turns", "assistants", "search:updated"]
    );
}
