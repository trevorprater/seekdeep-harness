//! Live Rust/WASM workflow-run panel and Client plugin parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Reflect};
use seekdeep_client_ui_workflow_run::{
    apply_client_ui_workflow_run, configure_client_ui_workflow_run,
    exported_workflow_run_panel_component, workflow_run_inject,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
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

function renderer() {
  const Fragment = Symbol('Fragment')
  const componentIds = new WeakMap()
  const states = new Map()
  let nextComponentId = 1
  let currentInstance
  let hookCursor = 0
  let active = new Set()
  const idOf = component => {
    if (!componentIds.has(component)) componentIds.set(component, nextComponentId++)
    return componentIds.get(component)
  }
  const React = {
    Fragment,
    createElement(kind, supplied, ...children) {
      const props = { ...(supplied ?? {}) }
      const key = props.key
      delete props.key
      if (children.length === 1) props.children = children[0]
      else if (children.length > 1) props.children = children
      return { __element: true, kind, key, props }
    },
    useState(initial) {
      if (currentInstance === undefined) throw new Error('useState outside component')
      const index = hookCursor++
      let slots = states.get(currentInstance)
      if (slots === undefined) { slots = []; states.set(currentInstance, slots) }
      if (!(index in slots)) slots[index] = initial
      const instance = currentInstance
      return [slots[index], value => {
        const owned = states.get(instance)
        if (owned === undefined) return
        owned[index] = typeof value === 'function' ? value(owned[index]) : value
      }]
    },
  }
  function resolve(value, path = 'root') {
    if (value === null || value === undefined || value === false) return null
    if (typeof value === 'string' || typeof value === 'number') return value
    if (Array.isArray(value)) return value.map((child, index) => resolve(child, `${path}.${index}`))
    if (!value.__element) return value
    if (value.kind === Fragment) return resolve(value.props.children ?? [], `${path}.fragment`)
    if (typeof value.kind === 'function') {
      const segment = value.key === undefined ? '' : `:${String(value.key)}`
      const instance = `${path}.c${idOf(value.kind)}${segment}`
      active.add(instance)
      const previous = currentInstance
      const previousCursor = hookCursor
      currentInstance = instance
      hookCursor = 0
      const output = value.kind(value.props)
      currentInstance = previous
      hookCursor = previousCursor
      return resolve(output, instance)
    }
    const children = value.props.children === undefined ? []
      : Array.isArray(value.props.children) ? value.props.children : [value.props.children]
    const props = { ...value.props }
    delete props.children
    return {
      kind: value.kind,
      props,
      children: children.map((child, index) => resolve(child, `${path}.${index}`)),
    }
  }
  return {
    React,
    render(component, props) {
      active = new Set()
      const tree = resolve(React.createElement(component, props))
      for (const instance of [...states.keys()]) if (!active.has(instance)) states.delete(instance)
      return tree
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
function findAll(node, predicate, result = []) {
  if (node === null || node === undefined || node === false
    || typeof node === 'string' || typeof node === 'number') return result
  if (!Array.isArray(node) && predicate(node)) result.push(node)
  for (const child of Array.isArray(node) ? node : node.children ?? []) findAll(child, predicate, result)
  return result
}

const zh = {
  'run.title': '{name}', 'run.members.one': '{count} 个成员', 'run.members.other': '{count} 个成员',
  'run.empty': '没有启动成员', 'phase.unassigned': '未分阶段', 'phase.empty': '空阶段名',
  'statusCount.running': '运行中 {count}', 'statusCount.completed': '已完成 {count}',
  'statusCount.failed': '失败 {count}', 'statusCount.cancelled': '已取消 {count}',
  'statusCount.interrupted': '已中断 {count}', 'member.empty': '空成员名',
  'member.open': '打开 {name}', 'status.running': '运行中', 'status.completed': '已完成',
  'status.failed': '失败', 'status.cancelled': '已取消', 'status.interrupted': '已中断',
}
function translate(key, values = {}) {
  return Object.entries(values).reduce(
    (text, [name, value]) => text.replace(`{${name}}`, String(value)), zh[key] ?? key,
  )
}

function DisclosureRow(props) {
  const rowExpands = props.expandable === true && props.expandOnRowClick === true
  const toggleKey = event => {
    if (event.key !== 'Enter' && event.key !== ' ') return
    event.preventDefault?.()
    props.onToggle()
  }
  const row = this.React.createElement('div', {
    className: props.rowClassName,
    'data-disclosure-row': true,
    'data-expandable': rowExpands ? true : undefined,
    role: rowExpands ? 'button' : undefined,
    tabIndex: rowExpands ? 0 : undefined,
    'aria-expanded': rowExpands ? props.open : undefined,
    onClick: rowExpands ? props.onToggle : undefined,
    onKeyDown: rowExpands ? toggleKey : undefined,
  }, props.icon, props.title, (props.keepContentWhenOpen || !props.open) ? props.collapsedContent : null)
  return this.React.createElement('div', {
    className: props.className,
    'data-open': props.open ? true : undefined,
  }, row, props.open ? props.children : null)
}

export function makeWorkflowBench() {
  const engine = renderer()
  const opened = []
  const primitives = {
    DisclosureRow: props => DisclosureRow.call({ React: engine.React }, props),
    IconChevronRightOutline14: props => engine.React.createElement('IconChevronRightOutline14', props),
    StateDot: props => engine.React.createElement('StateDot', { 'data-state': props.state }),
  }
  const runtime = { shallowEqual() { return true } }
  const parent = 'parent'
  const child = 'child-1'
  const bench = {
    engine, React: engine.React, primitives, runtime, opened,
    sessions: {
      ids: [parent, child],
      byId: {
        [parent]: { id: parent, running: true },
        [child]: { id: child, parentId: parent, origin: 'subagent', running: true },
      },
    },
  }
  bench.props = {
    node: { data: {
      name: 'audit', status: 'running',
      phases: [{ key: 'research', phase: 'Research', members: [
        { seq: 1, label: 'worker', childId: child, status: 'running' },
      ] }],
    } },
    sessionId: parent,
    useSessions(selector, equality) {
      if (equality !== runtime.shallowEqual) throw new Error('workflow selector omitted shallowEqual')
      return selector(bench.sessions)
    },
    openSession(id) { opened.push(id) },
    t: translate,
  }
  return bench
}
export function workflowRender(bench, component) {
  bench.tree = bench.engine.render(component, bench.props)
  return bench.tree
}
export function workflowText(tree) { return textOf(tree) }
export function workflowDisclosureRows(tree) {
  return findAll(tree, node => node.props?.['data-disclosure-row'] === true)
}
export function workflowFindAria(tree, label) {
  return find(tree, node => node.props?.['aria-label'] === label)
}
export function workflowFindText(tree, text) {
  return find(tree, node => node.props?.['data-disclosure-row'] === true && textOf(node) === text)
}
export function workflowClick(node) { return node.props.onClick() }
export function workflowKey(node, key) { return node.props.onKeyDown({ key, preventDefault() {} }) }
export function workflowSetData(bench, data) { bench.props = { ...bench.props, node: { data } } }
export function workflowSetSessions(bench, sessions) { bench.sessions = sessions }
export function workflowOpened(bench) { return bench.opened }
export function workflowNodes(tree, property) {
  return findAll(tree, node => node.props?.[property] !== undefined)
}
export function workflowStyleCount() {
  return document.querySelectorAll('style[data-plugin="@seekdeep-ai/seekdeep-client-ui-workflow-run"]').length
}

export function makeWorkflowPluginBench() {
  const ui = makeWorkflowBench()
  const effects = [], entries = [], definitions = [], opened = []
  const own = dispose => { effects.push(dispose); return dispose }
  const ctx = {
    effect(setup) { return own(setup()) },
    locale: { register() { return () => {} } },
    sessions: { open(id) { opened.push(id) } },
    conversationEvents: {
      register(definition) {
        definitions.push(definition)
        return own(() => definitions.splice(definitions.indexOf(definition), 1))
      },
    },
  }
  ctx.slots = {
    inject(name, install) { return own(install()) },
    register(options, component) {
      const entry = { options, component }
      entries.push(entry)
      return () => entries.splice(entries.indexOf(entry), 1)
    },
  }
  return { ...ui, ctx, effects, entries, definitions, pluginOpened: opened }
}
export function workflowPluginEntries(bench) { return bench.entries }
export function workflowPluginDefinitions(bench) { return bench.definitions }
export function workflowPluginInject(bench) { return bench.entries[0].options.inject() }
export function workflowPluginOpened(bench) { return bench.pluginOpened }
export function workflowPluginDispose(bench) {
  for (const dispose of bench.effects.splice(0).reverse()) dispose()
}
"#)]
extern "C" {
    fn makeWorkflowBench() -> JsValue;
    fn workflowRender(bench: &JsValue, component: &Function) -> JsValue;
    fn workflowText(tree: &JsValue) -> String;
    fn workflowDisclosureRows(tree: &JsValue) -> Array;
    fn workflowFindAria(tree: &JsValue, label: &str) -> JsValue;
    fn workflowFindText(tree: &JsValue, text: &str) -> JsValue;
    fn workflowClick(node: &JsValue) -> JsValue;
    fn workflowKey(node: &JsValue, key: &str) -> JsValue;
    fn workflowSetData(bench: &JsValue, data: &JsValue);
    fn workflowSetSessions(bench: &JsValue, sessions: &JsValue);
    fn workflowOpened(bench: &JsValue) -> Array;
    fn workflowNodes(tree: &JsValue, property: &str) -> Array;
    fn workflowStyleCount() -> u32;
    fn makeWorkflowPluginBench() -> JsValue;
    fn workflowPluginEntries(bench: &JsValue) -> Array;
    fn workflowPluginDefinitions(bench: &JsValue) -> Array;
    fn workflowPluginInject(bench: &JsValue) -> JsValue;
    fn workflowPluginOpened(bench: &JsValue) -> Array;
    fn workflowPluginDispose(bench: &JsValue);
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
    configure_client_ui_workflow_run(
        property(bench, "React"),
        property(bench, "primitives"),
        property(bench, "runtime"),
    )
    .unwrap();
    exported_workflow_run_panel_component()
        .unwrap()
        .dyn_into()
        .unwrap()
}

fn data(value: &str) -> JsValue {
    js_sys::JSON::parse(value).unwrap()
}

#[wasm_bindgen_test]
fn running_attention_navigation_and_terminal_folding_are_live() {
    let bench = makeWorkflowBench();
    let component = component(&bench);
    let running = workflowRender(&bench, &component);
    assert!(workflowText(&running).contains("worker"));
    let rows = workflowDisclosureRows(&running);
    assert_eq!(rows.length(), 2);
    for row in rows.iter() {
        assert!(property(&row, "role").is_undefined());
        assert!(property(&row, "aria-expanded").is_undefined());
    }
    let member = workflowFindAria(&running, "打开 worker");
    assert!(!member.is_undefined());
    assert!(workflowClick(&member).is_undefined());
    assert_eq!(
        workflowOpened(&bench).get(0).as_string().as_deref(),
        Some("child-1")
    );
    assert_eq!(workflowNodes(&running, "data-state").length(), 2);
    assert_eq!(workflowStyleCount(), 1);
    workflowSetSessions(
        &bench,
        &data(
            r#"{"ids":["parent","child-1"],"byId":{"child-1":{"parentId":"parent","running":false}}}"#,
        ),
    );
    let terminal_session = workflowRender(&bench, &component);
    assert!(workflowFindAria(&terminal_session, "打开 worker").is_undefined());

    workflowSetData(
        &bench,
        &data(
            r#"{"name":"audit","status":"running","phases":[{"key":"missing","phase":null,"members":[{"seq":1,"label":"done","childId":"child-1","status":"completed"}]}]}"#,
        ),
    );
    let phase_clean = workflowRender(&bench, &component);
    let phase_header = workflowFindText(&phase_clean, "未分阶段1 个成员已完成 1");
    assert!(
        !phase_header.is_undefined(),
        "{}",
        workflowText(&phase_clean)
    );
    assert_eq!(
        property(&phase_header, "aria-expanded").as_bool(),
        Some(false)
    );
    assert!(!workflowText(&phase_clean).contains("done"));
    workflowClick(&phase_header);
    let reviewed = workflowRender(&bench, &component);
    assert!(workflowText(&reviewed).contains("done"));

    workflowSetData(
        &bench,
        &data(
            r#"{"name":"audit","status":"completed","phases":[{"key":"missing","phase":null,"members":[{"seq":1,"label":"done","childId":"child-1","status":"completed"}]}]}"#,
        ),
    );
    let complete = workflowRender(&bench, &component);
    let run_header = workflowFindText(&complete, "audit1 个成员已完成");
    assert!(!run_header.is_undefined(), "{}", workflowText(&complete));
    assert_eq!(
        property(&run_header, "aria-expanded").as_bool(),
        Some(false)
    );
    workflowKey(&run_header, "Enter");
    let opened = workflowRender(&bench, &component);
    assert!(workflowText(&opened).contains("未分阶段"));
}

#[wasm_bindgen_test]
fn clean_cycles_empty_names_and_mixed_status_are_live() {
    let bench = makeWorkflowBench();
    let component = component(&bench);
    workflowSetData(
        &bench,
        &data(
            r#"{"name":"cycle","status":"running","phases":[{"key":"missing","phase":null,"members":[{"seq":1,"label":"first","childId":"child-1","status":"completed"}]}]}"#,
        ),
    );
    let first = workflowRender(&bench, &component);
    let phase_header = workflowFindText(&first, "未分阶段1 个成员已完成 1");
    workflowClick(&phase_header);
    assert!(workflowText(&workflowRender(&bench, &component)).contains("first"));
    workflowSetData(
        &bench,
        &data(
            r#"{"name":"cycle","status":"running","phases":[{"key":"missing","phase":null,"members":[{"seq":1,"label":"first","childId":"child-1","status":"completed"},{"seq":2,"label":"second","childId":"child-2","status":"completed"}]}]}"#,
        ),
    );
    let refolded = workflowRender(&bench, &component);
    assert!(!workflowText(&refolded).contains("first"));
    assert!(!workflowText(&refolded).contains("second"));

    workflowSetData(
        &bench,
        &data(
            r#"{"name":"repo-audit","status":"interrupted","phases":[{"key":"value:0:","phase":"","members":[{"seq":1,"label":"","childId":"child-1","status":"completed"},{"seq":2,"label":"interrupted","childId":"child-2","status":"interrupted"}]}]}"#,
        ),
    );
    let mixed = workflowRender(&bench, &component);
    assert!(workflowText(&mixed).contains("空阶段名"));
    assert!(workflowText(&mixed).contains("空成员名"));
    assert!(workflowText(&mixed).contains("已完成 1 · 已中断 1"));
    assert_eq!(workflowNodes(&mixed, "data-member-status").length(), 2);
    assert_eq!(workflowNodes(&mixed, "data-state").length(), 3);

    workflowSetData(
        &bench,
        &data(r#"{"name":"empty","status":"running","phases":[]}"#),
    );
    let empty_running = workflowRender(&bench, &component);
    assert!(workflowText(&empty_running).contains("没有启动成员"));
    workflowSetData(
        &bench,
        &data(r#"{"name":"empty","status":"completed","phases":[]}"#),
    );
    let empty_complete = workflowRender(&bench, &component);
    assert!(!workflowText(&empty_complete).contains("没有启动成员"));
}

#[wasm_bindgen_test]
fn plugin_registers_definition_keyed_renderer_navigation_and_disposes() {
    let bench = makeWorkflowPluginBench();
    configure_client_ui_workflow_run(
        property(&bench, "React"),
        property(&bench, "primitives"),
        property(&bench, "runtime"),
    )
    .unwrap();
    apply_client_ui_workflow_run(property(&bench, "ctx")).unwrap();
    assert_eq!(workflow_run_inject().length(), 4);
    assert_eq!(workflowPluginDefinitions(&bench).length(), 1);
    assert_eq!(workflowPluginEntries(&bench).length(), 1);
    let definition = workflowPluginDefinitions(&bench).get(0);
    assert_eq!(
        property(&definition, "kind").as_string().as_deref(),
        Some("workflow-run")
    );
    assert!(property(&definition, "buildViewNode").is_function());
    let entry = workflowPluginEntries(&bench).get(0);
    let options = property(&entry, "options");
    assert_eq!(
        property(&options, "name").as_string().as_deref(),
        Some("conversation.chat.node")
    );
    assert_eq!(
        property(&options, "key").as_string().as_deref(),
        Some("workflow-run")
    );
    let injected = workflowPluginInject(&bench);
    let open = property(&injected, "openSession")
        .dyn_into::<Function>()
        .unwrap();
    assert!(
        open.call1(&injected, &JsValue::from_str("child-9"))
            .unwrap()
            .is_undefined()
    );
    assert_eq!(
        workflowPluginOpened(&bench).get(0).as_string().as_deref(),
        Some("child-9")
    );
    workflowPluginDispose(&bench);
    assert_eq!(workflowPluginDefinitions(&bench).length(), 0);
    assert_eq!(workflowPluginEntries(&bench).length(), 0);
    apply_client_ui_workflow_run(property(&bench, "ctx")).unwrap();
    assert_eq!(workflowPluginDefinitions(&bench).length(), 1);
    assert_eq!(workflowPluginEntries(&bench).length(), 1);
    workflowPluginDispose(&bench);
    assert_eq!(workflowPluginDefinitions(&bench).length(), 0);
    assert_eq!(workflowPluginEntries(&bench).length(), 0);
}
