//! Live WASM coverage for composer stats derivation, rendering, and measurement.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_conversation::{
    billed_input_tokens_browser, cache_hit_percent_browser,
    configure_client_ui_conversation_stats_line, context_occupancy_browser, derive_stats_browser,
    format_duration_browser, format_tokens_browser, stats_line_component,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hooks = []
let cursor = 0
let pendingLayouts = []
let session = { chat: { legacy: { nodes: [] } } }
let projections = {}
let projectionKeys = []
let locale = 'en'
let rowElement = { scrollWidth: 0, clientWidth: 0 }
let observers = []
let trackedReads = 0
function sameDeps(left, right) { return left?.length === right?.length && left.every((value, index) => Object.is(value, right[index])) }
class BenchResizeObserver {
  constructor(callback) { this.callback = callback; this.disconnected = false; this.element = null; observers.push(this) }
  observe(element) { this.element = element }
  disconnect() { this.disconnected = true }
}
export function installStatsBench() {
  hooks = []; cursor = 0; pendingLayouts = []; session = { chat: { legacy: { nodes: [] } } }
  projections = {}; projectionKeys = []; locale = 'en'; rowElement = { scrollWidth: 0, clientWidth: 0 }
  observers = []; trackedReads = 0
  globalThis.ResizeObserver = BenchResizeObserver
  globalThis.document = {
    head: { appendChild() {} }, createElement() { return { setAttribute() {} } }, querySelector() { return null },
  }
  const React = {
    Fragment: 'Fragment',
    createElement(kind, props, ...children) {
      if (kind === 'div' && props?.ref) props.ref.current = rowElement
      return { kind, props: props ?? {}, children }
    },
    memo(component) { component.memoized = true; return component },
    useState(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { type: 'state', value: initial }
      const set = update => { hooks[index].value = typeof update === 'function' ? update(hooks[index].value) : update }
      return [hooks[index].value, set]
    },
    useMemo(factory, deps) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].deps, deps)) hooks[index] = { type: 'memo', deps: [...deps], value: factory() }
      return hooks[index].value
    },
    useRef(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { type: 'ref', value: { current: initial } }
      return hooks[index].value
    },
    useLayoutEffect(effect, deps) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].deps, deps)) {
        const previous = hooks[index]
        pendingLayouts.push(() => {
          previous?.cleanup?.()
          hooks[index] = { type: 'layout', deps: [...deps], cleanup: effect() }
        })
      }
    },
  }
  return { React, uiPrimitives: { Tooltip: 'Tooltip' } }
}
export function statsObject(entries) { return Object.fromEntries(entries) }
export function statsSetSession(value) { session = value }
export function statsSetProjections(value) { projections = value }
export function statsSetLocale(value) { locale = value }
export function makeStatsUseSession() { return selector => selector(session) }
export function makeStatsProjection() { return key => { projectionKeys.push(key); return projections[key] } }
export function makeStatsTranslate() {
  return (key, vars) => {
    const en = {
      'stats.counts': `${vars?.turns} turns · ${vars?.steps} steps`,
      'stats.llm': `LLM ${vars?.duration}`, 'stats.toolCall': `Tool call ${vars?.duration}`,
      'stats.ttftAverage': `TTFT avg ${vars?.duration}`, 'stats.tokensPerSecond': `${vars?.throughput} tok/s`,
      'stats.cacheHit': `Cache hit ${vars?.percent}%`,
      'stats.tokens': `Input ${vars?.input} tok · Output ${vars?.output} tok`,
    }
    const zh = {
      'stats.counts': `${vars?.turns} 轮 · ${vars?.steps} 步`,
      'stats.llm': `LLM ${vars?.duration}`, 'stats.toolCall': `工具调用 ${vars?.duration}`,
      'stats.ttftAverage': `首 token 平均 ${vars?.duration}`, 'stats.tokensPerSecond': `${vars?.throughput} tok/s`,
      'stats.cacheHit': `缓存命中 ${vars?.percent}%`,
      'stats.tokens': `输入 ${vars?.input} tok · 输出 ${vars?.output} tok`,
    }
    return (locale === 'zh' ? zh : en)[key] ?? key
  }
}
export function statsRender(component, props) {
  cursor = 0
  pendingLayouts = []
  for (const hook of hooks) if (hook?.type === 'ref') hook.value.current = null
  const tree = component(props)
  for (const run of pendingLayouts) run()
  return tree
}
export function statsUnmount() { for (const hook of [...hooks].reverse()) hook?.cleanup?.(); hooks = [] }
export function statsSetWidths(scrollWidth, clientWidth) { rowElement.scrollWidth = scrollWidth; rowElement.clientWidth = clientWidth }
export function statsTriggerResize() { for (const observer of observers) if (!observer.disconnected) observer.callback([]) }
export function statsDisableResizeObserver() { delete globalThis.ResizeObserver }
export function statsObserverCounts() {
  return { created: observers.length, disconnected: observers.filter(observer => observer.disconnected).length,
    active: observers.filter(observer => !observer.disconnected).length }
}
export function statsProjectionKeys() { return projectionKeys }
export function statsTrackedAssistant(turn) {
  return { get kind() { trackedReads += 1; return 'assistant' }, turn, step: 1, time: 1000,
    blocks: [{ kind: 'text', text: 'tracked' }] }
}
export function statsTrackedReads() { return trackedReads }
export function statsText(value) {
  if (value === null || value === undefined || typeof value === 'boolean') return ''
  if (typeof value === 'string' || typeof value === 'number') return String(value)
  if (Array.isArray(value)) return value.map(statsText).join('')
  return statsText(value.children)
}
export function statsFindKind(value, kind) {
  if (value === null || value === undefined || typeof value !== 'object') return undefined
  if (value.kind === kind) return value
  for (const child of value.children ?? []) { const found = statsFindKind(child, kind); if (found) return found }
  return undefined
}
export function statsFindAllClass(value, className) {
  if (value === null || value === undefined || typeof value !== 'object') return []
  const own = String(value.props?.className ?? '').split(/\s+/).includes(className) ? [value] : []
  return own.concat(...(value.children ?? []).map(child => statsFindAllClass(child, className)))
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = installStatsBench)]
    fn install_stats_bench() -> JsValue;
    #[wasm_bindgen(js_name = statsObject)]
    fn stats_object(entries: &Array) -> JsValue;
    #[wasm_bindgen(js_name = statsSetSession)]
    fn stats_set_session(value: &JsValue);
    #[wasm_bindgen(js_name = statsSetProjections)]
    fn stats_set_projections(value: &JsValue);
    #[wasm_bindgen(js_name = statsSetLocale)]
    fn stats_set_locale(value: &str);
    #[wasm_bindgen(js_name = makeStatsUseSession)]
    fn make_stats_use_session() -> Function;
    #[wasm_bindgen(js_name = makeStatsProjection)]
    fn make_stats_projection() -> Function;
    #[wasm_bindgen(js_name = makeStatsTranslate)]
    fn make_stats_translate() -> Function;
    #[wasm_bindgen(js_name = statsRender)]
    fn stats_render(component: &JsValue, props: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = statsUnmount)]
    fn stats_unmount();
    #[wasm_bindgen(js_name = statsSetWidths)]
    fn stats_set_widths(scroll_width: f64, client_width: f64);
    #[wasm_bindgen(js_name = statsTriggerResize)]
    fn stats_trigger_resize();
    #[wasm_bindgen(js_name = statsDisableResizeObserver)]
    fn stats_disable_resize_observer();
    #[wasm_bindgen(js_name = statsObserverCounts)]
    fn stats_observer_counts() -> JsValue;
    #[wasm_bindgen(js_name = statsProjectionKeys)]
    fn stats_projection_keys() -> Array;
    #[wasm_bindgen(js_name = statsTrackedAssistant)]
    fn stats_tracked_assistant(turn: f64) -> JsValue;
    #[wasm_bindgen(js_name = statsTrackedReads)]
    fn stats_tracked_reads() -> u32;
    #[wasm_bindgen(js_name = statsText)]
    fn stats_text(value: &JsValue) -> String;
    #[wasm_bindgen(js_name = statsFindKind)]
    fn stats_find_kind(value: &JsValue, kind: &str) -> JsValue;
    #[wasm_bindgen(js_name = statsFindAllClass)]
    fn stats_find_all_class(value: &JsValue, class_name: &str) -> Array;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn object(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    stats_object(&array).unchecked_into()
}

