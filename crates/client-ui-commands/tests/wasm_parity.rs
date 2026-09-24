//! Live Rust/WASM command directory, service, popup controller, and view parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_ui_commands::{
    WasmCommandDirectory, apply_client_ui_commands, commands_inject, configure_client_ui_commands,
    exported_filter_options,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
class FakeNode {
  constructor(kind, props, children) {
    this.kind = kind
    this.props = props ?? {}
    this.children = children.flat(Infinity).filter(value => value !== null && value !== undefined && value !== false)
    this.parentElement = null
    this.scrolled = 0
    this.focused = 0
    for (const child of this.children) if (child instanceof FakeNode) child.parentElement = this
  }
  contains(target) { return target === this || this.children.some(child => child instanceof FakeNode && child.contains(target)) }
  querySelector(selector) {
    if (selector === '[aria-selected="true"]') return commandAll(this, node => node.props?.['aria-selected'] === true)[0] ?? null
    return null
  }
  scrollIntoView() { this.scrolled++ }
  focus() { this.focused++; document.activeElement = this }
}
globalThis.Node = FakeNode

const styles = [], documentListeners = new Map()
if (typeof globalThis.document === 'undefined') globalThis.document = {}
Object.assign(document, {
  currentScript: null,
  activeElement: null,
  body: new FakeNode('body', {}, []),
  querySelector(selector) {
    const match = /^style\[data-plugin=(.+)\]$/.exec(selector)
    if (!match) return null
    const plugin = JSON.parse(match[1])
    return styles.find(node => node.attributes['data-plugin'] === plugin) ?? null
  },
  querySelectorAll(selector) { const node = this.querySelector(selector); return node === null ? [] : [node] },
  createElement(kind) { return { kind, attributes: {}, textContent: '', setAttribute(name, value) { this.attributes[name] = value } } },
  head: { appendChild(node) { styles.push(node); return node } },
  addEventListener(name, listener) { const rows = documentListeners.get(name) ?? new Set(); rows.add(listener); documentListeners.set(name, rows) },
  removeEventListener(name, listener) { documentListeners.get(name)?.delete(listener) },
})
document.activeElement = document.body

const EN = {
  'search.placeholder': 'Search…', 'search.aria': 'Filter options',
  'status.loading': 'Loading options…', 'status.applying': 'Applying…',
  'status.empty': 'No options', 'overlay.aria': '/{command} options',
  'listbox.aria': '/{command} matches', retry: 'Retry',
}
function commandTranslate(key, values = {}) {
  return (EN[key] ?? key).replace(/\{([^}]+)\}/g, (_, field) => String(values[field]))
}

function hookRuntime() {
  const refs = [], effects = []
  let refCursor = 0, effectCursor = 0, pending = []
  const Fragment = Symbol('Fragment')
  const React = {
    Fragment,
    createElement(kind, props, ...children) {
      const node = new FakeNode(kind, props ?? {}, children)
      if (props?.ref && typeof props.ref === 'object') props.ref.current = node
      else if (typeof props?.ref === 'function') props.ref(node)
      return node
    },
    useRef(initial) { const index = refCursor++; if (!(index in refs)) refs[index] = { current: initial }; return refs[index] },
    useEffect(run, deps) {
      const index = effectCursor++
      const old = effects[index]
      const changed = old === undefined || deps.length !== old.deps.length || deps.some((value, i) => !Object.is(value, old.deps[i]))
      if (changed) pending.push({ index, run, deps: [...deps], old })
    },
    useSyncExternalStore(_subscribe, getSnapshot) { return getSnapshot() },
  }
  return {
    React,
    render(component, props) {
      refCursor = 0; effectCursor = 0; pending = []
      const tree = component(props)
      for (const effect of pending) {
        effect.old?.cleanup?.()
        const cleanup = effect.run()
        effects[effect.index] = { deps: effect.deps, cleanup: typeof cleanup === 'function' ? cleanup : undefined }
      }
      return tree
    },
    dispose() { for (const effect of effects.reverse()) effect?.cleanup?.() },
  }
}

