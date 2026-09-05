//! Live WASM coverage for context-form bodies and disclosure state.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_conversation::{
    catalog_body_component, configure_client_ui_conversation_context_bodies, context_body_browser,
    context_injection_row_component, instructions_body_component, notice_body_component,
    opaque_body_component, recall_body_component, relay_body_component, snapshot_body_component,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hooks = []
let cursor = 0
export function installContextBench() {
  hooks = []; cursor = 0
  globalThis.document = {
    head: { appendChild() {} }, createElement() { return { setAttribute() {} } }, querySelector() { return null },
  }
  const React = {
    Fragment: 'Fragment',
    createElement(kind, props, ...children) {
      if (kind === 'DisclosureRow') {
        const visible = [props?.collapsedContent]
        if (props?.open) visible.push(...children)
        return { kind, props: props ?? {}, children: visible }
      }
      return { kind, props: props ?? {}, children }
    },
    useState(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { value: initial }
      const set = update => { hooks[index].value = typeof update === 'function' ? update(hooks[index].value) : update }
      return [hooks[index].value, set]
    },
  }
  const uiPrimitives = { JsonBlock: 'JsonBlock', DisclosureRow: 'DisclosureRow', IconBrowseOutline16: 'IconBrowseOutline16' }
  return { React, uiPrimitives }
}
export function contextObject(entries) { return Object.fromEntries(entries) }
export function makeContextTranslate() {
  return (key, vars) => ({
    'message.contextInjection': '上下文注入', 'message.contextRecall': '跨会话召回',
    'message.unknownBlock': '未知内容块',
    'json.truncated': `… 已截断，共 ${vars?.total} 字符`,
    'message.context.instructions.removed': '已移除',
    'message.context.instructions.loaded': '已载入',
    'message.context.instructions.added': '已新增',
    'message.context.instructions.updated': '已更新',
    'message.context.catalog.replaced': '替换目录',
    'message.context.catalog.more': `…还有 ${vars?.count} 条`,
    'message.context.snapshot.supersedes': '取代先前的快照',
    'message.context.relay.from': `来自会话 ${vars?.session}`,
    'message.context.recall.counts': `保留 ${vars?.retained} 条 · 省略 ${vars?.omitted} 条`,
    'message.context.recall.truncated': '已截断',
  })[key] ?? key
}
export function contextRender(component, props) { cursor = 0; return component(props) }
export function contextText(value) {
  if (value === null || value === undefined || typeof value === 'boolean') return ''
  if (typeof value === 'string' || typeof value === 'number') return String(value)
  if (Array.isArray(value)) return value.map(contextText).join('')
  if (value.kind === 'JsonBlock') return value.props?.label ?? ''
  return contextText(value.children)
}
export function contextFindMarker(value, marker) {
  if (value === null || value === undefined || typeof value !== 'object') return undefined
  if (Object.prototype.hasOwnProperty.call(value.props ?? {}, marker)) return value
  for (const child of value.children ?? []) { const found = contextFindMarker(child, marker); if (found) return found }
  return undefined
}
export function contextFindAllMarker(value, marker) {
  if (value === null || value === undefined || typeof value !== 'object') return []
  const own = Object.prototype.hasOwnProperty.call(value.props ?? {}, marker) ? [value] : []
  return own.concat(...(value.children ?? []).map(child => contextFindAllMarker(child, marker)))
}
export function contextFindAllKind(value, kind) {
  if (value === null || value === undefined || typeof value !== 'object') return []
  const own = value.kind === kind ? [value] : []
  return own.concat(...(value.children ?? []).map(child => contextFindAllKind(child, kind)))
}
export function contextFindAllClass(value, className) {
  if (value === null || value === undefined || typeof value !== 'object') return []
  const own = String(value.props?.className ?? '').split(/\s+/).includes(className) ? [value] : []
  return own.concat(...(value.children ?? []).map(child => contextFindAllClass(child, className)))
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = installContextBench)]
    fn install_context_bench() -> JsValue;
    #[wasm_bindgen(js_name = contextObject)]
    fn context_object(entries: &Array) -> JsValue;
    #[wasm_bindgen(js_name = makeContextTranslate)]
    fn make_context_translate() -> Function;
    #[wasm_bindgen(js_name = contextRender)]
    fn context_render(component: &JsValue, props: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = contextText)]
    fn context_text(value: &JsValue) -> String;
    #[wasm_bindgen(js_name = contextFindMarker)]
    fn context_find_marker(value: &JsValue, marker: &str) -> JsValue;
    #[wasm_bindgen(js_name = contextFindAllMarker)]
    fn context_find_all_marker(value: &JsValue, marker: &str) -> Array;
    #[wasm_bindgen(js_name = contextFindAllKind)]
    fn context_find_all_kind(value: &JsValue, kind: &str) -> Array;
    #[wasm_bindgen(js_name = contextFindAllClass)]
    fn context_find_all_class(value: &JsValue, class_name: &str) -> Array;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn object(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    context_object(&array).unchecked_into()
}