fn assistant(turn: f64, output_tokens: Option<f64>, timed: bool) -> Object {
    let mut entries = vec![
        ("kind", JsValue::from_str("assistant")),
        ("turn", JsValue::from_f64(turn)),
        ("step", JsValue::from_f64(turn)),
        ("time", JsValue::from_f64(turn * 1_000.0)),
    ];
    if timed {
        entries.push((
            "timing",
            object(&[
                ("stepStartTime", JsValue::from_f64(1_000.0)),
                ("firstTokenTime", JsValue::from_f64(1_800.0)),
                ("completedTime", JsValue::from_f64(4_800.0)),
            ])
            .into(),
        ));
    }
    if let Some(output_tokens) = output_tokens {
        entries.push((
            "usage",
            object(&[("outputTokens", JsValue::from_f64(output_tokens))]).into(),
        ));
    }
    object(&entries)
}

fn session(nodes: &Array, top_level_nodes: Option<&Array>) -> Object {
    object(&[
        (
            "chat",
            object(&[("legacy", object(&[("nodes", nodes.clone().into())]).into())]).into(),
        ),
        (
            "nodes",
            top_level_nodes.map_or_else(|| Array::new().into(), |nodes| nodes.clone().into()),
        ),
    ])
}

fn usage(input: f64, output: f64, read: f64, write: f64) -> Object {
    object(&[
        ("uncachedInputTokens", JsValue::from_f64(input)),
        ("outputTokens", JsValue::from_f64(output)),
        ("cacheReadTokens", JsValue::from_f64(read)),
        ("cacheWriteTokens", JsValue::from_f64(write)),
    ])
}

