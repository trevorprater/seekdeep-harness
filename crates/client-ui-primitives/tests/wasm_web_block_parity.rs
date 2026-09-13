//! Live JavaScript coverage for compiled `WebBlock` search and fetch cards.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Reflect};
use seekdeep_client_ui_primitives::{configure_client_ui_primitive_web, web_block_component};
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
const styles = []
export function installWebBench() {
  styles.splice(0)
  globalThis.document = {
    head: { appendChild(node) { styles.push(node) } },
    createElement(kind) { return { kind, attributes: {}, setAttribute(k, v) { this.attributes[k] = v } } },
    querySelector(selector) {
      const match = selector.match(/data-plugin-css="([^"]+)"/)
      return match === null ? null : styles.find(style => style.attributes['data-plugin-css'] === match[1]) ?? null
    },
  }
  const React = { createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } } }
  return { React, Markdown: 'MarkdownText', styles }
}
export function webRender(component, props) { return component(props) }
export function webObject(entries) { return Object.fromEntries(entries) }
export function webText(node) {
  if (node === null || node === undefined || node === false) return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  return (node.children ?? []).map(webText).join('')
}
export function webFindKind(node, kind, out = []) {
  if (node === null || node === undefined || node === false) return out
  if (typeof node === 'object') {
    if (node.kind === kind) out.push(node)
    for (const child of node.children ?? []) webFindKind(child, kind, out)
  }
  return out
}
export function webFindClass(node, part, out = []) {
  if (node === null || node === undefined || node === false) return out
  if (typeof node === 'object') {
    if ((node.props?.className ?? '').includes(part)) out.push(node)
    for (const child of node.children ?? []) webFindClass(child, part, out)
  }
  return out
}
"#)]
extern "C" {
    fn installWebBench() -> JsValue;
    fn webRender(component: &JsValue, props: &JsValue) -> JsValue;
    fn webObject(entries: &Array) -> JsValue;
    fn webText(node: &JsValue) -> String;
    fn webFindKind(node: &JsValue, kind: &str) -> Array;
    fn webFindClass(node: &JsValue, part: &str) -> Array;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn object(entries: &[(&str, JsValue)]) -> JsValue {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    webObject(&array)
}

fn array(values: &[JsValue]) -> JsValue {
    values.iter().collect::<Array>().into()
}

fn source(entries: &[(&str, JsValue)]) -> JsValue {
    object(entries)
}

fn setup() -> (JsValue, JsValue) {
    let bench = installWebBench();
    configure_client_ui_primitive_web(property(&bench, "React"), property(&bench, "Markdown"))
        .unwrap();
    (bench, web_block_component().unwrap())
}

#[wasm_bindgen_test]
fn search_renders_answer_before_safe_labeled_citations_and_optional_metadata() {
    let (_bench, component) = setup();
    let bench = installWebBench();
    configure_client_ui_primitive_web(property(&bench, "React"), property(&bench, "Markdown"))
        .unwrap();
    configure_client_ui_primitive_web(property(&bench, "React"), property(&bench, "Markdown"))
        .unwrap();
    let styles = Array::from(&property(&bench, "styles"));
    assert_eq!(styles.length(), 1);
    assert_eq!(
        property(&property(&styles.get(0), "attributes"), "data-plugin")
            .as_string()
            .as_deref(),
        Some("@seekdeep-ai/seekdeep-client-ui-primitives")
    );
    let sources = [
        source(&[
            ("url", JsValue::from_str("https://example.com/a")),
            ("title", JsValue::from_str("Example A")),
            ("snippet", JsValue::from_str("Snippet")),
            ("publishedAt", JsValue::from_str("2026-01-01")),
        ]),
        source(&[("url", JsValue::from_str("http://sub.example.org/path"))]),
        source(&[("url", JsValue::from_str("javascript:alert(1)"))]),
        source(&[("url", JsValue::from_str("not a url"))]),
    ];
    let tree = webRender(
        &component,
        &object(&[
            ("kind", JsValue::from_str("search")),
            ("answer", JsValue::from_str("**answer**")),
            ("sources", array(&sources)),
            ("truncated", JsValue::TRUE),
            ("className", JsValue::from_str("caller")),
        ]),
    );
    assert_eq!(property(&tree, "kind").as_string().as_deref(), Some("div"));
    assert_eq!(
        property(&property(&tree, "props"), "data-web")
            .as_string()
            .as_deref(),
        Some("search")
    );
    assert!(
        property(&property(&tree, "props"), "className")
            .as_string()
            .unwrap()
            .contains("caller")
    );
    let markdown = webFindKind(&tree, "MarkdownText").get(0);
    assert_eq!(
        property(&property(&markdown, "props"), "text")
            .as_string()
            .as_deref(),
        Some("**answer**")
    );
    let links = webFindKind(&tree, "a");
    assert_eq!(links.length(), 2);
    for link in links.iter() {
        assert_eq!(
            property(&property(&link, "props"), "target")
                .as_string()
                .as_deref(),
            Some("_blank")
        );
        assert_eq!(
            property(&property(&link, "props"), "rel")
                .as_string()
                .as_deref(),
            Some("noopener noreferrer")
        );
    }
    assert_eq!(webText(&links.get(0)), "Example A");
    assert_eq!(webText(&links.get(1)), "sub.example.org");
    let plain = webFindClass(&tree, "webblock-sourceLink");
    assert_eq!(
        plain
            .iter()
            .filter(|node| property(node, "kind").as_string().as_deref() == Some("span"))
            .map(|node| webText(&node))
            .collect::<Vec<_>>(),
        ["javascript:alert(1)", "not a url"]
    );
    assert!(webText(&tree).contains("Snippet"));
    assert!(webText(&tree).contains("2026-01-01"));
    assert!(webText(&tree).contains("来源列表已截断"));
    let items = webFindKind(&tree, "li");
    assert_eq!(items.length(), 4);
    for (index, item) in items.iter().enumerate() {
        assert_eq!(
            property(&property(&item, "props"), "value").as_f64(),
            Some((index + 1).to_string().parse().unwrap())
        );
    }
}

#[wasm_bindgen_test]
fn search_empty_answer_only_and_source_only_arms_are_distinct() {
    let (_bench, component) = setup();
    let empty = webRender(
        &component,
        &object(&[
            ("kind", JsValue::from_str("search")),
            ("sources", array(&[])),
            ("truncated", JsValue::FALSE),
        ]),
    );
    assert!(webText(&empty).contains("未找到结果"));
    assert_eq!(webFindKind(&empty, "ol").length(), 0);

    let answer = webRender(
        &component,
        &object(&[
            ("kind", JsValue::from_str("search")),
            ("answer", JsValue::from_str("answer")),
            ("sources", array(&[])),
            ("truncated", JsValue::FALSE),
        ]),
    );
    assert_eq!(webFindKind(&answer, "ol").length(), 1);
    assert!(!webText(&answer).contains("未找到结果"));

    let sources = webRender(
        &component,
        &object(&[
            ("kind", JsValue::from_str("search")),
            (
                "sources",
                array(&[source(&[("url", JsValue::from_str("https://example.com"))])]),
            ),
            ("truncated", JsValue::FALSE),
        ]),
    );
    assert_eq!(webFindKind(&sources, "ol").length(), 1);
    assert!(!webText(&sources).contains("未找到结果"));
}

#[wasm_bindgen_test]
fn fetch_preserves_safe_link_plain_url_status_truncation_and_class() {
    let (_bench, component) = setup();
    let linked = webRender(
        &component,
        &object(&[
            ("kind", JsValue::from_str("fetch")),
            ("url", JsValue::from_str("https://example.com/final")),
            ("statusCode", JsValue::from_f64(206.0)),
            ("truncated", JsValue::TRUE),
            ("className", JsValue::from_str("caller")),
        ]),
    );
    assert_eq!(webFindKind(&linked, "a").length(), 1);
    assert!(webText(&linked).contains("HTTP 206"));
    assert!(webText(&linked).contains("内容已截断"));
    assert!(
        property(&property(&linked, "props"), "className")
            .as_string()
            .unwrap()
            .contains("caller")
    );
    let plain = webRender(
        &component,
        &object(&[
            ("kind", JsValue::from_str("fetch")),
            ("url", JsValue::from_str("file:///private/result")),
            ("statusCode", JsValue::from_f64(404.0)),
            ("truncated", JsValue::FALSE),
        ]),
    );
    assert_eq!(webFindKind(&plain, "a").length(), 0);
    assert!(webText(&plain).contains("file:///private/result"));
    assert!(webText(&plain).contains("HTTP 404"));
    assert!(!webText(&plain).contains("内容已截断"));
}
