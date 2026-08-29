//! Live WASM coverage for simple message branches and retry countdown ownership.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_ui_conversation::{
    compaction_node_view_component, configure_client_ui_conversation_message_item,
    context_message_node_view_component, pending_steering_bubble_component,
    retry_node_view_component, retry_seconds_browser, turn_error_node_view_component,
    turn_max_tokens_node_view_component, unknown_node_view_component,
    user_message_node_view_component,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hooks = []
let cursor = 0
let now = 10000
let nextTimer = 1
let timers = new Map()
function sameDeps(left, right) { return left?.length === right?.length && left.every((value, index) => Object.is(value, right[index])) }
export function installMessageBench() {
  hooks = []; cursor = 0; now = 10000; nextTimer = 1; timers = new Map()
  Date.now = () => now
  globalThis.window = globalThis
  globalThis.setInterval = (callback, delay) => {
    const id = nextTimer++
    timers.set(id, { callback, delay, due: now + delay })
    return id
  }
  globalThis.clearInterval = id => { timers.delete(id) }
  globalThis.document = {
    head: { appendChild() {} }, createElement() { return { setAttribute() {} } }, querySelector() { return null },
  }
  const React = {
    Fragment: 'Fragment',
    createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } },
    memo(component) { component.memoized = true; return component },
    useMemo(factory, deps) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].deps, deps)) hooks[index] = { type: 'memo', deps: [...deps], value: factory() }
      return hooks[index].value
    },
    useState(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { type: 'state', value: typeof initial === 'function' ? initial() : initial }
      const set = update => { hooks[index].value = typeof update === 'function' ? update(hooks[index].value) : update }
      return [hooks[index].value, set]
    },
    useEffect(effect, deps) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].deps, deps)) {
        hooks[index]?.cleanup?.()
        hooks[index] = { type: 'effect', deps: [...deps], cleanup: effect() }
      }
    },
  }
  const uiPrimitives = { JsonBlock: 'JsonBlock', MessageText: 'MessageText', StateDot: 'StateDot' }
  const uiAttachment = { ImageGallery: 'ImageGallery' }
  const dependencies = { MessageIconActions: 'MessageIconActions', CompactionItem: 'CompactionItem', ContextInjectionRow: 'ContextInjectionRow' }
  return { React, uiPrimitives, uiAttachment, dependencies }
}
export function messageObject(entries) { return Object.fromEntries(entries) }
export function messageRender(component, props) { cursor = 0; return component(props) }
export function messageUnmount() { for (const hook of [...hooks].reverse()) hook?.cleanup?.(); hooks = [] }
export function messageAdvance(milliseconds) {
  const target = now + milliseconds
  while (true) {
    let due = Infinity
    for (const timer of timers.values()) due = Math.min(due, timer.due)
    if (due > target) break
    now = due
    for (const [id, timer] of [...timers]) {
      if (timer.due !== due) continue
      timer.callback()
      if (timers.has(id)) timer.due += timer.delay
    }
  }
  now = target
}
export function messageTimerCount() { return timers.size }
export function makeMessageTranslate() {
  return (key, vars) => ({
    'message.extraBlock': '附加内容块', 'json.truncated': `… 已截断，共 ${vars?.total} 字符`,
    'image.serviceUnavailable': '图片服务不可用', 'image.label': '图片', 'image.openOriginal': '查看原图',
    'image.openOriginalLabel': `查看原图：${vars?.label}`, 'image.loading': '加载中', 'image.loadFailed': '加载失败',
    'image.preview': '图片预览', 'image.closePreview': '关闭预览',
    'message.retry.active': '正在重试模型请求', 'message.retry.cancelled': '模型请求重试已取消',
    'message.retry.started': '已重试模型请求', 'message.retry.scheduled': '模型请求将重试',
    'message.retry.status': `${vars?.label}（${vars?.retry}/${vars?.maximum}） · ${vars?.seconds}s`,
    'message.retry.delay': '重试延迟：', 'message.retry.failure': '失败原因：',
    'message.turnError': '请求失败', 'message.maxTokens': '已达到最大输出长度',
    'message.maxTokens.hint': '回答可能不完整',
    'message.unknownSurface': `未知 surface 事件：${vars?.type}`,
  })[key] ?? key
}
export function messageText(value) {
  if (value === null || value === undefined || typeof value === 'boolean') return ''
  if (typeof value === 'string' || typeof value === 'number') return String(value)
  if (Array.isArray(value)) return value.map(messageText).join('')
  if (value.kind === 'MessageText') return value.props?.text ?? ''
  if (value.kind === 'JsonBlock') return value.props?.label ?? ''
  return messageText(value.children)
}
export function messageFindKind(value, kind) {
  if (value === null || value === undefined || typeof value !== 'object') return undefined
  if (value.kind === kind) return value
  for (const child of value.children ?? []) { const found = messageFindKind(child, kind); if (found) return found }
  return undefined
}
export function messageFindAllKind(value, kind) {
  if (value === null || value === undefined || typeof value !== 'object') return []
  const own = value.kind === kind ? [value] : []
  return own.concat(...(value.children ?? []).map(child => messageFindAllKind(child, kind)))
}
export function messageFindAllClass(value, className) {
  if (value === null || value === undefined || typeof value !== 'object') return []
  const own = String(value.props?.className ?? '').split(/\s+/).includes(className) ? [value] : []
  return own.concat(...(value.children ?? []).map(child => messageFindAllClass(child, className)))
}
export function messageIdentity(value) { return value }
"#)]
extern "C" {
    #[wasm_bindgen(js_name = installMessageBench)]
    fn install_message_bench() -> JsValue;
    #[wasm_bindgen(js_name = messageObject)]
    fn message_object(entries: &Array) -> JsValue;
    #[wasm_bindgen(js_name = messageRender)]
    fn message_render(component: &JsValue, props: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = messageUnmount)]
    fn message_unmount();
    #[wasm_bindgen(js_name = messageAdvance)]
    fn message_advance(milliseconds: f64);
    #[wasm_bindgen(js_name = messageTimerCount)]
    fn message_timer_count() -> u32;
    #[wasm_bindgen(js_name = makeMessageTranslate)]
    fn make_message_translate() -> Function;
    #[wasm_bindgen(js_name = messageText)]
    fn message_text(value: &JsValue) -> String;
    #[wasm_bindgen(js_name = messageFindKind)]
    fn message_find_kind(value: &JsValue, kind: &str) -> JsValue;
    #[wasm_bindgen(js_name = messageFindAllKind)]
    fn message_find_all_kind(value: &JsValue, kind: &str) -> Array;
    #[wasm_bindgen(js_name = messageFindAllClass)]
    fn message_find_all_class(value: &JsValue, class_name: &str) -> Array;
    #[wasm_bindgen(js_name = messageIdentity)]
    fn message_identity(value: &JsValue) -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn object(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    message_object(&array).unchecked_into()
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

fn view_node(data: JsValue) -> Object {
    object(&[("data", data)])
}

fn props(node: Object) -> Object {
    object(&[
        ("node", node.into()),
        ("loadImage", message_identity(&JsValue::from_str("loader"))),
        ("t", make_message_translate().into()),
    ])
}

fn setup() {
    let bench = install_message_bench();
    configure_client_ui_conversation_message_item(
        property(&bench, "React"),
        property(&bench, "uiPrimitives"),
        property(&bench, "uiAttachment"),
        property(&bench, "dependencies"),
    )
    .unwrap();
}

fn retry_node(seq: f64, state: &str, mode: &str, retry: f64, delay: f64, message: &str) -> Object {
    let mut entries = vec![
        ("seq", JsValue::from_f64(seq)),
        ("retryState", JsValue::from_str(state)),
        ("mode", JsValue::from_str(mode)),
        ("retry", JsValue::from_f64(retry)),
        ("delayMs", JsValue::from_f64(delay)),
        (
            "failure",
            object(&[("message", JsValue::from_str(message))]).into(),
        ),
    ];
    if mode == "normal" {
        entries.push(("maxRetries", JsValue::from_f64(2.0)));
    }
    object(&entries)
}

#[wasm_bindgen_test]
fn user_partition_reference_projection_images_and_actions_match_source() {
    setup();
    let attachment = object(&[("id", JsValue::from_str("image-1"))]);
    let blocks = content(&[
        text_block("hello ").into(),
        text_block("/skill @agent bye").into(),
        object(&[
            ("type", JsValue::from_str("image")),
            ("attachment", attachment.clone().into()),
        ])
        .into(),
        object(&[
            ("type", JsValue::from_str("future")),
            ("value", JsValue::from_f64(1.0)),
        ])
        .into(),
    ]);
    let data = object(&[
        ("content", blocks.into()),
        ("time", JsValue::from_f64(12_345.0)),
    ]);
    let component = user_message_node_view_component().unwrap();
    let tree = message_render(&component, props(view_node(data.into())).as_ref());
    assert_eq!(property(&component, "memoized").as_bool(), Some(true));
    let gallery = message_find_kind(&tree, "ImageGallery");
    let images = property(&property(&gallery, "props"), "images").unchecked_into::<Array>();
    assert_eq!(images.length(), 1);
    assert!(Object::is(
        &property(&images.get(0), "attachment"),
        attachment.as_ref()
    ));
    assert_eq!(
        property(&property(&gallery, "props"), "align")
            .as_string()
            .as_deref(),
        Some("end")
    );
    assert_eq!(
        property(&property(&gallery, "props"), "load")
            .as_string()
            .as_deref(),
        Some("loader")
    );
    assert_eq!(
        property(&property(&property(&gallery, "props"), "labels"), "image")
            .as_string()
            .as_deref(),
        Some("图片")
    );
    let chips = message_find_all_class(&tree, "seekdeep-conversation-message-refChip");
    assert_eq!(chips.length(), 2);
    assert_eq!(message_text(&chips.get(0)), "/skill");
    assert_eq!(
        property(&property(&chips.get(0), "props"), "data-ref-chip")
            .as_string()
            .as_deref(),
        Some("skill")
    );
    assert_eq!(message_text(&chips.get(1)), "@agent");
    assert_eq!(
        property(&property(&chips.get(1), "props"), "data-ref-chip")
            .as_string()
            .as_deref(),
        Some("subagent")
    );
    assert!(message_text(&tree).contains("hello /skill @agent bye"));
    let extra = message_find_kind(&tree, "JsonBlock");
    assert_eq!(
        property(&property(&extra, "props"), "label")
            .as_string()
            .as_deref(),
        Some("附加内容块")
    );
    let actions = message_find_kind(&tree, "MessageIconActions");
    let action_props = property(&actions, "props");
    assert_eq!(
        property(&action_props, "text").as_string().as_deref(),
        Some("hello /skill @agent bye")
    );
    assert_eq!(property(&action_props, "time").as_f64(), Some(12_345.0));
    assert_eq!(
        property(&action_props, "clock").as_string().as_deref(),
        Some("start")
    );
    assert!(property(&action_props, "className").is_undefined());
    assert!(property(&property(&tree, "props"), "data-pending-steering").is_undefined());
}

#[wasm_bindgen_test]
fn export_family_preserves_pending_raw_and_seven_memoized_view_identities() {
    setup();
    assert!(property(&pending_steering_bubble_component().unwrap(), "memoized").is_undefined());
    for component in [
        user_message_node_view_component().unwrap(),
        context_message_node_view_component().unwrap(),
        compaction_node_view_component().unwrap(),
        retry_node_view_component().unwrap(),
        turn_error_node_view_component().unwrap(),
        turn_max_tokens_node_view_component().unwrap(),
        unknown_node_view_component().unwrap(),
    ] {
        assert_eq!(property(&component, "memoized").as_bool(), Some(true));
    }
}

#[wasm_bindgen_test(async)]
async fn pending_steering_marks_projection_and_supplies_rejecting_image_loader() {
    setup();
    let component = pending_steering_bubble_component().unwrap();
    let props = object(&[
        ("content", Array::new().into()),
        ("t", make_message_translate().into()),
    ]);
    let tree = message_render(&component, props.as_ref());
    assert_eq!(
        property(&property(&tree, "props"), "data-pending-steering").as_bool(),
        Some(true)
    );
    let gallery = message_find_kind(&tree, "ImageGallery");
    let load = property(&property(&gallery, "props"), "load")
        .dyn_into::<Function>()
        .unwrap();
    let rejected = load
        .call1(&JsValue::UNDEFINED, &JsValue::NULL)
        .unwrap()
        .dyn_into::<Promise>()
        .unwrap();
    let error = JsFuture::from(rejected).await.unwrap_err();
    assert_eq!(
        property(&error, "message").as_string().as_deref(),
        Some("图片服务不可用")
    );
    let actions = message_find_kind(&tree, "MessageIconActions");
    assert!(property(&property(&actions, "props"), "time").is_undefined());
    assert_eq!(
        property(&property(&actions, "props"), "text")
            .as_string()
            .as_deref(),
        Some("")
    );
}

#[wasm_bindgen_test]
fn branch_adapters_forward_context_compaction_unknown_and_terminal_notices() {
    setup();
    let translate = make_message_translate();
    let context_data = object(&[
        ("content", Array::new().into()),
        ("source", JsValue::NULL),
        (
            "provenance",
            object(&[
                ("role", JsValue::from_str("inject")),
                ("label", JsValue::NULL),
            ])
            .into(),
        ),
        ("form", JsValue::NULL),
    ]);
    let context = message_render(
        &context_message_node_view_component().unwrap(),
        props(view_node(context_data.clone().into())).as_ref(),
    );
    assert_eq!(
        property(&context, "kind").as_string().as_deref(),
        Some("ContextInjectionRow")
    );
    assert!(Object::is(
        &property(&property(&context, "props"), "provenance"),
        &property(&context_data, "provenance")
    ));

    let compaction_data = object(&[("summary", JsValue::from_str("checkpoint"))]);
    let compaction = message_render(
        &compaction_node_view_component().unwrap(),
        props(view_node(compaction_data.clone().into())).as_ref(),
    );
    assert_eq!(
        property(&compaction, "kind").as_string().as_deref(),
        Some("CompactionItem")
    );
    assert!(Object::is(
        &property(&property(&compaction, "props"), "node"),
        compaction_data.as_ref()
    ));

    let unknown_data = object(&[
        ("type", JsValue::from_str("surface/next")),
        ("data", object(&[("x", JsValue::from_f64(1.0))]).into()),
    ]);
    let unknown = message_render(
        &unknown_node_view_component().unwrap(),
        props(view_node(unknown_data.into())).as_ref(),
    );
    assert!(message_text(&unknown).contains("未知 surface 事件：surface/next"));

    let error_data = object(&[
        ("message", JsValue::from_str("API key is invalid")),
        ("code", JsValue::from_str("AUTH")),
    ]);
    let error = message_render(
        &turn_error_node_view_component().unwrap(),
        props(view_node(error_data.into())).as_ref(),
    );
    assert_eq!(
        property(
            &property(&message_find_kind(&error, "StateDot"), "props"),
            "state"
        )
        .as_string()
        .as_deref(),
        Some("error")
    );
    assert_eq!(message_text(&error), "请求失败API key is invalidAUTH");
    let maximum = message_render(
        &turn_max_tokens_node_view_component().unwrap(),
        object(&[("node", object(&[]).into()), ("t", translate.into())]).as_ref(),
    );
    assert_eq!(
        property(
            &property(&message_find_kind(&maximum, "StateDot"), "props"),
            "state"
        )
        .as_string()
        .as_deref(),
        Some("warning")
    );
    assert_eq!(message_text(&maximum), "已达到最大输出长度回答可能不完整");
}

#[wasm_bindgen_test]
fn retry_countdown_reanchors_ticks_stops_and_uses_durable_states() {
    setup();
    let component = retry_node_view_component().unwrap();
    let scheduled = retry_node(5.0, "scheduled", "normal", 1.0, 2_500.4, "连接被重置");
    let retry_data = object(&[("current", scheduled.into())]);
    let retry_props = props(view_node(retry_data.into()));
    let first = message_render(&component, retry_props.as_ref());
    assert_eq!(
        property(&property(&first, "props"), "data-active").as_bool(),
        Some(true)
    );
    assert!(message_text(&first).contains("正在重试模型请求（1/2） · 3s"));
    assert!(message_text(&first).contains("重试延迟：2500ms"));
    assert!(message_text(&first).contains("失败原因：连接被重置"));
    assert_eq!(message_timer_count(), 1);

    message_advance(1_100.0);
    let second = message_render(&component, retry_props.as_ref());
    assert!(message_text(&second).contains("正在重试模型请求（1/2） · 2s"));
    message_advance(1_000.0);
    let final_tick = message_render(&component, retry_props.as_ref());
    assert!(message_text(&final_tick).contains("正在重试模型请求（1/2） · 1s"));
    assert_eq!(message_timer_count(), 0);

    let next = retry_node(6.0, "scheduled", "normal", 2.0, 3_500.4, "再次断开");
    let next_props = props(view_node(object(&[("current", next.into())]).into()));
    let reanchored = message_render(&component, next_props.as_ref());
    assert!(message_text(&reanchored).contains("正在重试模型请求（2/2） · 4s"));
    assert_eq!(message_timer_count(), 1);

    let started = retry_node(6.0, "started", "normal", 2.0, 3_500.4, "再次断开");
    let started_props = props(view_node(object(&[("current", started.into())]).into()));
    let settled = message_render(&component, started_props.as_ref());
    assert!(message_text(&settled).contains("已重试模型请求（2/2） · 4s"));
    assert!(property(&property(&settled, "props"), "data-active").is_undefined());
    assert_eq!(message_timer_count(), 0);

    let always = retry_node(7.0, "started", "always", 3.0, 3_500.4, "继续重试");
    let always_props = props(view_node(object(&[("current", always.into())]).into()));
    assert!(
        message_text(&message_render(&component, always_props.as_ref()))
            .contains("已重试模型请求（3/∞） · 4s")
    );
    let cancelled = retry_node(8.0, "cancelled", "normal", 1.0, 3_500.4, "用户取消");
    let cancelled_props = props(view_node(object(&[("current", cancelled.into())]).into()));
    assert!(
        message_text(&message_render(&component, cancelled_props.as_ref()))
            .contains("模型请求重试已取消（1/2） · 4s")
    );
    message_unmount();
    assert_eq!(message_timer_count(), 0);
    assert!((retry_seconds_browser(-1.0) - 1.0).abs() < f64::EPSILON);
}
