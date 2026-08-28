//! Live Rust/WASM Plan chip and command plugin parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Promise, Reflect};
use seekdeep_client_ui_plan::{
    apply_client_ui_plan, configure_client_ui_plan, exported_plan_chip_component, plan_inject,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
if (typeof globalThis.document === 'undefined') {
  const nodes = []
  const selected = selector => {
    const match = /^style\[data-plugin=(.+)\]$/.exec(selector)
    if (match === null) return []
    const plugin = JSON.parse(match[1])
    return nodes.filter(node => node.kind === 'style' && node.attributes['data-plugin'] === plugin)
  }
  globalThis.document = {
    querySelector(selector) { return selected(selector)[0] ?? null },
    querySelectorAll(selector) { return selected(selector) },
    createElement(kind) {
      return {
        kind, attributes: {}, textContent: '',
        setAttribute(name, value) { this.attributes[name] = value },
      }
    },
    head: { appendChild(node) { nodes.push(node); return node } },
  }
}

function hooks() {
  const slots = []
  let cursor = 0
  let pending = []
  const React = {
    createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } },
    useState(initial) {
      const index = cursor++
      if (slots[index] === undefined) {
        const slot = { kind: 'state', value: initial }
        slot.set = value => { slot.value = typeof value === 'function' ? value(slot.value) : value }
        slots[index] = slot
      }
      return [slots[index].value, slots[index].set]
    },
    useRef(initial) {
      const index = cursor++
      if (slots[index] === undefined) slots[index] = { kind: 'ref', current: initial }
      return slots[index]
    },
    useEffect(effect, dependencies) {
      const index = cursor++
      if (slots[index] === undefined) slots[index] = { kind: 'effect', deps: undefined, cleanup: undefined }
      const slot = slots[index]
      const deps = Array.from(dependencies)
      const changed = slot.deps === undefined || slot.deps.length !== deps.length
        || slot.deps.some((value, at) => !Object.is(value, deps[at]))
      if (changed) pending.push({ slot, deps, effect })
    },
  }
  return {
    React,
    render(component, props) {
      cursor = 0
      pending = []
      const tree = component(props)
      for (const entry of pending) {
        if (typeof entry.slot.cleanup === 'function') entry.slot.cleanup()
        entry.slot.deps = entry.deps
        entry.slot.cleanup = entry.effect()
      }
      return tree
    },
    unmount() {
      for (const slot of [...slots].reverse()) {
        if (slot?.kind === 'effect' && typeof slot.cleanup === 'function') slot.cleanup()
      }
    },
  }
}

function textOf(node) {
  if (node === null || node === undefined || node === false) return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(textOf).join('')
  return (node.children ?? []).map(textOf).join('')
}
function find(node, predicate) {
  if (node === null || node === undefined || node === false) return undefined
  if (typeof node === 'string' || typeof node === 'number') return undefined
  if (!Array.isArray(node) && predicate(node)) return node
  for (const child of Array.isArray(node) ? node : node.children ?? []) {
    const found = find(child, predicate)
    if (found !== undefined) return found
  }
  return undefined
}
const translations = {
  'chip.on.aria': 'plan mode 已开启，按下关闭',
  'chip.on.title': 'plan mode 已开启 — 点击关闭（/plan off）',
  'chip.off.aria': 'plan mode 已关闭，按下开启',
  'chip.off.title': 'plan mode 已关闭 — 点击开启（/plan）',
}

export function makePlanBench() {
  const hookState = hooks()
  const bench = {
    hooks: hookState, React: hookState.React,
    primitives: { IconCloseFill14: 'IconCloseFill14' },
    plan: undefined, locked: false, calls: 0,
    exitMode: 'resolve', exitValue: null,
    pendingResolve: undefined, pendingReject: undefined,
  }
  bench.props = {
    useProjection(key) { if (key !== 'plan') throw new Error('wrong projection key'); return bench.plan },
    get locked() { return bench.locked },
    exitPlanMode() {
      bench.calls += 1
      if (bench.exitMode === 'pending') {
        return new Promise((resolve, reject) => { bench.pendingResolve = resolve; bench.pendingReject = reject })
      }
      if (bench.exitMode === 'reject-error') return Promise.reject(new Error(bench.exitValue))
      if (bench.exitMode === 'reject-value') return Promise.reject(bench.exitValue)
      return Promise.resolve(bench.exitValue)
    },
    t(key) { return translations[key] ?? key },
  }
  return bench
}
export function planRender(bench, component) { bench.tree = bench.hooks.render(component, bench.props); return bench.tree }
export function planSetProjection(bench, projection) { bench.plan = projection }
export function planSetLocked(bench, locked) { bench.locked = locked }
export function planSetExit(bench, mode, value) { bench.exitMode = mode; bench.exitValue = value }
export function planResolve(bench, value) { bench.pendingResolve?.(value) }
export function planReject(bench, value) { bench.pendingReject?.(value) }
export function planUnmount(bench) { bench.hooks.unmount() }
export function planCalls(bench) { return bench.calls }
export function planText(tree) { return textOf(tree) }
export function planButton(tree) { return find(tree, node => node.kind === 'button') }
export function planStatus(tree) { return find(tree, node => node.props?.role === 'status') }
export function planClick(node) {
  if (node === undefined || node.props.disabled) return undefined
  return node.props.onClick()
}
export function planTick() { return Promise.resolve().then(() => Promise.resolve()).then(() => Promise.resolve()) }
export function planStyleCount() {
  return document.querySelectorAll('style[data-plugin="@seekdeep-ai/seekdeep-client-ui-plan"]').length
}

