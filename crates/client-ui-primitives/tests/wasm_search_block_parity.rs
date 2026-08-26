//! Live JavaScript coverage for compiled `SearchBlock` shapes, caps, and copy.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Reflect};
use seekdeep_client_ui_primitives::{configure_client_ui_primitive_blocks, search_block_component};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let slots = []
let cursor = 0
let writes = []
let timers = []
const styles = []
export function installSearchBench(accepted) {
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
export function searchRender(component, props) { cursor = 0; return component(props) }
export function searchObject(entries) { return Object.fromEntries(entries) }
export function searchWrites() { return writes }
export function searchTimers() { return timers }
export function searchFireTimer(index) { timers[index].callback() }
export function searchTick() { return Promise.resolve().then(() => Promise.resolve()) }
export function searchText(node) {
  if (node === null || node === undefined || node === false) return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  return (node.children ?? []).map(searchText).join('')
}
export function searchFindClass(node, part, out = []) {
  if (node === null || node === undefined || node === false) return out
  if (typeof node === 'object') {
    if ((node.props?.className ?? '').includes(part)) out.push(node)
    for (const child of node.children ?? []) searchFindClass(child, part, out)
  }
  return out
}
export function searchKeys(node, out = []) {
  if (node === null || node === undefined || node === false) return out
  if (typeof node === 'object') {
    if (node.props?.key !== undefined) out.push(node.props.key)
    for (const child of node.children ?? []) searchKeys(child, out)
  }
  return out
}
"#)]
extern "C" {
    fn installSearchBench(accepted: bool) -> JsValue;
    fn searchRender(component: &JsValue, props: &JsValue) -> JsValue;
    fn searchObject(entries: &Array) -> JsValue;
    fn searchWrites() -> Array;
    fn searchTimers() -> Array;
    fn searchFireTimer(index: u32);
    fn searchTick() -> js_sys::Promise;
    fn searchText(node: &JsValue) -> String;
    fn searchFindClass(node: &JsValue, part: &str) -> Array;
    fn searchKeys(node: &JsValue) -> Array;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn object(entries: &[(&str, JsValue)]) -> JsValue {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    searchObject(&array)
}

fn array(values: &[JsValue]) -> JsValue {
    values.iter().collect::<Array>().into()
}

fn invoke(node: &JsValue) {
    property(&property(node, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
}

fn line(number: i32, text: &str) -> JsValue {
    object(&[
        ("lineNumber", JsValue::from_f64(f64::from(number))),
        ("line", JsValue::from_str(text)),
    ])
}

fn file(path: &str, lines: &[JsValue]) -> JsValue {
    object(&[("path", JsValue::from_str(path)), ("matches", array(lines))])
}

fn group(path: &str, count: i32, from: i32) -> JsValue {
    let lines = (0..count)
        .map(|offset| {
            let number = from + offset;
            line(number, &format!("hit {number}"))
        })
        .collect::<Vec<_>>();
    file(path, &lines)
}

fn render(component: &JsValue, entries: &[(&str, JsValue)]) -> JsValue {
    searchRender(component, &object(entries))
}

fn texts(tree: &JsValue, class: &str) -> Vec<String> {
    let suffix = format!("-{}", class.rsplit('-').next().unwrap_or(class));
    searchFindClass(tree, class)
        .iter()
        .filter(|node| {
            property(&property(node, "props"), "className")
                .as_string()
                .is_some_and(|classes| {
                    classes
                        .split_whitespace()
                        .any(|name| name.ends_with(&suffix))
                })
        })
        .map(|node| searchText(&node))
        .collect()
}

#[wasm_bindgen_test]
fn match_groups_paths_summaries_empty_and_file_collapse_are_exact() {
    let bench = installSearchBench(true);
    configure_client_ui_primitive_blocks(property(&bench, "React")).unwrap();
    configure_client_ui_primitive_blocks(property(&bench, "React")).unwrap();
    let styles = Array::from(&property(&bench, "styles"));
    assert_eq!(styles.length(), 5);
    assert_eq!(
        property(&property(&styles.get(0), "attributes"), "data-plugin")
            .as_string()
            .as_deref(),
        Some("@seekdeep-ai/seekdeep-client-ui-primitives")
    );
    let component = search_block_component().unwrap();
    let files = [
        file("a.ts", &[line(12, "const a = 1"), line(40, "return a")]),
        file("b.ts", &[line(7, "const b = 2")]),
    ];
    let entries = [
        ("kind", JsValue::from_str("matches")),
        ("truncated", JsValue::FALSE),
        ("total", JsValue::from_f64(3.0)),
        ("files", array(&files)),
    ];
    let first = render(&component, &entries);
    assert_eq!(texts(&first, "searchblock-fileHeader"), ["a.ts2", "b.ts1"]);
    assert_eq!(
        texts(&first, "searchblock-line"),
        ["12: const a = 1", "40: return a", "7: const b = 2"]
    );
    assert!(searchText(&first).contains("3 处匹配 · 2 个文件"));
    let header = searchFindClass(&first, "searchblock-fileHeader").get(0);
    invoke(&header);
    let collapsed = render(&component, &entries);
    assert_eq!(texts(&collapsed, "searchblock-line"), ["7: const b = 2"]);
    assert_eq!(
        property(
            &property(
                &searchFindClass(&collapsed, "searchblock-fileHeader").get(0),
                "props"
            ),
            "aria-expanded",
        )
        .as_bool(),
        Some(false)
    );
    invoke(&searchFindClass(&collapsed, "searchblock-fileHeader").get(0));
    assert_eq!(
        texts(&render(&component, &entries), "searchblock-line").len(),
        3
    );

    let truncated = render(
        &component,
        &[
            ("kind", JsValue::from_str("matches")),
            ("truncated", JsValue::TRUE),
            ("total", JsValue::from_f64(99.0)),
            ("files", array(&[group("a.ts", 2, 1)])),
        ],
    );
    assert!(searchText(&truncated).contains("显示 2 / 共 99 处匹配 · 1 个文件"));
    let paths = render(
        &component,
        &[
            ("kind", JsValue::from_str("paths")),
            ("truncated", JsValue::TRUE),
            ("total", JsValue::from_f64(50.0)),
            (
                "paths",
                array(&[JsValue::from_str("a"), JsValue::from_str("b")]),
            ),
        ],
    );
    assert_eq!(texts(&paths, "searchblock-line"), ["a", "b"]);
    assert!(searchText(&paths).contains("显示 2 / 共 50 个路径"));

    for (kind, field) in [("matches", "files"), ("paths", "paths")] {
        let empty = render(
            &component,
            &[
                ("kind", JsValue::from_str(kind)),
                ("truncated", JsValue::FALSE),
                ("total", JsValue::from_f64(0.0)),
                (field, array(&[])),
            ],
        );
        assert!(searchText(&empty).contains("无结果"));
        assert_eq!(
            searchFindClass(&empty, "searchblock-copyButton").length(),
            0
        );
    }
}

#[wasm_bindgen_test]
fn caps_count_headers_restore_tail_ownership_and_expand_collapse() {
    let bench = installSearchBench(true);
    configure_client_ui_primitive_blocks(property(&bench, "React")).unwrap();
    let component = search_block_component().unwrap();
    let paths = (1..=10)
        .map(|index| JsValue::from_str(&format!("p{index}")))
        .collect::<Vec<_>>();
    let props = [
        ("kind", JsValue::from_str("paths")),
        ("truncated", JsValue::FALSE),
        ("total", JsValue::from_f64(10.0)),
        ("paths", array(&paths)),
        ("maxLines", JsValue::from_f64(4.0)),
    ];
    let collapsed = render(&component, &props);
    assert_eq!(
        texts(&collapsed, "searchblock-line"),
        ["p1", "p2", "p9", "p10"]
    );
    assert_eq!(
        searchKeys(&collapsed)
            .iter()
            .filter_map(|value| value.as_string())
            .collect::<Vec<_>>(),
        ["path:p1", "path:p2", "path:p9", "path:p10"]
    );
    let toggle = searchFindClass(&collapsed, "searchblock-expand").get(0);
    assert_eq!(
        property(&property(&toggle, "props"), "aria-label")
            .as_string()
            .as_deref(),
        Some("展开其余 6 行结果")
    );
    invoke(&toggle);
    let expanded = render(&component, &props);
    assert_eq!(texts(&expanded, "searchblock-line").len(), 10);
    invoke(&searchFindClass(&expanded, "searchblock-expand").get(0));
    assert_eq!(
        texts(&render(&component, &props), "searchblock-line").len(),
        4
    );

    let grouped = render(
        &component,
        &[
            ("kind", JsValue::from_str("matches")),
            ("truncated", JsValue::FALSE),
            ("total", JsValue::from_f64(20.0)),
            ("maxLines", JsValue::from_f64(8.0)),
            (
                "files",
                array(&[group("a.ts", 10, 1), group("b.ts", 10, 11)]),
            ),
        ],
    );
    assert_eq!(
        texts(&grouped, "searchblock-fileHeader"),
        ["a.ts10", "b.ts10"]
    );
    assert_eq!(
        texts(&grouped, "searchblock-line"),
        [
            "1: hit 1",
            "2: hit 2",
            "3: hit 3",
            "18: hit 18",
            "19: hit 19",
            "20: hit 20"
        ]
    );
    assert_eq!(
        property(
            &property(
                &searchFindClass(&grouped, "searchblock-expand").get(0),
                "props"
            ),
            "aria-label",
        )
        .as_string()
        .as_deref(),
        Some("展开其余 14 行结果")
    );
    assert!(
        searchKeys(&grouped)
            .iter()
            .filter_map(|value| value.as_string())
            .any(|key| key == "tailHeader:file:1")
    );

    let uncapped = render(
        &component,
        &[
            ("kind", JsValue::from_str("paths")),
            ("truncated", JsValue::FALSE),
            ("total", JsValue::from_f64(10.0)),
            ("paths", array(&paths)),
            ("maxLines", JsValue::from_f64(f64::NAN)),
        ],
    );
    assert_eq!(texts(&uncapped, "searchblock-line").len(), 10);
    assert_eq!(searchFindClass(&uncapped, "searchblock-expand").length(), 0);
}

#[wasm_bindgen_test(async)]
async fn copy_uses_complete_structure_ignores_visual_collapse_and_respects_refusal() {
    let bench = installSearchBench(true);
    configure_client_ui_primitive_blocks(property(&bench, "React")).unwrap();
    let component = search_block_component().unwrap();
    let props = [
        ("kind", JsValue::from_str("matches")),
        ("truncated", JsValue::TRUE),
        ("total", JsValue::from_f64(9.0)),
        ("maxLines", JsValue::from_f64(2.0)),
        (
            "files",
            array(&[
                file("a.ts", &[line(1, "x"), line(2, "y")]),
                file("b.ts", &[line(3, "z")]),
            ]),
        ),
        ("className", JsValue::from_str("caller")),
    ];
    let first = render(&component, &props);
    invoke(&searchFindClass(&first, "searchblock-fileHeader").get(0));
    let collapsed = render(&component, &props);
    invoke(&searchFindClass(&collapsed, "searchblock-copyButton").get(0));
    JsFuture::from(searchTick()).await.unwrap();
    assert_eq!(
        searchWrites().get(0).as_string().as_deref(),
        Some("a.ts\n1: x\n2: y\n\nb.ts\n3: z")
    );
    let copied = render(&component, &props);
    assert!(searchText(&copied).contains("复制成功"));
    invoke(&searchFindClass(&copied, "searchblock-copyButton").get(0));
    assert_eq!(searchWrites().length(), 1);
    assert_eq!(
        property(&searchTimers().get(0), "delay").as_f64(),
        Some(1_000.0)
    );
    searchFireTimer(0);
    assert!(
        property(&property(&render(&component, &props), "props"), "className")
            .as_string()
            .unwrap()
            .contains("caller")
    );

    let bench = installSearchBench(false);
    configure_client_ui_primitive_blocks(property(&bench, "React")).unwrap();
    let component = search_block_component().unwrap();
    let paths = [
        ("kind", JsValue::from_str("paths")),
        ("truncated", JsValue::FALSE),
        ("total", JsValue::from_f64(2.0)),
        (
            "paths",
            array(&[JsValue::from_str("src/a.ts"), JsValue::from_str("src/b.ts")]),
        ),
    ];
    let first = render(&component, &paths);
    invoke(&searchFindClass(&first, "searchblock-copyButton").get(0));
    JsFuture::from(searchTick()).await.unwrap();
    assert_eq!(
        searchWrites().get(0).as_string().as_deref(),
        Some("src/a.ts\nsrc/b.ts")
    );
    assert!(!searchText(&render(&component, &paths)).contains("复制成功"));
}
