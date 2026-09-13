//! Live Rust/WASM Goal bar, command bubble, and plugin parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Promise, Reflect};
use seekdeep_client_ui_goal::{
    apply_client_ui_goal, configure_client_ui_goal, exported_goal_bar_component, goal_inject,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
function hooks() {
  const slots = []
  let cursor = 0
  return {
    React: {
      createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } },
      useRef(initial) { const i = cursor++; if (!(i in slots)) slots[i] = { current: initial }; return slots[i] },
      useState(initial) { const i = cursor++; if (!(i in slots)) slots[i] = initial; return [slots[i], value => { slots[i] = typeof value === 'function' ? value(slots[i]) : value }] },
    },
    reset() { cursor = 0 },
  }
}
function t(key) { return ({
  'phase.active': 'Ongoing Goal', 'phase.paused': 'Paused Goal', 'phase.blocked': 'Blocked Goal',
  'objective.aria': 'Goal objective', 'commandInput.aria': 'Command input',
  'action.save': 'Save goal', 'action.cancel': 'Cancel edit', 'action.pause': 'Pause goal',
  'action.resume': 'Resume goal', 'action.edit': 'Edit goal', 'action.clear': 'Clear goal',
})[key] ?? key }
export function makeGoalBench(phase = 'active') {
  const react = hooks()
  const calls = []
  const goal = { id: 'goal-1', revision: 1, objective: 'Ship it', phase, blockedReason: phase === 'blocked' ? { code: 'waiting', message: 'Waiting for review' } : undefined }
  const ok = () => Promise.resolve({ ok: true, value: null })
  return {
    react, React: react.React,
    primitives: {
      Tooltip: 'Tooltip', MessageText: 'MessageText', IconGoalOutline16: 'IconGoalOutline16',
      IconPauseOutline16: 'IconPauseOutline16', IconPlayOutline16: 'IconPlayOutline16',
      IconEditOutline16: 'IconEditOutline16', IconTrashOutline16: 'IconTrashOutline16',
      IconCheckOutline16: 'IconCheckOutline16', IconCloseOutline16: 'IconCloseOutline16',
    },
    props: {
      goal, t,
      onEdit(value) { calls.push(['edit', value]); return ok() },
      onPause() { calls.push(['pause']); return ok() },
      onResume() { calls.push(['resume']); return ok() },
      onClear() { calls.push(['clear']); return ok() },
    },
    calls,
  }
}
export function goalRender(bench, component) { bench.react.reset(); return component(bench.props) }
export function goalText(node) {
  if (node === null || node === undefined || node === false) return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(goalText).join('')
  if (node.kind === 'MessageText') return node.props?.text ?? ''
  return (node.children ?? []).map(goalText).join('')
}
export function goalFind(node, property, value) {
  if (node === null || node === undefined || node === false) return undefined
  if (typeof node === 'string' || typeof node === 'number') return undefined
  if (Array.isArray(node)) { for (const child of node) { const found = goalFind(child, property, value); if (found !== undefined) return found }; return undefined }
  if (node.props?.[property] === value) return node
  for (const child of node.children ?? []) { const found = goalFind(child, property, value); if (found !== undefined) return found }
  return undefined
}
export function goalInvoke(node, property, event) { return event === undefined ? node.props[property]() : node.props[property](event) }
export function goalChange(value) { return { target: { value } } }
export function goalKey(key) { return { key } }
export function goalCalls(bench) { return bench.calls }
export function goalTick() { return Promise.resolve().then(() => Promise.resolve()) }
export function goalReplace(bench, goal) { bench.props.goal = goal }

export function makeGoalPluginBench() {
  const effects = [], entries = [], eventDefinitions = [], calls = []
  let projection = { goal: { id: 'goal-1', revision: 1, objective: 'Ship it', phase: 'active' } }
  const session = { projections: { faceOf() { return { getSnapshot() { return projection } } } } }
  const remote = { goals: {
    edit(sessionId, ref, value) { calls.push(['edit', sessionId, ref, value]); return Promise.resolve({ ok: true }) },
    pause(sessionId, ref) { calls.push(['pause', sessionId, ref]); return Promise.resolve({ ok: true }) },
    resume(sessionId, ref) { calls.push(['resume', sessionId, ref]); return Promise.resolve({ ok: true }) },
    clear(sessionId, ref) { calls.push(['clear', sessionId, ref]); return Promise.resolve({ ok: true }) },
  } }
  const ctx = {
    effect(setup) { const dispose = setup(); effects.push(dispose); return dispose },
    locale: { register() { return () => {} } }, remote,
    sessions: { binding(id) { return id === 's1' ? { session } : undefined } },
    conversationEvents: { register(definition) { eventDefinitions.push(definition); return () => eventDefinitions.splice(eventDefinitions.indexOf(definition), 1) } },
  }
  ctx['remote.goals'] = remote.goals
  ctx.slots = {
    inject(name, install) { const dispose = install(); effects.push(dispose); return dispose },
    register(options, component) { const entry = { options, component }; entries.push(entry); return () => entries.splice(entries.indexOf(entry), 1) },
  }
  return { ctx, effects, entries, eventDefinitions, calls, setProjection(value) { projection = value } }
}
export function goalPluginEntries(bench) { return bench.entries }
export function goalPluginDefinitions(bench) { return bench.eventDefinitions }
export function goalPluginInject(bench, id) { return bench.entries.find(entry => entry.options.id === 'goal').options.inject(id) }
export function goalPluginCalls(bench) { return bench.calls }
export function goalPluginDispose(bench) { [...bench.effects].reverse().forEach(dispose => dispose()) }
export function goalPluginRenderCommand(bench) {
  const entry = bench.entries.find(entry => entry.options.key === 'command-input')
  return entry.component({ node: { data: { text: '/goal\nShip it' } }, t })
}
"#)]
extern "C" {
    fn makeGoalBench(phase: &str) -> JsValue;
    fn goalRender(bench: &JsValue, component: &Function) -> JsValue;
    fn goalText(node: &JsValue) -> String;
    fn goalFind(node: &JsValue, property: &str, value: &JsValue) -> JsValue;
    fn goalInvoke(node: &JsValue, property: &str, event: &JsValue) -> JsValue;
    fn goalChange(value: &str) -> JsValue;
    fn goalKey(key: &str) -> JsValue;
    fn goalCalls(bench: &JsValue) -> Array;
    fn goalTick() -> Promise;
    fn goalReplace(bench: &JsValue, goal: &JsValue);
    fn makeGoalPluginBench() -> JsValue;
    fn goalPluginEntries(bench: &JsValue) -> Array;
    fn goalPluginDefinitions(bench: &JsValue) -> Array;
    fn goalPluginInject(bench: &JsValue, id: &str) -> JsValue;
    fn goalPluginCalls(bench: &JsValue) -> Array;
    fn goalPluginDispose(bench: &JsValue);
    fn goalPluginRenderCommand(bench: &JsValue) -> JsValue;
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
    configure_client_ui_goal(property(bench, "React"), property(bench, "primitives")).unwrap();
    exported_goal_bar_component().unwrap().dyn_into().unwrap()
}