fn text_block(text: &str) -> Object {
    object(&[
        ("type", JsValue::from_str("text")),
        ("text", JsValue::from_str(text)),
    ])
}

fn content(blocks: &[JsValue]) -> Array {
    blocks.iter().collect()
}

fn body_props(content: &Array, source: JsValue) -> Object {
    object(&[
        ("content", content.clone().into()),
        ("source", source),
        ("t", make_context_translate().into()),
    ])
}

fn row_props(
    content: &Array,
    source: JsValue,
    role: &str,
    label: JsValue,
    form: JsValue,
) -> Object {
    object(&[
        ("content", content.clone().into()),
        ("source", source),
        (
            "provenance",
            object(&[("role", JsValue::from_str(role)), ("label", label)]).into(),
        ),
        ("form", form),
        ("t", make_context_translate().into()),
    ])
}

fn setup() -> JsValue {
    let bench = install_context_bench();
    configure_client_ui_conversation_context_bodies(
        property(&bench, "React"),
        property(&bench, "uiPrimitives"),
    )
    .unwrap();
    context_injection_row_component().unwrap()
}

fn resolved(form: &str, content: &Array, source: JsValue) -> JsValue {
    context_body_browser(JsValue::from_str(form), body_props(content, source).into()).unwrap()
}

#[wasm_bindgen_test]
fn opaque_disclosure_preserves_order_fields_bounds_and_toggle_state() {
    let row = setup();
    let blocks = content(&[text_block("line one\n\nline two").into()]);
    let source = object(&[
        ("kind", JsValue::from_str("plugin")),
        ("plugin", JsValue::from_str("fixture")),
        ("empty", object(&[]).into()),
        ("list", Array::new().into()),
    ]);
    let props = row_props(
        &blocks,
        source.into(),
        "inject",
        JsValue::from_str("fixture"),
        JsValue::NULL,
    );
    let closed = context_render(&row, props.as_ref());
    assert_eq!(
        property(&closed, "kind").as_string().as_deref(),
        Some("DisclosureRow")
    );
    assert_eq!(
        property(&property(&closed, "props"), "title")
            .as_string()
            .as_deref(),
        Some("上下文注入")
    );
    assert_eq!(
        property(&property(&closed, "props"), "open").as_bool(),
        Some(false)
    );
    let disclosure_props = property(&closed, "props");
    let icon = property(&disclosure_props, "icon");
    assert_eq!(
        property(&icon, "kind").as_string().as_deref(),
        Some("IconBrowseOutline16")
    );
    assert_eq!(
        property(&property(&icon, "props"), "size").as_f64(),
        Some(14.0)
    );
    assert_eq!(
        property(&disclosure_props, "chevronClassName")
            .as_string()
            .as_deref(),
        Some("seekdeep-conversation-contextRow-chevron")
    );
    for key in ["keepContentWhenOpen", "expandable", "expandOnRowClick"] {
        assert_eq!(property(&disclosure_props, key).as_bool(), Some(true));
    }
    assert!(context_find_marker(&closed, "data-context-injection-body").is_undefined());
    property(&property(&closed, "props"), "onToggle")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    let open = context_render(&row, props.as_ref());
    assert_eq!(
        property(&property(&open, "props"), "open").as_bool(),
        Some(true)
    );
    assert_eq!(
        context_text(&context_find_marker(&open, "data-context-text")),
        "line one\n\nline two"
    );
    assert!(
        property(
            &property(
                &context_find_marker(&open, "data-context-injection-body"),
                "props"
            ),
            "data-context-form"
        )
        .is_undefined()
    );
    let field_keys =
        context_find_all_kind(&context_find_marker(&open, "data-context-fields"), "dt")
            .iter()
            .map(|node| context_text(&node))
            .collect::<Vec<_>>();
    assert_eq!(field_keys, ["plugin", "empty", "list"]);
}

