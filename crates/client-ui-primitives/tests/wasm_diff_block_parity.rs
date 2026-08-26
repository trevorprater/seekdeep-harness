//! Live JavaScript coverage for compiled `DiffBlock` rows, caps, and clipboard.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Reflect};
use seekdeep_client_ui_primitives::{configure_client_ui_primitive_blocks, diff_block_component};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let slots = []
let cursor = 0
let writes = []
const styles = []
let timers = []
export function installDiffBench(accepted) {
  slots = []
  cursor = 0
  writes = []
  timers = []
  styles.splice(0)
  globalThis.window = globalThis
  Object.defineProperty(globalThis, 'navigator', { configurable: true, value: {
    clipboard: { writeText(text) { writes.push(text); return accepted ? Promise.resolve() : Promise.reject(new Error('denied')) } },
  } })
  globalThis.setTimeout = (callback, delay) => { const timer = { callback, delay }; timers.push(timer); return timers.length }
  globalThis.document = {
    head: { appendChild(node) { styles.push(node) } },
    createElement(kind) { return { kind, attributes: {}, setAttribute(k, v) { this.attributes[k] = v } } },
  }
  const React = {
    createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } },
    useState(initial) {
      const index = cursor++
      if (!(index in slots)) slots[index] = initial
      return [slots[index], update => { slots[index] = typeof update === 'function' ? update(slots[index]) : update }]
    },
  }
  return { React, styles }
}
export function diffRender(component, props) { cursor = 0; return component(props) }
export function diffObject(entries) { return Object.fromEntries(entries) }
export function diffWrites() { return writes }
export function diffTimers() { return timers }
export function diffFireTimer(index) { timers[index].callback() }
export function diffTick() { return Promise.resolve().then(() => Promise.resolve()) }
export function diffText(node) {
  if (node === null || node === undefined || node === false) return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  return (node.children ?? []).map(diffText).join('')
}
export function diffFindClass(node, part, out = []) {
  if (node === null || node === undefined || node === false) return out
  if (typeof node === 'object') {
    if ((node.props?.className ?? '').includes(part)) out.push(node)
    for (const child of node.children ?? []) diffFindClass(child, part, out)
  }
  return out
}
"#)]
extern "C" {
    fn installDiffBench(accepted: bool) -> JsValue;
    fn diffRender(component: &JsValue, props: &JsValue) -> JsValue;
    fn diffObject(entries: &Array) -> JsValue;
    fn diffWrites() -> Array;
    fn diffTimers() -> Array;
    fn diffFireTimer(index: u32);
    fn diffTick() -> js_sys::Promise;
    fn diffText(node: &JsValue) -> String;
    fn diffFindClass(node: &JsValue, part: &str) -> Array;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn object(entries: &[(&str, JsValue)]) -> JsValue {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    diffObject(&array)
}

fn array(values: &[JsValue]) -> JsValue {
    values.iter().collect::<Array>().into()
}

fn render(component: &JsValue, diffs: &[JsValue], extra: &[(&str, JsValue)]) -> JsValue {
    let mut entries = vec![("diffs", array(diffs))];
    entries.extend_from_slice(extra);
    diffRender(component, &object(&entries))
}

fn invoke(node: &JsValue) {
    property(&property(node, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
}

fn hunk(path: &str, old: Option<&str>, new: &str) -> JsValue {
    object(&[
        ("path", JsValue::from_str(path)),
        ("oldText", old.map_or(JsValue::NULL, JsValue::from_str)),
        ("newText", JsValue::from_str(new)),
    ])
}

#[wasm_bindgen_test(async)]
async fn rows_footer_terminators_same_path_gaps_and_copy_text_are_exact() {
    let bench = installDiffBench(true);
    configure_client_ui_primitive_blocks(property(&bench, "React")).unwrap();
    let component = diff_block_component().unwrap();
    assert!(render(&component, &[], &[]).is_null());
    let diffs = [
        hunk("a.ts", Some("old\n\n"), "new\n"),
        hunk("a.ts", None, "next"),
        hunk("b.ts", Some("gone"), ""),
    ];
    let first = render(
        &component,
        &diffs,
        &[("className", JsValue::from_str("caller"))],
    );
    assert_eq!(property(&first, "kind").as_string().as_deref(), Some("div"));
    assert!(
        property(&property(&first, "props"), "className")
            .as_string()
            .unwrap()
            .contains("caller")
    );
    let lines = diffFindClass(&first, "diffblock-line");
    assert_eq!(
        lines.iter().map(|line| diffText(&line)).collect::<Vec<_>>(),
        ["a.ts", "old", "", "new", "⋯", "next", "b.ts", "gone"]
    );
    assert!(diffText(&first).contains("└ +2 -3 · 2 files"));
    let copy = diffFindClass(&first, "diffblock-copyButton").get(0);
    invoke(&copy);
    JsFuture::from(diffTick()).await.unwrap();
    assert_eq!(
        diffWrites().get(0).as_string().as_deref(),
        Some("a.ts\n- old\n- \n+ new\n⋯\n+ next\nb.ts\n- gone")
    );
    let copied = render(&component, &diffs, &[]);
    assert!(diffText(&copied).contains("复制成功"));
    invoke(&diffFindClass(&copied, "diffblock-copyButton").get(0));
    assert_eq!(diffWrites().length(), 1);
    assert_eq!(
        property(&diffTimers().get(0), "delay").as_f64(),
        Some(1_000.0)
    );
    diffFireTimer(0);
    assert!(diffText(&render(&component, &diffs, &[])).contains("复制"));
}

#[wasm_bindgen_test]
fn head_tail_cap_expands_collapses_and_keeps_the_documented_default() {
    let bench = installDiffBench(true);
    configure_client_ui_primitive_blocks(property(&bench, "React")).unwrap();
    let component = diff_block_component().unwrap();
    let added = (1..=9)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let diffs = [hunk("a.ts", None, &added)];
    let collapsed = render(&component, &diffs, &[("maxLines", JsValue::from_f64(4.0))]);
    assert_eq!(
        diffFindClass(&collapsed, "diffblock-line")
            .iter()
            .map(|line| diffText(&line))
            .collect::<Vec<_>>(),
        ["a.ts", "line 1", "line 8", "line 9"]
    );
    let toggle = diffFindClass(&collapsed, "diffblock-expand").get(0);
    assert_eq!(
        property(&property(&toggle, "props"), "aria-label")
            .as_string()
            .as_deref(),
        Some("展开其余 6 行差异")
    );
    invoke(&toggle);
    let expanded = render(&component, &diffs, &[("maxLines", JsValue::from_f64(4.0))]);
    assert_eq!(diffFindClass(&expanded, "diffblock-line").length(), 10);
    let collapse = diffFindClass(&expanded, "diffblock-expand").get(0);
    assert_eq!(
        property(&property(&collapse, "props"), "aria-expanded").as_bool(),
        Some(true)
    );
    invoke(&collapse);
    assert_eq!(
        diffFindClass(
            &render(&component, &diffs, &[("maxLines", JsValue::from_f64(4.0))]),
            "diffblock-line",
        )
        .length(),
        4
    );

    let default_body = (1..=16)
        .map(|line| format!("x{line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let default = render(&component, &[hunk("a", None, &default_body)], &[]);
    assert_eq!(diffFindClass(&default, "diffblock-line").length(), 16);
}

#[wasm_bindgen_test(async)]
async fn refused_clipboard_write_never_claims_success() {
    let bench = installDiffBench(false);
    configure_client_ui_primitive_blocks(property(&bench, "React")).unwrap();
    let component = diff_block_component().unwrap();
    let diffs = [hunk("a", None, "new")];
    let first = render(&component, &diffs, &[]);
    invoke(&diffFindClass(&first, "diffblock-copyButton").get(0));
    JsFuture::from(diffTick()).await.unwrap();
    assert!(!diffText(&render(&component, &diffs, &[])).contains("复制成功"));
}