fn projected_stats(entries: &[(&str, f64)]) -> Object {
    let mut values = vec![
        ("turns", JsValue::from_f64(0.0)),
        ("steps", JsValue::from_f64(0.0)),
        ("llmMs", JsValue::from_f64(0.0)),
        ("toolMs", JsValue::from_f64(0.0)),
        ("ttftMs", JsValue::from_f64(0.0)),
        ("ttftSteps", JsValue::from_f64(0.0)),
        ("decodeMs", JsValue::from_f64(0.0)),
        ("decodeTokens", JsValue::from_f64(0.0)),
    ];
    for (key, value) in entries {
        if let Some(existing) = values.iter_mut().find(|(name, _)| name == key) {
            existing.1 = JsValue::from_f64(*value);
        }
    }
    object(&values)
}

fn setup(nodes: &Array, values: &Object) -> (JsValue, Object) {
    let bench = install_stats_bench();
    configure_client_ui_conversation_stats_line(
        property(&bench, "React"),
        property(&bench, "uiPrimitives"),
    )
    .unwrap();
    stats_set_session(session(nodes, None).as_ref());
    stats_set_projections(values.as_ref());
    let props = object(&[
        ("useSession", make_stats_use_session().into()),
        ("useProjection", make_stats_projection().into()),
        ("t", make_stats_translate().into()),
    ]);
    (stats_line_component().unwrap(), props)
}

#[wasm_bindgen_test]
fn exported_helpers_fold_format_bill_and_resolve_context() {
    let nodes = Array::new();
    nodes.push(assistant(1.0, Some(40.0), true).as_ref());
    nodes.push(assistant(1.0, None, false).as_ref());
    nodes.push(assistant(2.0, None, false).as_ref());
    nodes.push(
        object(&[
            ("kind", JsValue::from_str("tool-result")),
            ("time", JsValue::from_f64(7_000.0)),
            ("callTime", JsValue::from_f64(4_000.0)),
        ])
        .as_ref(),
    );
    nodes.push(
        object(&[
            ("kind", JsValue::from_str("tool-result")),
            ("time", JsValue::from_f64(9_000.0)),
            ("callTime", JsValue::NULL),
        ])
        .as_ref(),
    );
    let stats = derive_stats_browser(nodes.into()).unwrap();
    for (key, expected) in [
        ("turns", 2.0),
        ("steps", 3.0),
        ("llmMs", 3_800.0),
        ("toolMs", 3_000.0),
        ("ttftMs", 800.0),
        ("ttftSteps", 1.0),
        ("decodeMs", 3_000.0),
        ("decodeTokens", 40.0),
    ] {
        assert_eq!(property(&stats, key).as_f64(), Some(expected));
    }
    assert_eq!(format_tokens_browser(517.0).unwrap(), "517");
    assert_eq!(format_tokens_browser(12_240.0).unwrap(), "12.2K");
    assert_eq!(format_tokens_browser(517_000.0).unwrap(), "517K");
    assert_eq!(format_tokens_browser(1_230_000.0).unwrap(), "1.2M");
    assert_eq!(format_duration_browser(45_230.0).unwrap(), "45.2s");
    assert_eq!(format_duration_browser(162_000.0).unwrap(), "2m42s");

    let usage_value = usage(10.0, 7.0, 90.0, 100.0);
    assert!(
        (billed_input_tokens_browser(usage_value.clone().into()).unwrap() - 200.0).abs()
            < f64::EPSILON
    );
    assert_eq!(
        cache_hit_percent_browser(usage_value.into())
            .unwrap()
            .as_f64(),
        Some(45.0)
    );
    assert!(
        cache_hit_percent_browser(usage(0.0, 0.0, 0.0, 0.0).into())
            .unwrap()
            .is_null()
    );
    let occupancy = context_occupancy_browser(
        object(&[
            ("pressureTokens", JsValue::from_f64(32_000.0)),
            ("projectedTokens", JsValue::from_f64(6_000.0)),
            ("contextWindow", JsValue::from_f64(128_000.0)),
        ])
        .into(),
    )
    .unwrap();
    assert_eq!(property(&occupancy, "percent").as_f64(), Some(5.0));
    assert_eq!(property(&occupancy, "usedTokens").as_f64(), Some(6_000.0));
    assert!(
        context_occupancy_browser(
            object(&[("pressureTokens", JsValue::from_f64(32_000.0),)]).into()
        )
        .unwrap()
        .is_null()
    );
}