#[wasm_bindgen_test]
fn opaque_content_preserves_run_order_unknown_blocks_and_bounded_fields() {
    setup();
    let interleaved = content(&[
        text_block("before").into(),
        object(&[
            ("type", JsValue::from_str("future-block")),
            ("payload", JsValue::from_f64(1.0)),
        ])
        .into(),
        text_block("after").into(),
        text_block(" joined").into(),
    ]);
    let resolved = context_body_browser(
        JsValue::NULL,
        body_props(&interleaved, JsValue::NULL).into(),
    )
    .unwrap();
    let body = property(&resolved, "body");
    let texts = context_find_all_marker(&body, "data-context-text")
        .iter()
        .map(|node| context_text(&node))
        .collect::<Vec<_>>();
    assert_eq!(texts, ["before", "after joined"]);
    assert_eq!(context_find_all_kind(&body, "JsonBlock").length(), 1);

    let long_source = object(&[
        ("kind", JsValue::from_str("plugin")),
        ("note", JsValue::from_str(&"y".repeat(21_000))),
        ("form", JsValue::from_str("future")),
    ]);
    let resolved = context_body_browser(
        JsValue::NULL,
        body_props(&content(&[text_block("short").into()]), long_source.into()).into(),
    )
    .unwrap();
    let body = property(&resolved, "body");
    let field_values =
        context_find_all_kind(&context_find_marker(&body, "data-context-fields"), "dd");
    assert!(context_text(&field_values.get(0)).ends_with("… 已截断，共 21000 字符"));
    let keys = context_find_all_kind(&context_find_marker(&body, "data-context-fields"), "dt")
        .iter()
        .map(|node| context_text(&node))
        .collect::<Vec<_>>();
    assert_eq!(keys, ["note", "form"]);
}

#[wasm_bindgen_test]
fn instructions_are_deduplicated_and_labeled_by_baseline_or_delta() {
    setup();
    let changes = Array::of3(
        object(&[
            ("action", JsValue::from_str("set")),
            ("path", JsValue::from_str("AGENTS.md")),
            ("digest", JsValue::from_str("abc")),
        ])
        .as_ref(),
        object(&[
            ("action", JsValue::from_str("remove")),
            ("path", JsValue::from_str("sub/AGENTS.md")),
        ])
        .as_ref(),
        object(&[
            ("action", JsValue::from_str("replace")),
            ("path", JsValue::from_str("AGENTS.md")),
        ])
        .as_ref(),
    );
    let source = object(&[
        ("kind", JsValue::from_str("agent-instructions")),
        ("form", JsValue::from_str("instructions")),
        ("baseline", JsValue::TRUE),
        ("changes", changes.into()),
    ]);
    let blocks = content(&[text_block("<system-reminder>\ntext\n</system-reminder>").into()]);
    let value = resolved("instructions", &blocks, source.into());
    assert_eq!(
        property(&value, "rendered").as_string().as_deref(),
        Some("instructions")
    );
    let body = property(&value, "body");
    let files = context_find_all_kind(&context_find_marker(&body, "data-context-files"), "li")
        .iter()
        .map(|node| context_text(&node))
        .collect::<Vec<_>>();
    assert_eq!(files, ["AGENTS.md已载入", "sub/AGENTS.md已移除"]);
    assert_eq!(
        property(
            &property(&context_find_all_kind(&body, "li").get(0), "props"),
            "title"
        )
        .as_string()
        .as_deref(),
        Some("abc")
    );
    assert!(context_text(&body).contains("<system-reminder>"));

    let delta = object(&[
        ("kind", JsValue::from_str("agent-instructions")),
        ("form", JsValue::from_str("instructions")),
        (
            "changes",
            Array::of2(
                object(&[
                    ("action", JsValue::from_str("set")),
                    ("path", JsValue::from_str("new/AGENTS.md")),
                ])
                .as_ref(),
                object(&[
                    ("action", JsValue::from_str("replace")),
                    ("path", JsValue::from_str("old/AGENTS.md")),
                ])
                .as_ref(),
            )
            .into(),
        ),
    ]);
    let value = resolved(
        "instructions",
        &content(&[text_block("delta").into()]),
        delta.into(),
    );
    let files = context_find_all_kind(
        &context_find_marker(&property(&value, "body"), "data-context-files"),
        "li",
    )
    .iter()
    .map(|node| context_text(&node))
    .collect::<Vec<_>>();
    assert_eq!(files, ["new/AGENTS.md已新增", "old/AGENTS.md已更新"]);
}