function own(effects, setup) {
  const returned = setup()
  const dispose = typeof returned === 'function' ? returned : () => {}
  effects.push(dispose)
  return dispose
}
function remove(array, value) { const index = array.indexOf(value); if (index >= 0) array.splice(index, 1) }
function commandText(node) {
  if (node === null || node === undefined || node === false) return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(commandText).join('')
  return (node.children ?? []).map(commandText).join('')
}
function commandAll(node, predicate, rows = []) {
  if (node === null || node === undefined || node === false || typeof node === 'string' || typeof node === 'number') return rows
  if (!Array.isArray(node) && predicate(node)) rows.push(node)
  for (const child of Array.isArray(node) ? node : node.children ?? []) commandAll(child, predicate, rows)
  return rows
}
function commandOne(node, predicate) { return commandAll(node, predicate)[0] }

export function commandSession(id) { return { sessionId: id, marker: { id } } }
export function commandRequest(query, position) { return { query, position, signal: new AbortController().signal } }
export function commandMenuPick(name, session, revision = 7) {
  return { candidate: { name }, session, position: 'leading', via: 'menu', span: { start: 0, end: name.length + 1, draftRev: revision } }
}
export function commandSegment(revision = 7) { return { via: 'menu', span: { start: 0, end: 4, draftRev: revision } } }
export function commandAbortSignal() { return new AbortController().signal }
export function commandMakeContribution(name, description, rows = []) {
  return { name, description, available: () => true, ui: { kind: 'popupSelect', options: () => Promise.resolve(rows), onSelect: () => Promise.resolve() } }
}

export function makeCommandBench() {
  const hooks = hookRuntime(), rootEffects = [], scopes = new Map(), addressed = new Set()
  const sources = new Map(), entries = [], locales = [], services = new Map(), remoteEvents = new Map(), contextEvents = new Map()
  const listCalls = [], executeCalls = [], consumes = [], notices = [], executed = [], warnings = [], injects = []
  const host = new Map([['s1', [
    { name: 'bare', description: 'Bare command' },
    { name: 'with-input', description: 'Input command', input: { hint: 'value' } },
    { name: 'alpha-beta', description: 'Boundary match' },
  ]]])
  const malformed = new Set(), rpcFailures = new Map(), executedListeners = []
  let focusCount = 0
  const commands = {
    list(sessionId) {
      listCalls.push(sessionId)
      if (rpcFailures.has('list:' + sessionId)) return Promise.resolve({ ok: false, error: { code: 'offline', message: rpcFailures.get('list:' + sessionId) } })
      return Promise.resolve({ ok: true, value: host.get(sessionId) ?? [] })
    },
    execute(sessionId, line) {
      executeCalls.push({ sessionId, line })
      if (rpcFailures.has('execute:' + line)) return Promise.resolve({ ok: false, error: { code: 'offline', message: rpcFailures.get('execute:' + line) } })
      if (malformed.has(line)) return Promise.resolve({ ok: true, value: undefined })
      return Promise.resolve({ ok: true, value: { commandId: 'cmd-1', result: { kind: 'success' } } })
    },
  }
  const addEvent = (map, name, listener) => {
    const rows = map.get(name) ?? new Set(); rows.add(listener); map.set(name, rows)
    const dispose = () => rows.delete(listener); rootEffects.push(dispose); return dispose
  }
  const sessionScope = id => {
    const effects = []
    const actx = {
      id,
      effect(setup) { return own(effects, setup) },
      bail(_scope, event, payload) { consumes.push({ id, event, payload }); return true },
      get(name) {
        if (name !== 'conversation') return undefined
        return { input: { for: () => ({ notify(level, text) { notices.push({ id, level, text }) } }) } }
      },
      dispose() { for (const dispose of effects.splice(0).reverse()) dispose() },
    }
    scopes.set(id, actx)
    return actx
  }
  const sessions = {
    scope(id) { return scopes.get(id) },
    scopeOf(actx) { return actx?.id },
    subagentAddress(id) { return addressed.has(id) ? { parentSessionId: 'parent', childSessionId: id, mode: 'continuable' } : undefined },
  }
  const remote = { commands, $on(name, listener) { return addEvent(remoteEvents, name, listener) } }
  const slots = {
    inject(name, install) { injects.push(name); return own(rootEffects, install) },
    register(options, component) { const row = { options, component }; entries.push(row); return () => remove(entries, row) },
  }
  const ctx = {
    inputTriggers: { registerSource(source) { sources.set(source.name, source); return () => sources.delete(source.name) } },
    sessions, remote, slots,
    locale: { register(namespace, dictionaries) { const row = { namespace, dictionaries }; locales.push(row); return () => remove(locales, row) } },
    reflect: { provide(name, value) { services.set(name, value); ctx[name] = value; return () => { services.delete(name); delete ctx[name] } } },
    events: { dispatch(mode, args) { if (mode !== 'emit') throw new Error('unexpected dispatch mode'); executed.push(args); return executedListeners } },
    logger: { warn(...args) { warnings.push(args) } },
    effect(setup) { return own(rootEffects, setup) },
    inject(dependencies, callback) { injects.push([...dependencies]); callback(ctx) },
    on(name, listener) { return addEvent(contextEvents, name, listener) },
  }
  return {
    hooks, React: hooks.React,
    primitives: { useAnchoredMaxHeight: () => 240, IconCheckOutline16: 'IconCheckOutline16', RiskConfirmation: 'RiskConfirmation' },
    ctx, rootEffects, scopes, addressed, sources, entries, locales, services, remoteEvents, contextEvents,
    listCalls, executeCalls, consumes, notices, executed, warnings, injects, host, malformed, rpcFailures, executedListeners,
    mint: sessionScope,
    addExecuted(listener) { executedListeners.push(listener) },
    bindFocus() { focusCount++ },
    get focusCount() { return focusCount },
  }
}

