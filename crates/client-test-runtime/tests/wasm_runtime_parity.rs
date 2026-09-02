//! Assembled browser Slot test-runtime lifecycle and renderer parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_test_runtime::{WasmSlotTestRuntime, configure_client_test_runtime};
use seekdeep_client_web_react::{
    configure_client_web_react, create_selector_shim, create_slot_renderer,
};
use seekdeep_cordis::{configure_context_wrapper, create_context};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
class FakeText {
  constructor(value) { this.value = String(value) }
  get textContent() { return this.value }
  get innerHTML() { return this.value }
}

class FakeHTMLElement {
  constructor(tagName = 'div', props = {}, children = []) {
    this.tagName = String(tagName).toUpperCase()
    this.props = { ...props }
    this.children = children.flat(Infinity).filter(value => value !== null && value !== undefined && value !== false)
  }
  get childNodes() { return this.children }
  get textContent() { return this.children.map(child => child?.textContent ?? String(child ?? '')).join('') }
  get innerHTML() { return this.children.map(child => child?.innerHTML ?? String(child ?? '')).join('') }
  set innerHTML(value) { if (value === '') this.children = [] }
  getAttribute(name) {
    if (name === 'class') return this.props.className ?? this.props.class ?? null
    return Object.hasOwn(this.props, name) ? String(this.props[name]) : null
  }
  setAttribute(name, value) { this.props[name] = String(value) }
  replaceChildren(...children) { this.children = children.flat(Infinity) }
  matches(selector) {
    const slot = selector.match(/^\[data-slot="([^"]+)"\]$/)
    if (slot) return this.getAttribute('data-slot') === slot[1]
    const testId = selector.match(/^\[data-testid="([^"]+)"\]$/)
    if (testId) return this.getAttribute('data-testid') === testId[1]
    return this.tagName.toLowerCase() === selector.toLowerCase()
  }
  querySelectorAll(selector) {
    const found = []
    const visit = node => {
      if (!(node instanceof FakeHTMLElement)) return
      for (const child of node.children) {
        if (child instanceof FakeHTMLElement && child.matches(selector)) found.push(child)
        visit(child)
      }
    }
    visit(this)
    return found
  }
  querySelector(selector) { return this.querySelectorAll(selector)[0] ?? null }
}

export function runtimeContextWrapper() {
  const filter = Symbol('Context.filter')
  const constructor = { filter }
  return core => {
    let ctx
    ctx = new Proxy(core, {
      get(target, key, receiver) {
        if (key === 'emit') return (name, ...args) => target.emitArgs(name, args)
        if (key === 'parallel') return (name, ...args) => target.parallelArgs(name, args)
        if (key === 'serial') return (name, ...args) => target.serialArgs(name, args)
        if (key === 'bail') return (name, ...args) => target.bailArgs(name, args)
        if (key === 'get') return name => target.get(name)
        if (key === 'constructor') return constructor
        if (Reflect.has(target, key)) {
          const value = Reflect.get(target, key, receiver)
          return typeof value === 'function' ? value.bind(target) : value
        }
        const metadata = target.metaGet(key)
        if (metadata !== undefined) return metadata
        return typeof key === 'string' ? target.get(key) : undefined
      },
    })
    return ctx
  }
}