#[wasm_bindgen_test]
fn malformed_instruction_changes_fall_back_whole_with_source_fields() {
    setup();
    for changes in [
        Array::of1(object(&[("action", JsValue::from_str("set"))]).as_ref()),
        Array::of1(
            object(&[
                ("action", JsValue::from_str("merge")),
                ("path", JsValue::from_str("AGENTS.md")),
            ])
            .as_ref(),
        ),
    ] {
        let malformed = object(&[
            ("kind", JsValue::from_str("agent-instructions")),
            ("form", JsValue::from_str("instructions")),
            ("changes", changes.into()),
        ]);
        let value = resolved(
            "instructions",
            &content(&[text_block("instruction prose").into()]),
            malformed.into(),
        );
        assert!(property(&value, "rendered").is_null());
        assert_eq!(
            context_text(&context_find_marker(
                &property(&value, "body"),
                "data-context-text"
            )),
            "instruction prose"
        );
        assert!(
            !context_find_marker(&property(&value, "body"), "data-context-fields").is_undefined()
        );
    }
}

#[wasm_bindgen_test]
fn catalog_replaces_text_bounds_rows_keeps_unknown_blocks_and_falls_back_whole() {
    setup();
    let entries = Array::new();
    for index in 0..205 {
        entries.push(
            object(&[
                ("name", JsValue::from_str(&format!("s-{index}"))),
                ("description", JsValue::from_str("d")),
            ])
            .as_ref(),
        );
    }
    let source = object(&[
        ("kind", JsValue::from_str("skill-catalog")),
        ("form", JsValue::from_str("catalog")),
        ("update", JsValue::TRUE),
        ("entries", entries.into()),
    ]);
    let blocks = content(&[
        text_block("catalog prose").into(),
        object(&[("type", JsValue::from_str("future-block"))]).into(),
    ]);
    let value = resolved("catalog", &blocks, source.into());
    assert_eq!(
        property(&value, "rendered").as_string().as_deref(),
        Some("catalog")
    );
    let body = property(&value, "body");
    assert_eq!(
        context_text(&context_find_marker(&body, "data-context-catalog-update")),
        "替换目录"
    );
    assert_eq!(
        context_find_all_kind(&context_find_marker(&body, "data-context-entries"), "li").length(),
        200
    );
    assert_eq!(
        context_text(&context_find_marker(
            &body,
            "data-context-entries-truncated"
        )),
        "…还有 5 条"
    );
    assert!(context_find_marker(&body, "data-context-text").is_undefined());
    assert_eq!(context_find_all_kind(&body, "JsonBlock").length(), 1);

    let empty = object(&[
        ("kind", JsValue::from_str("skill-catalog")),
        ("form", JsValue::from_str("catalog")),
        ("update", JsValue::TRUE),
        ("entries", Array::new().into()),
    ]);
    let value = resolved(
        "catalog",
        &content(&[text_block("prose").into()]),
        empty.into(),
    );
    assert_eq!(
        property(&value, "rendered").as_string().as_deref(),
        Some("catalog")
    );
    assert_eq!(
        context_find_all_kind(
            &context_find_marker(&property(&value, "body"), "data-context-entries"),
            "li",
        )
        .length(),
        0
    );

    for entries in [
        JsValue::from_str("not-a-list"),
        Array::of2(
            object(&[
                ("name", JsValue::from_str("a")),
                ("description", JsValue::from_str("A")),
            ])
            .as_ref(),
            object(&[("name", JsValue::from_str("b"))]).as_ref(),
        )
        .into(),
    ] {
        let source = object(&[
            ("kind", JsValue::from_str("skill-catalog")),
            ("form", JsValue::from_str("catalog")),
            ("entries", entries),
        ]);
        let value = resolved(
            "catalog",
            &content(&[text_block("catalog prose").into()]),
            source.into(),
        );
        assert!(property(&value, "rendered").is_null());
        assert_eq!(
            context_text(&context_find_marker(
                &property(&value, "body"),
                "data-context-text"
            )),
            "catalog prose"
        );
    }
}