export function makePlanPluginBench() {
  const ui = makePlanBench()
  const effects = [], entries = [], calls = []
  let result = { ok: true, value: { commandId: 'c1' } }
  const commands = {
    execute(sessionId, line) { calls.push([sessionId, line]); return Promise.resolve(result) },
  }
  const own = dispose => { effects.push(dispose); return dispose }
  const ctx = {
    effect(setup) { return own(setup()) },
    locale: { register() { return () => {} } },
    remote: { commands },
    'remote.commands': commands,
  }
  ctx.slots = {
    inject(name, install) { return own(install()) },
    register(options, component) {
      const entry = { options, component }
      entries.push(entry)
      return () => entries.splice(entries.indexOf(entry), 1)
    },
  }
  return { ...ui, ctx, effects, entries, calls, setResult(value) { result = value } }
}
export function planPluginEntries(bench) { return bench.entries }
export function planPluginInject(bench, sessionId) { return bench.entries[0].options.inject(sessionId) }
export function planPluginCalls(bench) { return bench.calls }
export function planPluginSetResult(bench, result) { bench.setResult(result) }
export function planPluginDispose(bench) {
  for (const dispose of bench.effects.splice(0).reverse()) dispose()
}
"#)]
extern "C" {
    fn makePlanBench() -> JsValue;
    fn planRender(bench: &JsValue, component: &Function) -> JsValue;
    fn planSetProjection(bench: &JsValue, projection: &JsValue);
    fn planSetLocked(bench: &JsValue, locked: bool);
    fn planSetExit(bench: &JsValue, mode: &str, value: &JsValue);
    fn planResolve(bench: &JsValue, value: &JsValue);
    fn planReject(bench: &JsValue, value: &JsValue);
    fn planUnmount(bench: &JsValue);
    fn planCalls(bench: &JsValue) -> u32;
    fn planText(tree: &JsValue) -> String;
    fn planButton(tree: &JsValue) -> JsValue;
    fn planStatus(tree: &JsValue) -> JsValue;
    fn planClick(node: &JsValue) -> JsValue;
    fn planTick() -> Promise;
    fn planStyleCount() -> u32;
    fn makePlanPluginBench() -> JsValue;
    fn planPluginEntries(bench: &JsValue) -> Array;
    fn planPluginInject(bench: &JsValue, session_id: &str) -> JsValue;
    fn planPluginCalls(bench: &JsValue) -> Array;
    fn planPluginSetResult(bench: &JsValue, result: &JsValue);
    fn planPluginDispose(bench: &JsValue);
}

