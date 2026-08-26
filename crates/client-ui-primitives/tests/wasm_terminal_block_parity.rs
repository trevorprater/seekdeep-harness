//! Live browser-boundary coverage for compiled `TerminalBlock` behavior.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Reflect};
use seekdeep_client_ui_primitives::{
    configure_client_ui_primitive_blocks, terminal_block_component,
};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let slots = []
let cursor = 0
let writes = []
let timers = []
const styles = []
export function installTerminalBench(accepted) {
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
    querySelector(selector) {
      const match = selector.match(/data-plugin-css="([^"]+)"/)
      return match === null ? null : styles.find(style => style.attributes['data-plugin-css'] === match[1]) ?? null
    },
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
export function terminalRender(component, props) { cursor = 0; return component(props) }
export function terminalObject(entries) { return Object.fromEntries(entries) }
export function terminalWrites() { return writes }
export function terminalTimers() { return timers }
export function terminalFireTimer(index) { timers[index].callback() }
export function terminalTick() { return Promise.resolve().then(() => Promise.resolve()) }
export function terminalText(node) {
  if (node === null || node === undefined || node === false) return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  return (node.children ?? []).map(terminalText).join('')
}
export function terminalFindClass(node, part, out = []) {
  if (node === null || node === undefined || node === false) return out
  if (typeof node === 'object') {
    if ((node.props?.className ?? '').includes(part)) out.push(node)
    for (const child of node.children ?? []) terminalFindClass(child, part, out)
  }
  return out
}
export function terminalFindState(node) {
  if (node === null || node === undefined || node === false) return undefined
  if (typeof node === 'object') {
    if (node.props?.['data-state'] !== undefined) return node
    for (const child of node.children ?? []) { const found = terminalFindState(child); if (found !== undefined) return found }
  }
  return undefined
}
"#)]
extern "C" {
    fn installTerminalBench(accepted: bool) -> JsValue;
    fn terminalRender(component: &JsValue, props: &JsValue) -> JsValue;
    fn terminalObject(entries: &Array) -> JsValue;
    fn terminalWrites() -> Array;
    fn terminalTimers() -> Array;
    fn terminalFireTimer(index: u32);
    fn terminalTick() -> js_sys::Promise;
    fn terminalText(node: &JsValue) -> String;
    fn terminalFindClass(node: &JsValue, part: &str) -> Array;
    fn terminalFindState(node: &JsValue) -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn object(entries: &[(&str, JsValue)]) -> JsValue {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    terminalObject(&array)
}

fn render(component: &JsValue, entries: &[(&str, JsValue)]) -> JsValue {
    terminalRender(component, &object(entries))
}

fn texts(tree: &JsValue, class: &str) -> Vec<String> {
    terminalFindClass(tree, class)
        .iter()
        .map(|node| terminalText(&node))
        .collect()
}

fn invoke(node: &JsValue) {
    property(&property(node, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
}

fn setup(accepted: bool) -> JsValue {
    let bench = installTerminalBench(accepted);
    configure_client_ui_primitive_blocks(property(&bench, "React")).unwrap();
    terminal_block_component().unwrap()
}

fn base(command: &str) -> Vec<(&str, JsValue)> {
    vec![("command", JsValue::from_str(command))]
}

#[wasm_bindgen_test]
fn prompt_paths_multiline_rows_and_run_states_are_exact() {
    let component = setup(true);
    let mut entries = base("echo one\necho two\n");
    entries.extend([
        ("cwd", JsValue::from_str("C:\\Users\\me\\Projects\\")),
        ("home", JsValue::from_str("C:\\Users\\me")),
        ("output", JsValue::from_str("done")),
        ("exitCode", JsValue::from_f64(0.0)),
    ]);
    let tree = render(&component, &entries);
    assert_eq!(
        texts(&tree, "terminalblock-promptLine"),
        ["Projectsecho one", "$echo two"]
    );
    assert_eq!(texts(&tree, "terminalblock-runStateLabel"), ["已完成"]);
    assert_eq!(
        property(&property(&terminalFindState(&tree), "props"), "data-state")
            .as_string()
            .as_deref(),
        Some("done")
    );
    let home = render(
        &component,
        &[
            ("command", JsValue::from_str("ls")),
            ("cwd", JsValue::from_str("/Users/me/")),
            ("home", JsValue::from_str("/Users/me")),
            ("running", JsValue::TRUE),
            ("signal", JsValue::from_str("SIGINT")),
            ("output", JsValue::from_str("partial")),
        ],
    );
    assert_eq!(texts(&home, "terminalblock-cwd"), ["~"]);
    assert_eq!(texts(&home, "terminalblock-runStateLabel"), ["运行中"]);
    assert_eq!(
        property(&property(&terminalFindState(&home), "props"), "data-state")
            .as_string()
            .as_deref(),
        Some("ongoing")
    );
    assert!(terminalText(&home).contains("信号 SIGINT"));
    assert!(!terminalText(&home).contains("partial"));
    assert_eq!(
        terminalFindClass(&home, "terminalblock-copyButton").length(),
        0
    );
    assert!(!property(&home, "props").is_undefined());
    assert_eq!(
        property(&property(&home, "props"), "data-running").as_string(),
        Some(String::new())
    );

    let root = render(
        &component,
        &[
            ("command", JsValue::from_str("pwd")),
            ("cwd", JsValue::from_str("/")),
        ],
    );
    assert_eq!(texts(&root, "terminalblock-cwd"), ["/"]);
}

#[wasm_bindgen_test]
fn emptiness_terminators_ansi_spans_and_status_precedence_are_exact() {
    let component = setup(true);
    for output in ["", "  \n ", "\u{1b}[0m", "\u{1b}]0;title\u{1b}\\"] {
        let tree = render(
            &component,
            &[
                ("command", JsValue::from_str("true")),
                ("output", JsValue::from_str(output)),
            ],
        );
        assert!(terminalText(&tree).contains("无输出"));
        assert_eq!(
            terminalFindClass(&tree, "terminalblock-copyButton").length(),
            0
        );
    }

    let terminated = render(
        &component,
        &[
            ("command", JsValue::from_str("ls")),
            ("output", JsValue::from_str("a\nb\n\u{1b}[0m")),
        ],
    );
    assert_eq!(texts(&terminated, "terminalblock-line"), ["a", "b"]);
    let blank = render(
        &component,
        &[
            ("command", JsValue::from_str("ls")),
            ("output", JsValue::from_str("a\n\n")),
        ],
    );
    assert_eq!(texts(&blank, "terminalblock-line"), ["a", ""]);

    let ansi = render(
        &component,
        &[
            ("command", JsValue::from_str("paint")),
            ("output", JsValue::from_str("\u{1b}[31mred\u{1b}[0m plain")),
        ],
    );
    let line = terminalFindClass(&ansi, "terminalblock-line").get(0);
    let first = Array::from(&property(&line, "children")).get(0);
    assert_eq!(
        property(&first, "kind").as_string().as_deref(),
        Some("span")
    );
    assert_eq!(
        property(&property(&property(&first, "props"), "style"), "color")
            .as_string()
            .as_deref(),
        Some("var(--dsw-alias-state-error-primary)")
    );

    let failed = render(
        &component,
        &[
            ("command", JsValue::from_str("false")),
            ("output", JsValue::from_str("failed")),
            ("exitCode", JsValue::from_f64(2.0)),
            ("signal", JsValue::from_str("SIGTERM")),
        ],
    );
    assert!(terminalText(&failed).contains("信号 SIGTERM"));
    assert!(!terminalText(&failed).contains("退出码 2"));
    assert_eq!(texts(&failed, "terminalblock-runStateLabel"), ["失败"]);
    assert_eq!(
        property(
            &property(&terminalFindState(&failed), "props"),
            "data-state"
        )
        .as_string()
        .as_deref(),
        Some("error")
    );
}

#[wasm_bindgen_test]
fn head_tail_cap_expand_collapse_default_and_nan_are_exact() {
    let component = setup(true);
    let output = (1..=20)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let props = [
        ("command", JsValue::from_str("lines")),
        ("output", JsValue::from_str(&output)),
        ("maxLines", JsValue::from_f64(4.0)),
    ];
    let collapsed = render(&component, &props);
    assert_eq!(
        texts(&collapsed, "terminalblock-line"),
        ["line 1", "line 2", "line 19", "line 20"]
    );
    let toggle = terminalFindClass(&collapsed, "terminalblock-expand").get(0);
    assert_eq!(
        property(&property(&toggle, "props"), "aria-label")
            .as_string()
            .as_deref(),
        Some("展开其余 16 行输出")
    );
    invoke(&toggle);
    let expanded = render(&component, &props);
    assert_eq!(texts(&expanded, "terminalblock-line").len(), 20);
    invoke(&terminalFindClass(&expanded, "terminalblock-expand").get(0));
    assert_eq!(
        texts(&render(&component, &props), "terminalblock-line").len(),
        4
    );

    let defaults = render(
        &component,
        &[
            ("command", JsValue::from_str("lines")),
            ("output", JsValue::from_str(&output)),
        ],
    );
    assert_eq!(texts(&defaults, "terminalblock-line").len(), 16);
    let uncapped = render(
        &component,
        &[
            ("command", JsValue::from_str("lines")),
            ("output", JsValue::from_str(&output)),
            ("maxLines", JsValue::from_f64(f64::NAN)),
        ],
    );
    assert_eq!(texts(&uncapped, "terminalblock-line").len(), 20);
}

#[wasm_bindgen_test(async)]
async fn raw_copy_success_duplicate_suppression_refusal_and_custom_labels_are_exact() {
    let component = setup(true);
    let output = "a\nb\n";
    let props = [
        ("command", JsValue::from_str("copy")),
        ("output", JsValue::from_str(output)),
        ("exitCode", JsValue::from_f64(3.0)),
        ("className", JsValue::from_str("caller")),
    ];
    let first = render(&component, &props);
    invoke(&terminalFindClass(&first, "terminalblock-copyButton").get(0));
    JsFuture::from(terminalTick()).await.unwrap();
    assert_eq!(terminalWrites().get(0).as_string().as_deref(), Some(output));
    let copied = render(&component, &props);
    assert!(terminalText(&copied).contains("复制成功"));
    invoke(&terminalFindClass(&copied, "terminalblock-copyButton").get(0));
    assert_eq!(terminalWrites().length(), 1);
    assert_eq!(
        property(&terminalTimers().get(0), "delay").as_f64(),
        Some(1_000.0)
    );
    terminalFireTimer(0);
    assert!(
        property(&property(&render(&component, &props), "props"), "className")
            .as_string()
            .unwrap()
            .contains("caller")
    );

    let signal = Closure::wrap(
        Box::new(|value: String| format!("signal={value}")) as Box<dyn FnMut(String) -> String>
    )
    .into_js_value();
    let labels = object(&[
        ("signal", signal),
        ("running", JsValue::from_str("RUN")),
        ("copy", JsValue::from_str("COPY")),
    ]);
    let custom = render(
        &component,
        &[
            ("command", JsValue::from_str("x")),
            ("output", JsValue::from_str("x")),
            ("signal", JsValue::from_str("S")),
            ("labels", labels),
        ],
    );
    assert!(terminalText(&custom).contains("signal=S"));
    assert!(terminalText(&custom).contains("COPY"));

    let bench = installTerminalBench(false);
    configure_client_ui_primitive_blocks(property(&bench, "React")).unwrap();
    let component = terminal_block_component().unwrap();
    let refused = render(
        &component,
        &[
            ("command", JsValue::from_str("copy")),
            ("output", JsValue::from_str("x")),
        ],
    );
    invoke(&terminalFindClass(&refused, "terminalblock-copyButton").get(0));
    JsFuture::from(terminalTick()).await.unwrap();
    assert!(
        !terminalText(&render(
            &component,
            &[
                ("command", JsValue::from_str("copy")),
                ("output", JsValue::from_str("x")),
            ],
        ))
        .contains("复制成功")
    );
}