#[wasm_bindgen_test]
fn snapshot_and_notice_render_attributed_or_collapsed_accounts() {
    let row = setup();
    let snapshot = object(&[
        ("kind", JsValue::from_str("plugin")),
        ("form", JsValue::from_str("snapshot")),
        (
            "sections",
            Array::of2(
                object(&[
                    ("name", JsValue::from_str("sandbox:policy")),
                    ("text", JsValue::from_str("workspace-write")),
                ])
                .as_ref(),
                object(&[
                    ("name", JsValue::from_str("workspace")),
                    ("text", JsValue::from_str("/repo")),
                ])
                .as_ref(),
            )
            .into(),
        ),
    ]);
    let value = resolved(
        "snapshot",
        &content(&[text_block("Current runtime context.").into()]),
        snapshot.into(),
    );
    let body = property(&value, "body");
    assert_eq!(
        context_text(&context_find_marker(
            &body,
            "data-context-snapshot-supersedes"
        )),
        "取代先前的快照"
    );
    let sections =
        context_find_all_kind(&context_find_marker(&body, "data-context-sections"), "div")
            .iter()
            .map(|node| context_text(&node))
            .collect::<Vec<_>>();
    assert_eq!(
        sections,
        ["sandbox:policyworkspace-write", "workspace/repo"]
    );

    let notice_source = object(&[
        ("kind", JsValue::from_str("plugin")),
        ("form", JsValue::from_str("notice")),
        (
            "summary",
            JsValue::from_str("bash pnpm test [status: completed]"),
        ),
    ]);
    let notice_props = row_props(
        &content(&[text_block("background job finished").into()]),
        notice_source.into(),
        "inject",
        JsValue::from_str("tool-jobs"),
        JsValue::from_str("notice"),
    );
    let closed = context_render(&row, notice_props.as_ref());
    assert_eq!(
        context_text(&context_find_marker(&closed, "data-context-summary")),
        "bash pnpm test [status: completed]"
    );
    assert!(context_find_marker(&closed, "data-context-injection-body").is_undefined());
    property(&property(&closed, "props"), "onToggle")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    let open = context_render(&row, notice_props.as_ref());
    assert_eq!(
        property(
            &property(
                &context_find_marker(&open, "data-context-injection-body"),
                "props"
            ),
            "data-context-form"
        )
        .as_string()
        .as_deref(),
        Some("notice")
    );
}