#[wasm_bindgen_test]
fn grouped_row_projection_precedence_locale_and_empty_gates_match_source() {
    let nodes = Array::of1(assistant(1.0, Some(60.0), true).as_ref());
    let values = object(&[("tokenUsage", usage(10.0, 5.0, 90.0, 0.0).into())]);
    let (component, props) = setup(&nodes, &values);
    stats_set_widths(400.0, 400.0);
    let fallback = stats_render(&component, props.as_ref());
    assert_eq!(
        stats_text(&fallback),
        "1 turns · 1 steps| LLM 3.8s| TTFT avg 0.8s · 20 tok/s| Cache hit 90%| Input 100 tok · Output 5 tok"
    );
    let tooltip = stats_find_kind(&fallback, "Tooltip");
    assert_eq!(
        property(&property(&tooltip, "props"), "label")
            .as_string()
            .as_deref(),
        Some(
            "1 turns · 1 steps | LLM 3.8s | TTFT avg 0.8s · 20 tok/s | Cache hit 90% | Input 100 tok · Output 5 tok"
        )
    );
    assert_eq!(
        property(&property(&tooltip, "props"), "side")
            .as_string()
            .as_deref(),
        Some("top")
    );
    assert_eq!(
        property(&property(&tooltip, "props"), "delayMs").as_f64(),
        Some(500.0)
    );
    let separators = stats_find_all_class(&fallback, "seekdeep-conversation-statsLine-sep");
    assert_eq!(separators.length(), 4);
    assert!(
        separators
            .iter()
            .all(|separator| stats_text(&separator) == "|")
    );
    assert_eq!(
        stats_projection_keys()
            .iter()
            .filter_map(|key| key.as_string())
            .collect::<Vec<_>>(),
        ["tokenUsage", "sessionStats"]
    );

    stats_set_projections(
        object(&[
            ("tokenUsage", usage(10.0, 5.0, 90.0, 0.0).into()),
            (
                "sessionStats",
                projected_stats(&[
                    ("turns", 200.0),
                    ("steps", 200.0),
                    ("llmMs", 100_000.0),
                    ("toolMs", 62_000.0),
                    ("ttftMs", 1_600.0),
                    ("ttftSteps", 2.0),
                    ("decodeMs", 3_000.0),
                    ("decodeTokens", 60.0),
                ])
                .into(),
            ),
        ])
        .as_ref(),
    );
    let projected = stats_render(&component, props.as_ref());
    assert_eq!(
        stats_text(&projected),
        "200 turns · 200 steps| LLM 1m40s · Tool call 1m2s| TTFT avg 0.8s · 20 tok/s| Cache hit 90%| Input 100 tok · Output 5 tok"
    );
    stats_set_locale("zh");
    let localized = stats_render(&component, props.as_ref());
    assert_eq!(
        stats_text(&localized),
        "200 轮 · 200 步| LLM 1m40s · 工具调用 1m2s| 首 token 平均 0.8s · 20 tok/s| 缓存命中 90%| 输入 100 tok · 输出 5 tok"
    );

    stats_set_projections(
        object(&[(
            "sessionStats",
            projected_stats(&[("turns", 1.0), ("steps", 1.0)]).into(),
        )])
        .as_ref(),
    );
    assert_eq!(
        stats_text(&stats_render(&component, props.as_ref())),
        "1 轮 · 1 步"
    );
    stats_set_projections(
        object(&[
            ("tokenUsage", usage(0.0, 0.0, 0.0, 0.0).into()),
            ("sessionStats", projected_stats(&[]).into()),
        ])
        .as_ref(),
    );
    assert!(stats_render(&component, props.as_ref()).is_null());
    stats_unmount();
}