export function commandBenchMint(bench, id) { return bench.mint(id) }
export function commandBenchSource(bench) { return bench.sources.get('command') }
export function commandBenchService(bench) { return bench.ctx.commandUi }
export function commandBenchEntry(bench) { return bench.entries[0] }
export function commandBenchCalls(bench, name) { return bench[name] }
export function commandBenchAddress(bench, id) { bench.addressed.add(id) }
export function commandBenchSetMalformed(bench, line) { bench.malformed.add(line) }
export function commandBenchDispatch(bench, map, name, first, second) { for (const listener of bench[map].get(name) ?? []) listener(first, second) }
export function commandBenchAddExecuted(bench, kind) {
  if (kind === 'capture') bench.addExecuted((sessionId, name, result) => { bench.executed.push(['listener', sessionId, name, result]) })
  if (kind === 'throw') bench.addExecuted(() => { throw new Error('observer exploded') })
  if (kind === 'reject') bench.addExecuted(() => Promise.reject(new Error('observer rejected')))
}
export function commandBenchDispose(bench) { bench.hooks.dispose(); for (const dispose of bench.rootEffects.splice(0).reverse()) dispose() }
export function commandScopeDispose(scope) { scope.dispose() }
export function commandTick() { return Promise.resolve().then(() => Promise.resolve()).then(() => Promise.resolve()).then(() => Promise.resolve()) }
export function commandStyleCount() { return styles.filter(node => node.attributes['data-plugin'] === '@seekdeep-ai/seekdeep-client-ui-commands').length }

export function commandMakePopupSpec(bench, context, mode = 'gated') {
  let loads = 0
  const selected = []
  const spec = {
    kind: 'popupSelect',
    options(received, signal) {
      loads++
      bench.popupContextExact = Object.is(received, context)
      bench.popupSignal = signal
      if (mode === 'fail-once' && loads === 1) return Promise.reject(new Error('options offline'))
      if (mode === 'empty') return Promise.resolve([])
      return Promise.resolve([
        { id: 'dark', label: 'Dark', detail: 'Night', active: true },
        { id: 'danger', label: 'Danger', confirmation: { title: 'Confirm', description: 'Risk', acknowledgeLabel: 'Acknowledge', cancelLabel: 'Cancel', confirmLabel: 'Enable' } },
      ])
    },
    onSelect(option, received) { bench.selectContextExact = Object.is(received, context); selected.push(option.id); return Promise.resolve() },
    get loads() { return loads }, selected,
  }
  return spec
}
export function commandPopupExact(bench, which) { return bench[which] === true }
export function commandRender(bench, component, popup) { return bench.hooks.render(component, { popup, t: commandTranslate }) }
export function commandTreeInput(tree) { return commandOne(tree, node => node.kind === 'input') }
export function commandTreeRow(tree, text) { return commandOne(tree, node => node.props?.role === 'option' && commandText(node).includes(text)) }
export function commandTreeAlert(tree) { return commandOne(tree, node => node.props?.role === 'alert') }
export function commandTreeButton(tree, text) { return commandOne(tree, node => node.kind === 'button' && commandText(node).includes(text)) }
export function commandTreeConfirmation(tree) { return commandOne(tree, node => node.kind === 'RiskConfirmation') }
export function commandTreeText(tree) { return commandText(tree) }
export function commandNodeProp(node, key) { return node?.props?.[key] }
export function commandChange(node, value) { node.props.onChange({ currentTarget: { value } }) }
export function commandKey(node, key) { (typeof node?.props?.onKeyDown === 'function' ? node : commandOne(node, child => typeof child.props?.onKeyDown === 'function')).props.onKeyDown({ key, preventDefault() {} }) }
export function commandClick(node) { return node.props.onClick() }
export function commandHover(node) { return node.props.onMouseEnter() }
export function commandPointerOutside() { for (const listener of documentListeners.get('pointerdown') ?? []) listener({ target: document.body }) }
export function commandActiveElement() { return document.activeElement }