#[wasm_bindgen_test]
fn relay_and_recall_report_sender_and_completeness() {
    setup();
    let relay = object(&[
        ("kind", JsValue::from_str("subagent-report")),
        ("form", JsValue::from_str("relay")),
        ("senderSessionId", JsValue::from_str("child-7")),
    ]);
    let value = resolved(
        "relay",
        &content(&[text_block("child report body").into()]),
        relay.into(),
    );
    assert_eq!(
        context_text(&context_find_marker(
            &property(&value, "body"),
            "data-context-relay-sender"
        )),
        "来自会话 child-7"
    );

    let references = Array::of2(
        object(&[
            ("label", JsValue::from_str("重构 loader")),
            ("retainedMessages", JsValue::from_f64(18.0)),
            ("omittedMessages", JsValue::from_f64(42.0)),
            ("truncated", JsValue::TRUE),
        ])
        .as_ref(),
        object(&[
            ("label", JsValue::from_str("修 CI")),
            ("retainedMessages", JsValue::from_f64(3.0)),
            ("omittedMessages", JsValue::from_f64(0.0)),
            ("truncated", JsValue::FALSE),
        ])
        .as_ref(),
    );
    let recall = object(&[
        ("kind", JsValue::from_str("session-reference")),
        ("form", JsValue::from_str("recall")),
        ("references", references.into()),
    ]);
    let value = resolved(
        "recall",
        &content(&[text_block("recalled material").into()]),
        recall.into(),
    );
    let rows = context_find_all_kind(
        &context_find_marker(&property(&value, "body"), "data-context-recalls"),
        "li",
    )
    .iter()
    .map(|node| context_text(&node))
    .collect::<Vec<_>>();
    assert_eq!(
        rows,
        [
            "重构 loader保留 18 条 · 省略 42 条已截断",
            "修 CI保留 3 条 · 省略 0 条"
        ]
    );
}

#[wasm_bindgen_test]
fn unreadable_snapshot_notice_relay_and_recall_fall_back_to_opaque() {
    setup();
    for (form, source) in [
        (
            "snapshot",
            object(&[("sections", JsValue::from_str("not-a-list"))]),
        ),
        ("notice", object(&[])),
        ("relay", object(&[])),
        (
            "recall",
            object(&[(
                "references",
                Array::of1(object(&[("label", JsValue::from_str("x"))]).as_ref()).into(),
            )]),
        ),
    ] {
        let value = resolved(
            form,
            &content(&[text_block(&format!("{form} prose")).into()]),
            source.into(),
        );
        assert!(property(&value, "rendered").is_null());
        assert_eq!(
            context_text(&context_find_marker(
                &property(&value, "body"),
                "data-context-text"
            )),
            format!("{form} prose")
        );
    }
}

#[wasm_bindgen_test]
fn exports_recall_title_utf16_bound_and_closed_form_failure_are_preserved() {
    let row = setup();
    for component in [
        opaque_body_component().unwrap(),
        instructions_body_component().unwrap(),
        catalog_body_component().unwrap(),
        snapshot_body_component().unwrap(),
        notice_body_component().unwrap(),
        relay_body_component().unwrap(),
        recall_body_component().unwrap(),
    ] {
        assert!(component.is_function());
    }
    let recall_props = row_props(
        &content(&[text_block("x").into()]),
        JsValue::NULL,
        "recall",
        JsValue::from_str("prior session"),
        JsValue::NULL,
    );
    let tree = context_render(&row, recall_props.as_ref());
    assert_eq!(
        property(&property(&tree, "props"), "title")
            .as_string()
            .as_deref(),
        Some("跨会话召回")
    );

    let oversized = format!("{}x", "😀".repeat(10_000));
    let value = context_body_browser(
        JsValue::NULL,
        body_props(&content(&[text_block(&oversized).into()]), JsValue::NULL).into(),
    )
    .unwrap();
    let text = context_text(&context_find_marker(
        &property(&value, "body"),
        "data-context-text",
    ));
    assert!(text.ends_with("… 已截断，共 20001 字符"));
    assert!(text.starts_with(&"😀".repeat(10_000)));

    let error = context_body_browser(
        JsValue::from_str("future"),
        body_props(&content(&[]), JsValue::NULL).into(),
    )
    .unwrap_err();
    assert_eq!(
        property(&error, "message").as_string().as_deref(),
        Some("unreachable context form: future")
    );
    assert_eq!(
        context_find_all_class(&tree, "seekdeep-conversation-contextRow-source").length(),
        1
    );
    let no_label = context_render(
        &row,
        row_props(
            &content(&[text_block("x").into()]),
            JsValue::NULL,
            "inject",
            JsValue::NULL,
            JsValue::NULL,
        )
        .as_ref(),
    );
    assert!(property(&property(&no_label, "props"), "collapsedContent").is_undefined());
}