#[wasm_bindgen_test]
fn durable_token_only_output_only_and_defined_zero_projection_gates_match_source() {
    let nodes = Array::new();
    let values = object(&[("tokenUsage", usage(10.0, 5.0, 90.0, 0.0).into())]);
    let (component, props) = setup(&nodes, &values);
    assert_eq!(
        stats_text(&stats_render(&component, props.as_ref())),
        "Cache hit 90%| Input 100 tok · Output 5 tok"
    );

    stats_set_projections(object(&[("tokenUsage", usage(0.0, 7.0, 0.0, 0.0).into())]).as_ref());
    assert_eq!(
        stats_text(&stats_render(&component, props.as_ref())),
        "Input 0 tok · Output 7 tok"
    );

    let visible = Array::of1(assistant(1.0, None, false).as_ref());
    stats_set_session(session(&visible, None).as_ref());
    stats_set_projections(
        object(&[
            ("tokenUsage", usage(0.0, 0.0, 0.0, 0.0).into()),
            ("sessionStats", projected_stats(&[]).into()),
        ])
        .as_ref(),
    );
    assert!(stats_render(&component, props.as_ref()).is_null());
    stats_unmount();
}

#[wasm_bindgen_test]
fn settled_node_identity_memoizes_the_fallback_and_ignores_top_level_nodes() {
    let tracked = stats_tracked_assistant(1.0);
    let settled = Array::of1(&tracked);
    let top = Array::of1(assistant(99.0, None, false).as_ref());
    let values = object(&[]);
    let (component, props) = setup(&settled, &values);
    stats_set_session(session(&settled, Some(&top)).as_ref());
    let first = stats_render(&component, props.as_ref());
    assert_eq!(stats_text(&first), "1 turns · 1 steps");
    assert_eq!(property(&component, "memoized").as_bool(), Some(true));
    let reads = stats_tracked_reads();
    assert!(reads > 0);

    let replacement_top = Array::of1(assistant(42.0, None, false).as_ref());
    stats_set_session(session(&settled, Some(&replacement_top)).as_ref());
    let second = stats_render(&component, props.as_ref());
    assert_eq!(stats_text(&second), "1 turns · 1 steps");
    assert_eq!(stats_tracked_reads(), reads);
    stats_unmount();
}

#[wasm_bindgen_test]
fn layout_measurement_observer_cleanup_and_missing_observer_match_source() {
    let nodes = Array::of1(assistant(1.0, None, false).as_ref());
    let values = object(&[("tokenUsage", usage(10.0, 5.0, 90.0, 0.0).into())]);
    let (component, props) = setup(&nodes, &values);
    stats_set_widths(800.0, 400.0);
    let initial = stats_render(&component, props.as_ref());
    assert_eq!(
        property(
            &property(&stats_find_kind(&initial, "Tooltip"), "props"),
            "disabled"
        )
        .as_bool(),
        Some(true)
    );
    let clipped = stats_render(&component, props.as_ref());
    assert_eq!(
        property(
            &property(&stats_find_kind(&clipped, "Tooltip"), "props"),
            "disabled"
        )
        .as_bool(),
        Some(false)
    );
    assert_eq!(
        property(&stats_observer_counts(), "active").as_f64(),
        Some(1.0)
    );

    stats_set_widths(300.0, 400.0);
    stats_trigger_resize();
    let fitted = stats_render(&component, props.as_ref());
    assert_eq!(
        property(
            &property(&stats_find_kind(&fitted, "Tooltip"), "props"),
            "disabled"
        )
        .as_bool(),
        Some(true)
    );
    stats_set_projections(
        object(&[(
            "sessionStats",
            projected_stats(&[("turns", 2.0), ("steps", 2.0)]).into(),
        )])
        .as_ref(),
    );
    stats_render(&component, props.as_ref());
    let counts = stats_observer_counts();
    assert_eq!(property(&counts, "created").as_f64(), Some(2.0));
    assert_eq!(property(&counts, "disconnected").as_f64(), Some(1.0));
    assert_eq!(property(&counts, "active").as_f64(), Some(1.0));
    stats_unmount();
    assert_eq!(
        property(&stats_observer_counts(), "disconnected").as_f64(),
        Some(2.0)
    );

    let (component, props) = setup(&nodes, &values);
    stats_disable_resize_observer();
    stats_set_widths(800.0, 400.0);
    stats_render(&component, props.as_ref());
    let clipped = stats_render(&component, props.as_ref());
    assert_eq!(
        property(
            &property(&stats_find_kind(&clipped, "Tooltip"), "props"),
            "disabled"
        )
        .as_bool(),
        Some(false)
    );
    assert_eq!(
        property(&stats_observer_counts(), "created").as_f64(),
        Some(0.0)
    );
    stats_unmount();
}