export function commandDirectoryFetchBench() {
  const calls = []
  let fail
  const fetch = sessionId => { calls.push(sessionId); return fail ? Promise.reject(new Error(fail)) : Promise.resolve([{ name: 'one', description: 'One' }]) }
  return { fetch, calls, setFail(value) { fail = value } }
}
export function commandDirectorySetFail(bench, value) { bench.setFail(value) }
"#)]
extern "C" {
    fn makeCommandBench() -> JsValue;
    fn commandSession(id: &str) -> JsValue;
    fn commandRequest(query: &str, position: &str) -> JsValue;
    fn commandMenuPick(name: &str, session: &JsValue, revision: u32) -> JsValue;
    fn commandSegment(revision: u32) -> JsValue;
    fn commandAbortSignal() -> JsValue;
    fn commandMakeContribution(name: &str, description: &str, rows: &Array) -> JsValue;
    fn commandBenchMint(bench: &JsValue, id: &str) -> JsValue;
    fn commandBenchSource(bench: &JsValue) -> JsValue;
    fn commandBenchService(bench: &JsValue) -> JsValue;
    fn commandBenchEntry(bench: &JsValue) -> JsValue;
    fn commandBenchCalls(bench: &JsValue, name: &str) -> Array;
    fn commandBenchAddress(bench: &JsValue, id: &str);
    fn commandBenchSetMalformed(bench: &JsValue, line: &str);
    fn commandBenchDispatch(
        bench: &JsValue,
        map: &str,
        name: &str,
        first: &JsValue,
        second: &JsValue,
    );
    fn commandBenchAddExecuted(bench: &JsValue, kind: &str);
    fn commandBenchDispose(bench: &JsValue);
    fn commandScopeDispose(scope: &JsValue);
    fn commandTick() -> Promise;
    fn commandStyleCount() -> u32;
    fn commandMakePopupSpec(bench: &JsValue, context: &JsValue, mode: &str) -> JsValue;
    fn commandPopupExact(bench: &JsValue, which: &str) -> bool;
    fn commandRender(bench: &JsValue, component: &Function, popup: &JsValue) -> JsValue;
    fn commandTreeInput(tree: &JsValue) -> JsValue;
    fn commandTreeRow(tree: &JsValue, text: &str) -> JsValue;
    fn commandTreeAlert(tree: &JsValue) -> JsValue;
    fn commandTreeButton(tree: &JsValue, text: &str) -> JsValue;
    fn commandTreeConfirmation(tree: &JsValue) -> JsValue;
    fn commandTreeText(tree: &JsValue) -> String;
    fn commandNodeProp(node: &JsValue, key: &str) -> JsValue;
    fn commandChange(node: &JsValue, value: &str);
    fn commandKey(node: &JsValue, key: &str);
    fn commandClick(node: &JsValue) -> JsValue;
    fn commandHover(node: &JsValue);
    fn commandPointerOutside();
    fn commandActiveElement() -> JsValue;
    fn commandDirectoryFetchBench() -> JsValue;
    fn commandDirectorySetFail(bench: &JsValue, value: &str);
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn call(value: &JsValue, key: &str) -> Function {
    property(value, key).dyn_into().unwrap()
}

fn configure(bench: &JsValue) {
    configure_client_ui_commands(property(bench, "React"), property(bench, "primitives")).unwrap();
}

async fn await_value(value: JsValue) -> JsValue {
    JsFuture::from(Promise::resolve(&value)).await.unwrap()
}

#[wasm_bindgen_test(async)]
async fn public_directory_is_live_single_flight_and_reports_source_errors() {
    let bench = commandDirectoryFetchBench();
    let directory = WasmCommandDirectory::new(property(&bench, "fetch").dyn_into().unwrap());
    assert_eq!(directory.status("s1".to_owned()), "cold");
    directory.warm("s1".to_owned());
    assert_eq!(directory.status("s1".to_owned()), "pending");
    JsFuture::from(commandTick()).await.unwrap();
    assert_eq!(directory.status("s1".to_owned()), "ready");
    assert_eq!(
        property(
            &directory
                .resolve("s1".to_owned(), "one".to_owned())
                .unwrap(),
            "description"
        )
        .as_string()
        .as_deref(),
        Some("One")
    );
    assert_eq!(Array::from(&property(&bench, "calls")).length(), 1);
    commandDirectorySetFail(&bench, "offline");
    JsFuture::from(directory.refresh("s1".to_owned()))
        .await
        .unwrap();
    assert_eq!(directory.status("s1".to_owned()), "failed");
    let error = JsFuture::from(directory.ensure_ready("s1".to_owned(), commandAbortSignal()))
        .await
        .unwrap_err();
    assert!(
        error
            .as_string()
            .unwrap_or_else(|| format!("{error:?}"))
            .contains("command directory warmup failed: offline")
    );
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)]
async fn assembled_plugin_merges_dispatches_executes_contains_observer_failures_and_tears_down() {
    let bench = makeCommandBench();
    configure(&bench);
    apply_client_ui_commands(property(&bench, "ctx")).unwrap();
    assert_eq!(
        commands_inject()
            .iter()
            .map(|value| value.as_string().unwrap())
            .collect::<Vec<_>>(),
        [
            "inputTriggers",
            "sessions",
            "remote",
            "remote.commands",
            "locale"
        ]
    );
    assert_eq!(commandStyleCount(), 1);
    assert_eq!(commandBenchCalls(&bench, "entries").length(), 1);
    assert_eq!(commandBenchCalls(&bench, "locales").length(), 1);
    let scope = commandBenchMint(&bench, "s1");
    let source = commandBenchSource(&bench);
    let session = commandSession("s1");
    let service = commandBenchService(&bench);
    let contribution = commandMakeContribution("client", "Client command", &Array::new());
    let contribution_disposer = call(&service, "register")
        .call1(&service, &contribution)
        .unwrap();
    let candidates = call(&source, "candidates")
        .call2(&source, &session, &commandRequest("", "leading"))
        .unwrap();
    let candidates = Array::from(&await_value(candidates).await);
    assert_eq!(candidates.length(), 4);
    assert_eq!(
        property(&candidates.get(3), "name").as_string().as_deref(),
        Some("client")
    );
    let inline = call(&source, "candidates")
        .call2(&source, &session, &commandRequest("", "inline"))
        .unwrap();
    let inline = Array::from(&await_value(inline).await);
    assert_eq!(inline.length(), 3);
    assert!(inline.iter().all(
        |candidate| property(&candidate, "name").as_string().as_deref() != Some("with-input")
    ));

    let claim = call(&source, "onPick")
        .call1(&source, &commandMenuPick("with-input", &session, 9))
        .unwrap();
    let claim = property(&claim, "claim");
    assert_eq!(
        property(&claim, "token").as_string().as_deref(),
        Some("/with-input ")
    );
    assert_eq!(
        property(&claim, "hint").as_string().as_deref(),
        Some("value")
    );
    commandBenchAddExecuted(&bench, "capture");
    commandBenchAddExecuted(&bench, "throw");
    commandBenchAddExecuted(&bench, "reject");
    let outcome = call(&claim, "submit")
        .call2(&claim, &JsValue::from_str("hello"), &scope)
        .unwrap();
    assert_eq!(
        property(&await_value(outcome).await, "kind")
            .as_string()
            .as_deref(),
        Some("success")
    );
    JsFuture::from(commandTick()).await.unwrap();
    assert_eq!(commandBenchCalls(&bench, "executeCalls").length(), 1);
    assert_eq!(commandBenchCalls(&bench, "warnings").length(), 4);

    let handled = call(&source, "onPick")
        .call1(&source, &commandMenuPick("bare", &session, 11))
        .unwrap();
    assert_eq!(handled.as_string().as_deref(), Some("handled"));
    JsFuture::from(commandTick()).await.unwrap();
    let consumes = commandBenchCalls(&bench, "consumes");
    assert_eq!(consumes.length(), 1);
    let revision = property(
        &property(&property(&consumes.get(0), "payload"), "guard"),
        "span",
    );
    assert_eq!(property(&revision, "draftRev").as_f64(), Some(11.0));
    assert_eq!(property(&revision, "draftRev").js_typeof(), "number");

    commandBenchSetMalformed(&bench, "/bare");
    call(&source, "onPick")
        .call1(&source, &commandMenuPick("bare", &session, 12))
        .unwrap();
    JsFuture::from(commandTick()).await.unwrap();
    assert!(
        commandBenchCalls(&bench, "notices")
            .iter()
            .any(|notice| property(&notice, "text").as_string().as_deref()
                == Some("unknown or malformed command: /bare"))
    );

    let collision = commandMakeContribution("bare", "Collision", &Array::new());
    let collision_disposer = call(&service, "register")
        .call1(&service, &collision)
        .unwrap();
    let collision_result = call(&source, "candidates")
        .call2(&source, &session, &commandRequest("", "leading"))
        .unwrap();
    let error = JsFuture::from(Promise::resolve(&collision_result))
        .await
        .unwrap_err();
    assert!(format!("{error:?}").contains("collides with a host command"));
    collision_disposer
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();

    commandBenchMint(&bench, "child");
    commandBenchAddress(&bench, "child");
    let child = commandSession("child");
    let child_rows = call(&source, "candidates")
        .call2(&source, &child, &commandRequest("", "leading"))
        .unwrap();
    assert_eq!(Array::from(&await_value(child_rows).await).length(), 1);
    assert_eq!(commandBenchCalls(&bench, "listCalls").length(), 1);

    commandBenchDispatch(
        &bench,
        "remoteEvents",
        "commands/change",
        &JsValue::UNDEFINED,
        &JsValue::UNDEFINED,
    );
    JsFuture::from(commandTick()).await.unwrap();
    assert_eq!(commandBenchCalls(&bench, "listCalls").length(), 2);
    contribution_disposer
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    commandScopeDispose(&scope);
    commandBenchDispose(&bench);
    assert!(commandBenchService(&bench).is_undefined());
    assert_eq!(commandBenchCalls(&bench, "entries").length(), 0);
    assert_eq!(commandBenchCalls(&bench, "locales").length(), 0);
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)]
async fn popup_view_preserves_context_focus_confirmation_filter_retry_and_outside_dismiss() {
    let bench = makeCommandBench();
    configure(&bench);
    apply_client_ui_commands(property(&bench, "ctx")).unwrap();
    let scope = commandBenchMint(&bench, "s1");
    let service = commandBenchService(&bench);
    let focus = property(&bench, "bindFocus")
        .dyn_into::<Function>()
        .unwrap();
    call(&service, "bindComposerFocus")
        .call2(&service, &JsValue::from_str("s1"), &focus)
        .unwrap();
    let entry = commandBenchEntry(&bench);
    let options = property(&entry, "options");
    let popup = call(&options, "inject")
        .call1(&options, &JsValue::from_str("s1"))
        .unwrap();
    let popup = property(&popup, "popup");
    let context = commandSession("s1");
    let spec = commandMakePopupSpec(&bench, &context, "gated");
    call(&popup, "open")
        .call4(
            &popup,
            &JsValue::from_str("theme"),
            &spec,
            &context,
            &commandSegment(31),
        )
        .unwrap();
    JsFuture::from(commandTick()).await.unwrap();
    let popup_state = property(&popup, "state");
    let popup_snapshot = call(&popup_state, "getSnapshot")
        .call0(&popup_state)
        .unwrap();
    assert!(
        commandPopupExact(&bench, "popupContextExact"),
        "popup context was not exact; state error={:?}",
        property(&popup_snapshot, "error")
    );
    let component = property(&entry, "component")
        .dyn_into::<Function>()
        .unwrap();
    let tree = commandRender(&bench, &component, &popup);
    assert!(commandTreeText(&tree).contains("Dark"));
    let input = commandTreeInput(&tree);
    assert!(Object::is(&commandActiveElement(), &input));
    commandChange(&input, "night");
    let filtered = commandRender(&bench, &component, &popup);
    assert!(commandTreeText(&filtered).contains("Dark"));
    assert!(!commandTreeText(&filtered).contains("Danger"));
    commandChange(&commandTreeInput(&filtered), "");
    let rows = commandRender(&bench, &component, &popup);
    let danger = commandTreeRow(&rows, "Danger");
    commandHover(&danger);
    commandClick(&danger);
    let confirmation = commandRender(&bench, &component, &popup);
    let confirmation = commandTreeConfirmation(&confirmation);
    assert_eq!(
        commandNodeProp(&confirmation, "title")
            .as_string()
            .as_deref(),
        Some("Confirm")
    );
    commandNodeProp(&confirmation, "onAcknowledgedChange")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &JsValue::TRUE)
        .unwrap();
    commandNodeProp(&confirmation, "onConfirm")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    JsFuture::from(commandTick()).await.unwrap();
    assert!(commandPopupExact(&bench, "selectContextExact"));
    assert_eq!(property(&bench, "focusCount").as_f64(), Some(1.0));
    let consumes = commandBenchCalls(&bench, "consumes");
    let draft_rev = property(
        &property(&property(&consumes.get(0), "payload"), "guard"),
        "span",
    );
    assert_eq!(property(&draft_rev, "draftRev").as_f64(), Some(31.0));

    let failed = commandMakePopupSpec(&bench, &context, "fail-once");
    call(&popup, "open")
        .call4(
            &popup,
            &JsValue::from_str("theme"),
            &failed,
            &context,
            &commandSegment(32),
        )
        .unwrap();
    JsFuture::from(commandTick()).await.unwrap();
    let failed_tree = commandRender(&bench, &component, &popup);
    assert!(commandTreeText(&commandTreeAlert(&failed_tree)).contains("options offline"));
    commandClick(&commandTreeButton(&failed_tree, "Retry"));
    JsFuture::from(commandTick()).await.unwrap();
    let recovered = commandRender(&bench, &component, &popup);
    assert!(commandTreeText(&recovered).contains("Dark"));
    commandPointerOutside();
    assert!(commandRender(&bench, &component, &popup).is_null());

    call(&popup, "open")
        .call4(
            &popup,
            &JsValue::from_str("theme"),
            &spec,
            &context,
            &commandSegment(33),
        )
        .unwrap();
    JsFuture::from(commandTick()).await.unwrap();
    let keyboard = commandRender(&bench, &component, &popup);
    commandKey(&keyboard, "Escape");
    assert_eq!(property(&bench, "focusCount").as_f64(), Some(2.0));
    commandScopeDispose(&scope);
    commandBenchDispose(&bench);
}

#[wasm_bindgen_test]
fn exported_filter_keeps_blank_array_and_filtered_row_identity() {
    let first = Object::new();
    Reflect::set(&first, &"id".into(), &"one".into()).unwrap();
    Reflect::set(&first, &"label".into(), &"Alpha".into()).unwrap();
    let second = Object::new();
    Reflect::set(&second, &"id".into(), &"two".into()).unwrap();
    Reflect::set(&second, &"label".into(), &"Beta".into()).unwrap();
    Reflect::set(&second, &"detail".into(), &"Night".into()).unwrap();
    let options = Array::of2(&first, &second);
    let blank = exported_filter_options(options.clone().into(), "  ".to_owned()).unwrap();
    assert!(Object::is(&blank, options.as_ref()));
    let filtered =
        Array::from(&exported_filter_options(options.clone().into(), "NIGHT".to_owned()).unwrap());
    assert_eq!(filtered.length(), 1);
    assert!(Object::is(&filtered.get(0), second.as_ref()));
}