#[wasm_bindgen_test(async)]
async fn active_edit_pause_clear_and_identity_reset_are_live() {
    let bench = makeGoalBench("active");
    let component = component(&bench);
    let first = goalRender(&bench, &component);
    assert!(goalText(&first).contains("Ongoing Goal"));
    assert!(goalText(&first).contains("Ship it"));
    let edit = goalFind(&first, "aria-label", &JsValue::from_str("Edit goal"));
    goalInvoke(&edit, "onClick", &JsValue::UNDEFINED);
    let editing = goalRender(&bench, &component);
    let input = goalFind(
        &editing,
        "className",
        &JsValue::from_str("seekdeep-goal-objectiveInput"),
    );
    assert!(!input.is_undefined(), "{}", goalText(&editing));
    assert_eq!(
        property(&input, "value").as_string().as_deref(),
        Some("Ship it")
    );
    goalInvoke(&input, "onChange", &goalChange("Changed objective"));
    let changed = goalRender(&bench, &component);
    let changed_input = goalFind(&changed, "aria-label", &JsValue::from_str("Goal objective"));
    goalInvoke(&changed_input, "onKeyDown", &goalKey("Enter"));
    JsFuture::from(goalTick()).await.unwrap();
    assert!(goalCalls(&bench).iter().any(|call| {
        let call = Array::from(&call);
        call.get(0).as_string().as_deref() == Some("edit")
            && call.get(1).as_string().as_deref() == Some("Changed objective")
    }));

    let active = goalRender(&bench, &component);
    let clear = goalFind(&active, "aria-label", &JsValue::from_str("Clear goal"));
    goalInvoke(&clear, "onClick", &JsValue::UNDEFINED);
    JsFuture::from(goalTick()).await.unwrap();
    assert!(goalRender(&bench, &component).is_null());
    goalReplace(
        &bench,
        &js_sys::JSON::parse(r#"{"id":"goal-2","revision":1,"objective":"Next","phase":"paused"}"#)
            .unwrap(),
    );
    let replacement = goalRender(&bench, &component);
    assert!(goalText(&replacement).contains("Paused Goal"));
    assert!(
        !goalFind(
            &replacement,
            "aria-label",
            &JsValue::from_str("Resume goal")
        )
        .is_undefined()
    );
}

#[wasm_bindgen_test]
fn hidden_blocked_and_command_bubble_paths_are_live() {
    let bench = makeGoalBench("blocked");
    let component = component(&bench);
    let blocked = goalRender(&bench, &component);
    let bar = goalFind(&blocked, "title", &JsValue::from_str("Waiting for review"));
    assert!(!bar.is_undefined());
    assert!(goalText(&blocked).contains("Blocked Goal"));
    goalReplace(&bench, &JsValue::NULL);
    assert!(goalRender(&bench, &component).is_null());
}

#[wasm_bindgen_test(async)]
async fn plugin_registers_definition_slots_and_reads_current_cas_ref() {
    let bench = makeGoalPluginBench();
    let ui = makeGoalBench("active");
    configure_client_ui_goal(property(&ui, "React"), property(&ui, "primitives")).unwrap();
    apply_client_ui_goal(property(&bench, "ctx")).unwrap();
    assert_eq!(goal_inject().length(), 6);
    assert_eq!(goalPluginDefinitions(&bench).length(), 1);
    assert_eq!(goalPluginEntries(&bench).length(), 2);
    let actions = goalPluginInject(&bench, "s1");
    let pause = property(&actions, "onPause")
        .dyn_into::<Function>()
        .unwrap();
    let returned = pause.call0(&JsValue::UNDEFINED).unwrap();
    JsFuture::from(Promise::resolve(&returned)).await.unwrap();
    let call = Array::from(&goalPluginCalls(&bench).get(0));
    assert_eq!(call.get(0).as_string().as_deref(), Some("pause"));
    assert_eq!(
        property(&call.get(2), "id").as_string().as_deref(),
        Some("goal-1")
    );
    assert_eq!(property(&call.get(2), "revision").as_f64(), Some(1.0));
    let command = goalPluginRenderCommand(&bench);
    assert_eq!(
        property(&command, "aria-label").as_string().as_deref(),
        Some("Command input")
    );
    assert!(goalText(&command).contains("/goal\nShip it"));
    goalPluginDispose(&bench);
    assert_eq!(goalPluginEntries(&bench).length(), 0);
}
