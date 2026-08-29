//! Live WASM coverage for compiled Markdown rendering, security, math, and streaming.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Object, Reflect};
use seekdeep_client_ui_primitives::{
    configure_client_ui_primitive_code_block, configure_client_ui_primitive_highlight,
    configure_client_ui_primitive_markdown, markdown_text_component,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hooks = []
let cursor = 0
let styles = []
let texCalls = []
let texValue = ''
let texDisplay = false
let mentionOpens = []

function sameDeps(left, right) {
  return left?.length === right?.length && left.every((value, index) => Object.is(value, right[index]))
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
function list(values) { return { length: values.length, item(index) { return values[index] ?? null } } }
function textDom(value) { return { nodeType: 3, textContent: value } }
function elementDom(localName, attributes = {}, children = []) {
  return {
    nodeType: 1,
    localName,
    attributes: list(Object.entries(attributes).map(([name, value]) => ({ name, value }))),
    childNodes: list(children),
  }
}

export function installMarkdownBench() {
  hooks = []
  cursor = 0
  styles = []
  texCalls = []
  texValue = ''
  texDisplay = false
  mentionOpens = []
  globalThis.document = {
    head: { appendChild(node) { styles.push(node) } },
    createElement(kind) { return { kind, attributes: {}, setAttribute(name, value) { this.attributes[name] = value } } },
    querySelector(selector) {
      const match = selector.match(/data-plugin-css="([^"]+)"/)
      return match === null ? null : styles.find(style => style.attributes['data-plugin-css'] === match[1]) ?? null
    },
  }
  globalThis.DOMParser = class {
    parseFromString() {
      const annotation = elementDom('annotation', { encoding: 'application/x-tex' }, [textDom(texValue)])
      const math = elementDom('span', { class: 'katex', style: 'font-size: 1.2em' }, [annotation])
      const root = texDisplay ? elementDom('span', { class: 'katex-display' }, [math]) : math
      return { body: { childNodes: list([root]) } }
    }
  }
  const React = {
    Fragment: Symbol('Fragment'),
    createElement(kind, props, ...children) {
      return { kind, props: props ?? {}, children: children.flat(Infinity).filter(value => value !== null && value !== undefined && value !== false) }
    },
    memo(component) { return component },
    useRef(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { current: initial }
      return hooks[index]
    },
    useMemo(factory, dependencies) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].dependencies, dependencies)) {
        hooks[index] = { value: factory(), dependencies: [...dependencies] }
      }
      return hooks[index].value
    },
    useState(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { value: initial }
      return [hooks[index].value, update => { hooks[index].value = typeof update === 'function' ? update(hooks[index].value) : update }]
    },
    useCallback(callback) { cursor += 1; return callback },
    useSyncExternalStore(_subscribe, snapshot) { cursor += 1; return snapshot() },
  }
  const highlighter = {
    warm() {}, loadGrammar() { return Promise.resolve() },
    codeToHtml(code) { return '<pre class="shiki"><code>' + code + '</code></pre>' },
    codeToTokens(code) { return { tokens: String(code).split('\n').map(line => [{ content: line, color: 'var(--shiki-foreground)' }]) } },
  }
  const backend = {
    cssUrl: 'https://example.com/katex/katex.min.css',
    normalizeUri(value) { return encodeURI(value) },
    renderTex(value, options) {
      texCalls.push({ value, options: { ...options } })
      texValue = value
      texDisplay = options.displayMode === true
      if (value === 'internal' || (value === 'retry' && options.throwOnError === true)) throw new Error('tex failed')
      return '<ignored>'
    },
  }
  return { React, highlighter, backend }
}
export function mdRender(component, props) { cursor = 0; return component(props) }
export function mdUnmount() { hooks = [] }
export function mdObject(entries) { return Object.fromEntries(entries) }
export function mdText(tree) { return text(tree) }
export function mdFindKind(tree, kind) { return all(tree, node => node.kind === kind)[0] }
export function mdFindKinds(tree, kind) { return all(tree, node => node.kind === kind) }
export function mdFindClass(tree, className) { return all(tree, node => String(node.props?.className ?? '').split(/\s+/).includes(className))[0] }
export function mdFindFunction(tree) { return all(tree, node => typeof node.kind === 'function')[0] }
export function mdTexCalls() { return texCalls }
export function mdStyles() { return styles }
export function mdFileMentions() {
  return {
    resolve(value) {
      if (value !== 'index.html' && value !== 'out/index.html') return undefined
      return { open() { mentionOpens.push(value) }, label: 'Open out/index.html', title: 'out/index.html' }
    },
  }
}
export function mdMentionOpens() { return mentionOpens }
export function mdClick(node) { node.props.onClick() }
"#)]
extern "C" {
    fn installMarkdownBench() -> JsValue;
    fn mdRender(component: &JsValue, props: &JsValue) -> JsValue;
    fn mdUnmount();
    fn mdObject(entries: &Array) -> JsValue;
    fn mdText(tree: &JsValue) -> String;
    fn mdFindKind(tree: &JsValue, kind: &str) -> JsValue;
    fn mdFindKinds(tree: &JsValue, kind: &str) -> Array;
    fn mdFindClass(tree: &JsValue, class_name: &str) -> JsValue;
    fn mdFindFunction(tree: &JsValue) -> JsValue;
    fn mdTexCalls() -> Array;
    fn mdStyles() -> Array;
    fn mdFileMentions() -> JsValue;
    fn mdMentionOpens() -> Array;
    fn mdClick(node: &JsValue);
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn props(entries: &[(&str, JsValue)]) -> Object {
    let values = Array::new();
    for (key, value) in entries {
        values.push(&Array::of2(&JsValue::from_str(key), value));
    }
    mdObject(&values).unchecked_into()
}

fn setup() -> JsValue {
    let bench = installMarkdownBench();
    let react = property(&bench, "React");
    let backend = property(&bench, "backend");
    configure_client_ui_primitive_highlight(property(&bench, "highlighter")).unwrap();
    configure_client_ui_primitive_code_block(react.clone()).unwrap();
    configure_client_ui_primitive_markdown(react.clone(), backend.clone()).unwrap();
    configure_client_ui_primitive_markdown(react, backend).unwrap();
    markdown_text_component().unwrap()
}

fn render(component: &JsValue, props: &Object) -> JsValue {
    mdRender(component, props.as_ref())
}

#[wasm_bindgen_test]
fn semantic_gfm_links_images_tables_and_raw_html_match_policy() {
    let markdown = setup();
    assert_eq!(mdStyles().length(), 3);
    let source = [
        "# Heading",
        "",
        "Paragraph with **strong**, *emphasis*, ~~deleted~~, `inline`, [safe](https://example.com), [mail](mailto:dev@example.com), and [bad](/settings).",
        "",
        "- [x] done",
        "- [ ] pending",
        "",
        "| A | B |",
        "| :--- | ---: |",
        "| one | two |",
        "",
        "![remote](https://example.com/a.png) ![local](a.png)",
        "",
        "<script>unsafe()</script>",
    ]
    .join("\n");
    let tree = render(&markdown, &props(&[("text", JsValue::from_str(&source))]));
    assert_eq!(mdText(&mdFindKind(&tree, "h1")), "Heading");
    assert_eq!(mdText(&mdFindKind(&tree, "strong")), "strong");
    assert_eq!(mdText(&mdFindKind(&tree, "em")), "emphasis");
    assert_eq!(mdText(&mdFindKind(&tree, "del")), "deleted");
    assert_eq!(mdFindKinds(&tree, "input").length(), 2);
    assert!(!mdFindKind(&tree, "table").is_undefined());
    let links = mdFindKinds(&tree, "a");
    assert_eq!(links.length(), 2);
    assert_eq!(
        property(&property(&links.get(0), "props"), "target")
            .as_string()
            .as_deref(),
        Some("_blank")
    );
    assert!(property(&property(&links.get(1), "props"), "target").is_undefined());
    assert_eq!(mdFindKinds(&tree, "img").length(), 1);
    assert!(mdText(&tree).contains("local"));
    assert!(mdText(&tree).contains("<script>unsafe()</script>"));
    mdUnmount();
}

#[wasm_bindgen_test]
fn nested_tight_lists_keep_paragraphs_unwrapped() {
    let markdown = setup();
    let source = "- tight one\n- tight two\n  - child";
    let tree = render(&markdown, &props(&[("text", JsValue::from_str(source))]));
    let outer = mdFindKind(&tree, "ul");
    let items = Array::from(&property(&outer, "children"));
    assert_eq!(items.length(), 2);
    assert!(mdFindKind(&items.get(0), "p").is_undefined());
    assert!(mdFindKind(&items.get(1), "p").is_undefined());
    assert_eq!(mdText(&items.get(1)), "tight two\nchild\n");
    mdUnmount();
}

#[wasm_bindgen_test]
fn code_fences_math_retry_and_streaming_finalize_match_source() {
    let markdown = setup();
    let labels = props(&[
        ("copyLabel", JsValue::from_str("Copy code")),
        ("copiedLabel", JsValue::from_str("Copied")),
    ]);
    let source = "```ts meta=value\nconst answer = 42\n```\n\n$retry$ and $internal$";
    let settled = props(&[
        ("text", JsValue::from_str(source)),
        ("codeLabels", labels.clone().into()),
    ]);
    let tree = render(&markdown, &settled);
    let code = mdFindFunction(&tree);
    assert_eq!(
        property(&property(&code, "props"), "lang")
            .as_string()
            .as_deref(),
        Some("ts")
    );
    assert_eq!(
        property(&property(&code, "props"), "copyLabel")
            .as_string()
            .as_deref(),
        Some("Copy code")
    );
    assert_eq!(mdTexCalls().length(), 4);
    assert!(!mdFindClass(&tree, "katex").is_undefined());
    assert!(!mdFindClass(&tree, "katex-error").is_undefined());
    mdUnmount();

    let markdown = setup();
    let streaming = props(&[
        ("text", JsValue::from_str(source)),
        ("streaming", JsValue::TRUE),
        ("codeLabels", labels.into()),
    ]);
    let tree = render(&markdown, &streaming);
    assert_eq!(mdTexCalls().length(), 0);
    assert!(mdText(&tree).contains("$retry$"));
    assert!(property(&property(&mdFindFunction(&tree), "props"), "lang").is_undefined());
    let tree = render(&markdown, &settled);
    assert!(mdTexCalls().length() > 0);
    assert!(!mdFindClass(&tree, "katex").is_undefined());
    mdUnmount();
}

#[wasm_bindgen_test]
fn inline_urls_file_mentions_and_anchor_nesting_are_exact() {
    let markdown = setup();
    let mentions = mdFileMentions();
    let source = [
        "`https://example.com/preview?q=one%20two`",
        "",
        "`index.html` and `other.css`",
        "",
        "[see `out/index.html`](https://example.com/doc)",
    ]
    .join("\n");
    let tree = render(
        &markdown,
        &props(&[
            ("text", JsValue::from_str(&source)),
            ("fileMentions", mentions),
        ]),
    );
    assert_eq!(mdFindKinds(&tree, "a").length(), 2);
    let buttons = mdFindKinds(&tree, "button");
    assert_eq!(buttons.length(), 1);
    mdClick(&buttons.get(0));
    assert_eq!(
        mdMentionOpens().get(0).as_string().as_deref(),
        Some("index.html")
    );
    mdUnmount();

    let markdown = setup();
    let tree = render(
        &markdown,
        &props(&[
            ("text", JsValue::from_str("`index.html`\n\nmore\n\n")),
            ("streaming", JsValue::TRUE),
            ("fileMentions", mdFileMentions()),
        ]),
    );
    assert_eq!(mdFindKinds(&tree, "button").length(), 0);
    mdUnmount();
}

#[wasm_bindgen_test]
fn references_footnotes_and_streaming_divergence_recover() {
    let markdown = setup();
    let source = [
        "See [the link][ref] and note[^n] twice[^n].",
        "",
        "[ref]: https://example.com/target",
        "",
        "[^n]: Footnote body.",
    ]
    .join("\n");
    let tree = render(&markdown, &props(&[("text", JsValue::from_str(&source))]));
    assert_eq!(mdFindKinds(&tree, "a").length(), 1);
    let superscripts = mdFindKinds(&tree, "sup");
    assert!(superscripts.length() >= 3);
    let section = mdFindClass(&tree, "footnotes");
    assert!(mdText(&section).contains("Footnote body. ↩ ↩2"));
    mdUnmount();

    let markdown = setup();
    let first = props(&[
        ("text", JsValue::from_str("alpha\n\nbeta\n\ngamma\n\ndelta")),
        ("streaming", JsValue::TRUE),
    ]);
    assert!(mdText(&render(&markdown, &first)).contains("delta"));
    let divergent = props(&[
        (
            "text",
            JsValue::from_str("totally\n\ndifferent\n\ndocument"),
        ),
        ("streaming", JsValue::TRUE),
    ]);
    let tree = render(&markdown, &divergent);
    assert!(mdText(&tree).contains("totally"));
    assert!(!mdText(&tree).contains("alpha"));
    mdUnmount();
}

#[wasm_bindgen_test]
fn streaming_label_identity_resets_cached_fences() {
    let markdown = setup();
    let source = "```ts\nconst a = 1\n```\n\np1\n\np2\n\np3";
    let first_labels = props(&[("copyLabel", JsValue::from_str("Copy"))]);
    let first = props(&[
        ("text", JsValue::from_str(source)),
        ("streaming", JsValue::TRUE),
        ("codeLabels", first_labels.into()),
    ]);
    let tree = render(&markdown, &first);
    assert_eq!(
        property(&property(&mdFindFunction(&tree), "props"), "copyLabel")
            .as_string()
            .as_deref(),
        Some("Copy")
    );
    let next_labels = props(&[("copyLabel", JsValue::from_str("Kopieren"))]);
    let next = props(&[
        ("text", JsValue::from_str(source)),
        ("streaming", JsValue::TRUE),
        ("codeLabels", next_labels.into()),
    ]);
    let tree = render(&markdown, &next);
    assert_eq!(
        property(&property(&mdFindFunction(&tree), "props"), "copyLabel")
            .as_string()
            .as_deref(),
        Some("Kopieren")
    );
    mdUnmount();
}