fn property(value: &JsValue, key: &str) -> JsValue {
    let direct = Reflect::get(value, &JsValue::from_str(key)).unwrap();
    if !direct.is_undefined() {
        return direct;
    }
    let props = Reflect::get(value, &JsValue::from_str("props")).unwrap_or(JsValue::UNDEFINED);
    Reflect::get(&props, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
}

fn component(bench: &JsValue) -> Function {
    configure_client_ui_plan(property(bench, "React"), property(bench, "primitives")).unwrap();
    exported_plan_chip_component().unwrap().dyn_into().unwrap()
}

fn projection(active: bool, pending: bool) -> JsValue {
    js_sys::JSON::parse(&format!(r#"{{"active":{active},"pending":{pending}}}"#)).unwrap()
}

#[wasm_bindgen_test(async)]
async fn visibility_one_flight_lock_and_projection_exit_are_live() {
    let bench = makePlanBench();
    let component = component(&bench);
    assert!(planRender(&bench, &component).is_null());
    planSetProjection(&bench, &projection(false, false));
    assert!(planRender(&bench, &component).is_null());
    planSetProjection(&bench, &projection(true, true));
    assert!(planRender(&bench, &component).is_null());
    planSetProjection(&bench, &projection(true, false));
    let active = planRender(&bench, &component);
    let button = planButton(&active);
    assert_eq!(planText(&button), "Plan");
    assert_eq!(
        property(&button, "aria-label").as_string().as_deref(),
        Some("plan mode 已开启，按下关闭")
    );
    assert_eq!(planStyleCount(), 1);
    planSetLocked(&bench, true);
    let locked = planRender(&bench, &component);
    assert_eq!(
        property(&planButton(&locked), "disabled").as_bool(),
        Some(true)
    );
    planSetLocked(&bench, false);
    planSetExit(&bench, "pending", &JsValue::NULL);
    let ready = planRender(&bench, &component);
    assert!(planClick(&planButton(&ready)).is_undefined());
    assert_eq!(planCalls(&bench), 1);
    let leaving = planRender(&bench, &component);
    let leaving_button = planButton(&leaving);
    assert_eq!(property(&leaving_button, "disabled").as_bool(), Some(true));
    planClick(&leaving_button);
    assert_eq!(planCalls(&bench), 1);
    planResolve(&bench, &JsValue::NULL);
    JsFuture::from(planTick()).await.unwrap();
    planSetProjection(&bench, &projection(true, true));
    assert!(planRender(&bench, &component).is_null());
    planSetProjection(&bench, &projection(false, true));
    assert_eq!(
        planText(&planButton(&planRender(&bench, &component))),
        "Plan"
    );
}

#[wasm_bindgen_test(async)]
async fn admission_transport_and_unmount_paths_are_live() {
    let bench = makePlanBench();
    let plan_component = component(&bench);
    planSetProjection(&bench, &projection(true, false));
    planSetExit(&bench, "resolve", &JsValue::from_str("host said no"));
    planClick(&planButton(&planRender(&bench, &plan_component)));
    JsFuture::from(planTick()).await.unwrap();
    let admission = planRender(&bench, &plan_component);
    assert_eq!(
        planText(&planStatus(&admission)),
        "failed to exit plan mode"
    );
    assert_eq!(
        property(&planStatus(&admission), "title")
            .as_string()
            .as_deref(),
        Some("host said no")
    );

    planSetExit(&bench, "reject-error", &JsValue::from_str("network down"));
    planClick(&planButton(&admission));
    JsFuture::from(planTick()).await.unwrap();
    let network = planRender(&bench, &plan_component);
    assert_eq!(
        property(&planStatus(&network), "title")
            .as_string()
            .as_deref(),
        Some("network down")
    );
    planSetExit(&bench, "reject-value", &JsValue::from_str("socket closed"));
    planClick(&planButton(&network));
    JsFuture::from(planTick()).await.unwrap();
    let socket = planRender(&bench, &plan_component);
    assert_eq!(
        property(&planStatus(&socket), "title")
            .as_string()
            .as_deref(),
        Some("socket closed")
    );

    planSetExit(&bench, "pending", &JsValue::NULL);
    planClick(&planButton(&socket));
    planUnmount(&bench);
    planResolve(&bench, &JsValue::NULL);
    JsFuture::from(planTick()).await.unwrap();

    let rejected = makePlanBench();
    let rejected_component = component(&rejected);
    planSetProjection(&rejected, &projection(true, false));
    planSetExit(&rejected, "pending", &JsValue::NULL);
    planClick(&planButton(&planRender(&rejected, &rejected_component)));
    planUnmount(&rejected);
    planReject(&rejected, &js_sys::Error::new("late").into());
    JsFuture::from(planTick()).await.unwrap();
}

#[wasm_bindgen_test(async)]
async fn plugin_folds_command_results_and_retracts_the_plan_seat() {
    let bench = makePlanPluginBench();
    configure_client_ui_plan(property(&bench, "React"), property(&bench, "primitives")).unwrap();
    apply_client_ui_plan(property(&bench, "ctx")).unwrap();
    assert_eq!(plan_inject().length(), 4);
    assert_eq!(planPluginEntries(&bench).length(), 1);
    let entry = planPluginEntries(&bench).get(0);
    assert_eq!(
        property(&property(&entry, "options"), "name")
            .as_string()
            .as_deref(),
        Some("conversation.input.plan")
    );
    let injected = planPluginInject(&bench, "s-plan");
    let exit = property(&injected, "exitPlanMode")
        .dyn_into::<Function>()
        .unwrap();
    let admitted = exit.call0(&injected).unwrap();
    assert!(
        JsFuture::from(Promise::resolve(&admitted))
            .await
            .unwrap()
            .is_null()
    );
    let first_call = Array::from(&planPluginCalls(&bench).get(0));
    assert_eq!(first_call.get(0).as_string().as_deref(), Some("s-plan"));
    assert_eq!(first_call.get(1).as_string().as_deref(), Some("/plan off"));

    planPluginSetResult(
        &bench,
        &js_sys::JSON::parse(
            r#"{"ok":false,"error":{"code":"session-not-found","message":"gone","details":{}}}"#,
        )
        .unwrap(),
    );
    let failure = JsFuture::from(Promise::resolve(&exit.call0(&injected).unwrap()))
        .await
        .unwrap();
    assert_eq!(
        failure.as_string().as_deref(),
        Some("gone (session-not-found)")
    );
    planPluginSetResult(&bench, &js_sys::JSON::parse(r#"{"ok":true}"#).unwrap());
    let unknown = JsFuture::from(Promise::resolve(&exit.call0(&injected).unwrap()))
        .await
        .unwrap();
    assert_eq!(
        unknown.as_string().as_deref(),
        Some("unknown command: /plan off")
    );
    planPluginDispose(&bench);
    assert_eq!(planPluginEntries(&bench).length(), 0);
    apply_client_ui_plan(property(&bench, "ctx")).unwrap();
    assert_eq!(planPluginEntries(&bench).length(), 1);
    planPluginDispose(&bench);
    assert_eq!(planPluginEntries(&bench).length(), 0);
}