export function runtimeBench() {
  const hooks = new WeakMap()
  const renderedViews = []
  const storage = new Map()
  let activeComponent
  let activeView
  let cursor = 0
  let serializerRegistrations = 0
  let storageClears = 0

  class Component { constructor(props) { this.props = props; this.state = {} } }
  const React = {
    Component,
    Fragment: Symbol('Fragment'),
    createContext(initial) {
      const context = { current: initial }
      function Provider() {}
      Provider.__context = context
      context.Provider = Provider
      return context
    },
    useContext(context) { return context.current },
    useRef(initial) {
      let row = hooks.get(activeComponent)
      if (!row) hooks.set(activeComponent, row = [])
      const seat = cursor++
      if (!(seat in row)) row[seat] = { current: initial }
      return row[seat]
    },
    useState(initial) {
      let row = hooks.get(activeComponent)
      if (!row) hooks.set(activeComponent, row = [])
      const seat = cursor++
      if (!(seat in row)) row[seat] = initial
      const view = activeView
      return [row[seat], value => {
        row[seat] = typeof value === 'function' ? value(row[seat]) : value
        view?.rerender()
      }]
    },
    useMemo(factory, deps) {
      let row = hooks.get(activeComponent)
      if (!row) hooks.set(activeComponent, row = [])
      const seat = cursor++
      const prior = row[seat]
      if (!prior || deps.some((value, index) => !Object.is(value, prior.deps[index]))) {
        row[seat] = { deps: [...deps], value: factory() }
      }
      return row[seat].value
    },
    useSyncExternalStore(subscribe, getSnapshot) {
      let row = hooks.get(activeComponent)
      if (!row) hooks.set(activeComponent, row = [])
      const seat = cursor++
      if (!(seat in row)) {
        const view = activeView
        const cleanup = subscribe(() => view?.rerender())
        row[seat] = { cleanup }
        view?.cleanups.push(cleanup)
      }
      return getSnapshot()
    },
    createElement(kind, props, ...children) {
      props = { ...(props ?? {}) }
      if (children.length === 1) props.children = children[0]
      else if (children.length > 1) props.children = children
      return { kind, props, children }
    },
  }

  const materialize = node => {
    if (node === undefined || node === null || node === false) return []
    if (Array.isArray(node)) return node.flatMap(materialize)
    if (typeof node !== 'object' || !('kind' in node)) return [new FakeText(node)]
    const { kind, props } = node
    if (kind?.__context) {
      const context = kind.__context
      const prior = context.current
      context.current = props.value
      try { return materialize(props.children) } finally { context.current = prior }
    }
    if (typeof kind === 'function' && kind.prototype instanceof Component) {
      const instance = new kind(props)
      try { return materialize(instance.render()) }
      catch (error) {
        instance.state = { ...instance.state, ...kind.getDerivedStateFromError(error) }
        instance.componentDidCatch?.(error)
        return materialize(instance.render())
      }
    }
    if (typeof kind === 'function') {
      const priorComponent = activeComponent
      const priorCursor = cursor
      activeComponent = kind
      cursor = 0
      try { return materialize(kind(props)) }
      finally { activeComponent = priorComponent; cursor = priorCursor }
    }
    if (typeof kind === 'symbol') return materialize(props.children)
    return [new FakeHTMLElement(kind, props, node.children.flatMap(materialize))]
  }

  const reconcile = (prior, next) => {
    if (prior instanceof FakeText && next instanceof FakeText) {
      prior.value = next.value
      return prior
    }
    if (prior instanceof FakeHTMLElement && next instanceof FakeHTMLElement
        && prior.tagName === next.tagName
        && prior.getAttribute('data-slot') === next.getAttribute('data-slot')) {
      prior.props = next.props
      prior.children = next.children.map((child, index) => reconcile(prior.children[index], child))
      return prior
    }
    return next
  }

  const render = node => {
    const container = new FakeHTMLElement('div')
    const view = {
      node, container, cleanups: [], mounted: true,
      rerender() {
        if (!view.mounted) return
        const prior = activeView
        activeView = view
        try {
          const next = materialize(view.node)
          container.replaceChildren(...next.map((child, index) => reconcile(container.children[index], child)))
        }
        finally { activeView = prior }
      },
      unmount() {
        if (!view.mounted) return
        view.mounted = false
        for (const cleanup of view.cleanups.splice(0)) cleanup()
        container.replaceChildren()
      },
      getByTestId(id) {
        const value = container.querySelector(`[data-testid="${id}"]`)
        if (value === null) throw new Error(`no element with test id ${id}`)
        return value
      },
      queryByTestId: id => container.querySelector(`[data-testid="${id}"]`),
    }
    renderedViews.push(view)
    view.rerender()
    return view
  }
  const within = container => ({
    getByTestId(id) {
      const value = container.querySelector(`[data-testid="${id}"]`)
      if (value === null) throw new Error(`no element with test id ${id}`)
      return value
    },
    queryByTestId: id => container.querySelector(`[data-testid="${id}"]`),
  })
  const produce = (base, mutator) => {
    const next = Array.isArray(base) ? [...base] : { ...base }
    for (const key of ['ids', 'items', 'pending', 'queue', 'archivedSessionIds']) {
      if (Array.isArray(base?.[key])) next[key] = [...base[key]]
    }
    for (const key of ['byId', 'subagentsByParent', 'jobsBySession']) {
      if (base?.[key] && typeof base[key] === 'object') next[key] = { ...base[key] }
    }
    mutator(next)
    return next
  }
  const act = callback => callback()
  const stabilize = async callback => {
    await callback()
    await Promise.resolve()
    await Promise.resolve()
    for (const view of renderedViews) view.rerender()
  }
  const invokePlugin = (plugin, ctx, config) => {
    if (typeof plugin !== 'function') return plugin.apply(ctx, config)
    if (!plugin.prototype) return plugin(ctx, config)
    const instance = new plugin(ctx, config)
    for (const hook of instance?.[Symbol.for('cordis.initHooks')] ?? []) hook()
    return instance?.[Symbol.for('cordis.init')]?.()
  }
  const resolveInject = (inject, result = {}) => {
    if (!inject) return Object.keys(result)
    if (Array.isArray(inject)) {
      for (const name of inject) result[name] = null
    } else if (Reflect.has(inject, Symbol.for('cordis.checkProto'))) {
      resolveInject(Object.getPrototypeOf(inject), result)
      for (const name of Object.keys(inject)) result[name] = inject[name] ?? null
    } else {
      for (const name of Object.keys(inject)) result[name] = inject[name] ?? null
    }
    return Object.keys(result)
  }
  const config = {
    react: React,
    render,
    within,
    produce,
    act,
    stabilize,
    registerSnapshotSerializer() { serializerRegistrations += 1 },
    clearStorage() { storage.clear(); storageClears += 1 },
    isHtmlElement: value => value instanceof FakeHTMLElement,
    invokePlugin,
    resolveInject,
  }
  const rootChildren = {
    'trt.panel': { kind: 'single', scope: 'root' },
    'trt.chat': { kind: 'single', scope: 'session' },
    'trt.rows': { kind: 'list', scope: 'root' },
  }
  const rootFrame = props => React.createElement(
    React.Fragment,
    null,
    props.renderSlot('trt.panel', { label: 'from-owner' }, { fallback: 'no panel' }),
    React.createElement(
      props.SessionProvider,
      { empty: () => 'no session' },
      () => props.renderSlot('trt.chat', {}),
    ),
    props.renderSlot('trt.rows', {}),
  )
  return {
    React, config, rootChildren, rootFrame, renderedViews, storage,
    serializerRegistrations: () => serializerRegistrations,
    storageClears: () => storageClears,
  }
}

export function runtimePanelComponent(React) {
  return props => React.createElement('b', { 'data-testid': 'panel' }, `panel:${props.label ?? 'none'}`)
}
export function runtimeChatComponent(React) {
  return props => React.createElement('span', {}, `chat:${props.sessionId}:${String(props.useSession(value => value.running))}`)
}
export function runtimeText(view) { return view.container.textContent }
export function runtimeAutoChildren() {
  return {
    'trt.panel': { kind: 'single', scope: 'root' },
    'trt.rows': { kind: 'list', scope: 'root' },
  }
}
export function runtimePanelOptions() { return { name: 'trt.panel' } }
export function runtimeChatOptions() { return { name: 'trt.chat' } }
export function runtimeRowsOptions() { return { name: 'trt.rows', id: 'row-1' } }
export function runtimeStoreOptions(store) { return { name: 'trt.chat', store } }
export function runtimeStoreHandle() {
  const created = []
  const cleared = []
  return {
    created, cleared,
    create(scopeKey) {
      const listeners = new Set()
      const state = { note: '' }
      const instance = {
        scopeKey,
        actions: {
          setNote(note) {
            state.note = note
            for (const listener of [...listeners]) listener()
          },
        },
        getSnapshot: () => ({ ...state }),
        subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener) },
        clearPersisted() { cleared.push(scopeKey) },
      }
      created.push(instance)
      return instance
    },
  }
}
export function runtimeFeaturePlugin(React) {
  return {
    name: 'runtime-feature',
    inject: ['slots', 'layout', 'conversationEvents', 'conversationViews'],
    apply(ctx) {
      ctx.provide('feature-service', { ok: true })
      ctx.conversationEvents.register({ kind: 'runtime-feature-event' })
      ctx.conversationViews.register({ target: 'runtime-feature-view' })
      ctx.slots.register(
        { name: 'trt.rows', id: 'feature-row', children: { 'trt.rows.hole': { kind: 'single', scope: 'root' } } },
        props => React.createElement('div', { 'data-testid': 'row' }, props.renderSlot('trt.rows.hole', {})),
      )
    },
  }
}
export function runtimeMissingPlugin() { return { inject: ['slots', 'absent-service'], apply() {} } }
export function runtimeClassPlugin() {
  const inherited = { slots: null }
  const inject = Object.create(inherited)
  inject[Symbol.for('cordis.checkProto')] = true
  return class RuntimeClassPlugin {
    static inject = inject
    constructor(ctx) {
      this[Symbol.for('cordis.initHooks')] = [
        () => ctx.provide('class-feature-service', { ok: true }),
      ]
    }
  }
}
export function runtimeSetStorage(bench, key, value) { bench.storage.set(key, value) }
export function runtimeFiberUid(handle) { return handle.fiber.uid }
"#)]
extern "C" {
    fn runtimeContextWrapper() -> JsValue;
    fn runtimeBench() -> JsValue;
    fn runtimePanelComponent(react: &JsValue) -> JsValue;
    fn runtimeChatComponent(react: &JsValue) -> JsValue;
    fn runtimeText(view: &JsValue) -> String;
    fn runtimeAutoChildren() -> JsValue;
    fn runtimePanelOptions() -> JsValue;
    fn runtimeChatOptions() -> JsValue;
    fn runtimeRowsOptions() -> JsValue;
    fn runtimeStoreOptions(store: &JsValue) -> JsValue;
    fn runtimeStoreHandle() -> JsValue;
    fn runtimeFeaturePlugin(react: &JsValue) -> JsValue;
    fn runtimeMissingPlugin() -> JsValue;
    fn runtimeClassPlugin() -> JsValue;
    fn runtimeSetStorage(bench: &JsValue, key: &str, value: &str);
    fn runtimeFiberUid(handle: &JsValue) -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn set(value: &JsValue, key: &str, entry: &JsValue) {
    Reflect::set(value, &JsValue::from_str(key), entry).unwrap();
}

fn call(value: &JsValue, method: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let function = property(value, method).dyn_into::<Function>()?;
    let arguments: Array = arguments.iter().cloned().collect();
    function.apply(value, &arguments)
}

async fn await_call(value: &JsValue, method: &str, arguments: &[JsValue]) -> JsValue {
    let result = call(value, method, arguments).unwrap();
    JsFuture::from(Promise::resolve(&result)).await.unwrap()
}

fn configure() -> JsValue {
    configure_context_wrapper(runtimeContextWrapper()).unwrap();
    let bench = runtimeBench();
    let react = property(&bench, "React");
    let selector = create_selector_shim(react.clone()).unwrap();
    configure_client_web_react(react, selector.into()).unwrap();
    let config = property(&bench, "config");
    let create_context =
        Closure::wrap(Box::new(create_context) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&config, "createContext", &create_context.into_js_value());
    let create_renderer = Closure::wrap(
        Box::new(create_slot_renderer) as Box<dyn FnMut() -> Result<JsValue, JsValue>>
    );
    set(
        &config,
        "createSlotRenderer",
        &create_renderer.into_js_value(),
    );
    configure_client_test_runtime(config).unwrap();
    bench
}

async fn runtime() -> JsValue {
    JsFuture::from(WasmSlotTestRuntime::create()).await.unwrap()
}

async fn declare_frame(runtime: &JsValue, bench: &JsValue) {
    let root = property(runtime, "root");
    await_call(
        &root,
        "declare",
        &[
            property(bench, "rootChildren"),
            property(bench, "rootFrame"),
        ],
    )
    .await;
}

#[wasm_bindgen_test(async)]
async fn root_renderer_sessions_and_direct_registration_share_the_live_assembly() {
    let bench = configure();
    let runtime = runtime().await;
    assert_eq!(
        property(&bench, "serializerRegistrations")
            .dyn_into::<Function>()
            .unwrap()
            .call0(&JsValue::UNDEFINED)
            .unwrap()
            .as_f64(),
        Some(1.0)
    );
    let error = call(&runtime, "renderRoot", &[]).unwrap_err();
    assert!(
        property(&error, "message")
            .as_string()
            .unwrap()
            .contains("root")
    );

    declare_frame(&runtime, &bench).await;
    let view = call(&runtime, "renderRoot", &[]).unwrap();
    let initial_text = runtimeText(&view);
    assert!(initial_text.contains("no panel"), "{initial_text:?}");
    assert!(initial_text.contains("no session"), "{initial_text:?}");

    let slots = property(&runtime, "slots");
    let panel_dispose = call(
        &slots,
        "register",
        &[
            runtimePanelOptions(),
            runtimePanelComponent(&property(&bench, "React")),
        ],
    )
    .unwrap()
    .dyn_into::<Function>()
    .unwrap();
    await_call(&runtime, "flush", &[]).await;
    assert!(runtimeText(&view).contains("panel:from-owner"));

    let panel_disposal = panel_dispose.call0(&JsValue::UNDEFINED).unwrap();
    JsFuture::from(Promise::resolve(&panel_disposal))
        .await
        .unwrap();
    await_call(&runtime, "flush", &[]).await;
    assert!(runtimeText(&view).contains("no panel"));

    call(
        &slots,
        "register",
        &[
            runtimeChatOptions(),
            runtimeChatComponent(&property(&bench, "React")),
        ],
    )
    .unwrap();
    let sessions = property(&runtime, "sessions");
    await_call(
        &sessions,
        "add",
        &[object(&[("id", JsValue::from_str("s1"))])],
    )
    .await;
    assert!(runtimeText(&view).contains("chat:s1:false"));
    let update = Function::new_with_args("draft", "draft.running = true");
    await_call(
        &sessions,
        "updateSnapshot",
        &[JsValue::from_str("s1"), update.into()],
    )
    .await;
    assert!(runtimeText(&view).contains("chat:s1:true"));
    await_call(&sessions, "setCurrent", &[JsValue::UNDEFINED]).await;
    assert!(runtimeText(&view).contains("no session"));
    await_call(&runtime, "dispose", &[]).await;
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)]
async fn automatic_local_views_update_owner_props_and_guard_their_lifecycle() {
    let bench = configure();
    let runtime = runtime().await;
    await_call(&runtime, "declare", &[runtimeAutoChildren()]).await;
    let slots = property(&runtime, "slots");
    call(
        &slots,
        "register",
        &[
            runtimePanelOptions(),
            runtimePanelComponent(&property(&bench, "React")),
        ],
    )
    .unwrap();
    call(
        &slots,
        "register",
        &[
            runtimeRowsOptions(),
            Function::new_with_args("_props", "return 'row'").into(),
        ],
    )
    .unwrap();
    await_call(&runtime, "flush", &[]).await;

    let panel = call(
        &runtime,
        "renderSlot",
        &[
            JsValue::from_str("trt.panel"),
            object(&[("label", JsValue::from_str("first"))]),
        ],
    )
    .unwrap();
    let container = property(&panel, "container");
    assert_eq!(
        call(
            &container,
            "getAttribute",
            &[JsValue::from_str("data-slot")]
        )
        .unwrap()
        .as_string()
        .as_deref(),
        Some("trt.panel")
    );
    assert_eq!(
        property(&container, "textContent").as_string().as_deref(),
        Some("panel:first")
    );
    let queries = property(&panel, "view");
    let first_panel = call(&queries, "getByTestId", &[JsValue::from_str("panel")]).unwrap();
    call(
        &panel,
        "update",
        &[object(&[("label", JsValue::from_str("second"))])],
    )
    .unwrap();
    assert_eq!(
        property(&container, "textContent").as_string().as_deref(),
        Some("panel:second")
    );
    let updated_panel = call(&queries, "getByTestId", &[JsValue::from_str("panel")]).unwrap();
    assert!(Object::is(&first_panel, &updated_panel));
    let rows = call(
        &runtime,
        "renderSlot",
        &[JsValue::from_str("trt.rows"), Object::new().into()],
    )
    .unwrap();
    assert_eq!(
        property(&property(&rows, "container"), "textContent")
            .as_string()
            .as_deref(),
        Some("row")
    );
    let error = call(
        &runtime,
        "renderSlot",
        &[JsValue::from_str("trt.chat"), Object::new().into()],
    )
    .unwrap_err();
    assert!(
        property(&error, "message")
            .as_string()
            .unwrap()
            .contains("without declare")
    );

    let mounted_views = Array::from(&property(&bench, "renderedViews"));
    call(&mounted_views.get(0), "unmount", &[]).unwrap();
    let error = call(
        &runtime,
        "renderSlot",
        &[JsValue::from_str("trt.panel"), Object::new().into()],
    )
    .unwrap_err();
    assert!(
        property(&error, "message")
            .as_string()
            .unwrap()
            .contains("rendered no wrapper")
    );

    await_call(&runtime, "dispose", &[]).await;
    let error = call(
        &runtime,
        "renderSlot",
        &[JsValue::from_str("trt.panel"), Object::new().into()],
    )
    .unwrap_err();
    assert!(
        property(&error, "message")
            .as_string()
            .unwrap()
            .contains("root")
    );
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)]
async fn stores_feature_fibers_and_runtime_disposal_share_one_cleanup_axis() {
    let bench = configure();
    let runtime = runtime().await;
    declare_frame(&runtime, &bench).await;
    let slots = property(&runtime, "slots");
    let store = runtimeStoreHandle();
    call(
        &slots,
        "register",
        &[
            runtimeStoreOptions(&store),
            Function::new_with_args("_props", "return null").into(),
        ],
    )
    .unwrap();
    let before_render = call(
        &runtime,
        "storeOf",
        &[JsValue::from_str("trt.chat"), JsValue::from_str("s1")],
    )
    .unwrap_err();
    assert!(
        property(&before_render, "message")
            .as_string()
            .unwrap()
            .contains("before renderRoot")
    );
    let view = call(&runtime, "renderRoot", &[]).unwrap();
    let sessions = property(&runtime, "sessions");
    await_call(
        &sessions,
        "add",
        &[object(&[("id", JsValue::from_str("s1"))])],
    )
    .await;
    let first = call(
        &runtime,
        "storeOf",
        &[JsValue::from_str("trt.chat"), JsValue::from_str("s1")],
    )
    .unwrap();
    let again = call(
        &runtime,
        "storeOf",
        &[JsValue::from_str("trt.chat"), JsValue::from_str("s1")],
    )
    .unwrap();
    assert!(Object::is(&first, &again));
    await_call(&sessions, "remove", &[JsValue::from_str("s1")]).await;
    assert_eq!(Array::from(&property(&store, "cleared")).length(), 1);
    await_call(
        &sessions,
        "add",
        &[object(&[("id", JsValue::from_str("s1"))])],
    )
    .await;
    let reborn = call(
        &runtime,
        "storeOf",
        &[JsValue::from_str("trt.chat"), JsValue::from_str("s1")],
    )
    .unwrap();
    assert!(!Object::is(&first, &reborn));

    call(
        &runtime,
        "provide",
        &[
            JsValue::from_str("layout"),
            object(&[("openDetails", Function::new_no_args("").into())]),
        ],
    )
    .unwrap();
    let handle = await_call(
        &runtime,
        "mount",
        &[runtimeFeaturePlugin(&property(&bench, "React"))],
    )
    .await;
    assert_eq!(
        Array::from(&call(&slots, "entries", &[JsValue::from_str("trt.rows")]).unwrap()).length(),
        1
    );
    assert!(
        !call(&view, "queryByTestId", &[JsValue::from_str("row")])
            .unwrap()
            .is_null()
    );
    let ctx = property(&runtime, "ctx");
    assert_eq!(
        property(
            &call(&ctx, "get", &[JsValue::from_str("feature-service")]).unwrap(),
            "ok"
        )
        .as_bool(),
        Some(true)
    );
    await_call(&handle, "dispose", &[]).await;
    await_call(&handle, "dispose", &[]).await;
    assert_eq!(
        Array::from(&call(&slots, "entries", &[JsValue::from_str("trt.rows")]).unwrap()).length(),
        0
    );
    assert!(
        call(&slots, "spec", &[JsValue::from_str("trt.rows.hole")])
            .unwrap()
            .is_undefined()
    );
    assert!(
        call(&view, "queryByTestId", &[JsValue::from_str("row")])
            .unwrap()
            .is_null()
    );
    assert!(
        call(&ctx, "get", &[JsValue::from_str("feature-service")])
            .unwrap()
            .is_undefined()
    );
    let events = call(&ctx, "get", &[JsValue::from_str("conversationEvents")]).unwrap();
    call(
        &events,
        "register",
        &[object(&[(
            "kind",
            JsValue::from_str("runtime-feature-event"),
        )])],
    )
    .unwrap();
    let views = call(&ctx, "get", &[JsValue::from_str("conversationViews")]).unwrap();
    call(
        &views,
        "register",
        &[object(&[(
            "target",
            JsValue::from_str("runtime-feature-view"),
        )])],
    )
    .unwrap();

    let missing = call(&runtime, "mount", &[runtimeMissingPlugin()]).unwrap();
    assert!(JsFuture::from(Promise::resolve(&missing)).await.is_err());
    let class_handle = await_call(&runtime, "mount", &[runtimeClassPlugin()]).await;
    assert_eq!(
        property(
            &call(&ctx, "get", &[JsValue::from_str("class-feature-service")]).unwrap(),
            "ok"
        )
        .as_bool(),
        Some(true)
    );
    await_call(&class_handle, "dispose", &[]).await;
    assert!(
        call(&ctx, "get", &[JsValue::from_str("class-feature-service")])
            .unwrap()
            .is_undefined()
    );
    runtimeSetStorage(&bench, "leftover", "x");
    await_call(&runtime, "dispose", &[]).await;
    await_call(&runtime, "dispose", &[]).await;
    assert_eq!(runtimeText(&view), "");
    assert!(runtimeFiberUid(&handle).is_null());
    assert_eq!(
        property(&bench, "storageClears")
            .dyn_into::<Function>()
            .unwrap()
            .call0(&JsValue::UNDEFINED)
            .unwrap()
            .as_f64(),
        Some(1.0)
    );
}

fn object(entries: &[(&str, JsValue)]) -> JsValue {
    let value = Object::new();
    for (key, entry) in entries {
        Reflect::set(&value, &JsValue::from_str(key), entry).unwrap();
    }
    value.into()
}
