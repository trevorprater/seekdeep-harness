//! Live browser Cordis, fixtures, scopes, provide-channel, list, and action parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Function, Promise, Reflect};
use seekdeep_client_test_runtime::install_test_sessions;
use seekdeep_cordis::{configure_context_wrapper, create_context};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function sessionsContextWrapper() {
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

export function sessionsControls(root) {
  const control = { stabilizations: 0, pruned: [], deferNext: false, resume: undefined }
  control.stabilize = callback => {
    control.stabilizations += 1
    if (!control.deferNext) {
      try { return Promise.resolve(callback()) } catch (error) { return Promise.reject(error) }
    }
    control.deferNext = false
    return new Promise((resolve, reject) => {
      control.resume = async () => {
        try { await callback(); resolve() } catch (error) { reject(error) }
        finally { control.resume = undefined }
      }
    })
  }
  control.produce = (base, mutator) => {
    const next = Array.isArray(base) ? [...base] : { ...base }
    if (Array.isArray(base.ids)) next.ids = [...base.ids]
    if (base.byId && typeof base.byId === 'object') next.byId = { ...base.byId }
    if (Array.isArray(base.pending)) next.pending = [...base.pending]
    if (Array.isArray(base.queue)) next.queue = [...base.queue]
    mutator(next)
    return next
  }
  root.provide('slots', { pruneStoreScope(id) { control.pruned.push(id) } })
  return control
}

export async function exerciseSessions(root, sessions, control) {
  const initial = sessions.list.getSnapshot()
  if (initial.ids.length !== 0 || initial.phase !== 'ready' || initial.current !== undefined) {
    throw new Error('initial Session list drifted')
  }
  const absent = sessions.currentProvideInfo.getSnapshot()
  if (absent.sessionId !== undefined || absent.hooks.session !== undefined) {
    throw new Error('initial maybe provide bundle drifted')
  }
  let listNotifications = 0
  const listListener = () => { listNotifications += 1 }
  const offList = sessions.list.subscribe(listListener)
  sessions.list.subscribe(listListener)

  const prompt = () => 'overridden'
  if (await sessions.add({
    id: 's1',
    snapshot: { running: true },
    summary: { displayTitle: 'One' },
    session: { prompt },
  }) !== 's1') throw new Error('add did not resolve the id')
  await sessions.add({ id: 's2' }, { current: false })
  const listed = sessions.list.getSnapshot()
  if (listed.ids.join('|') !== 's1|s2' || listed.current !== 's1'
      || listed.byId.s1.displayTitle !== 'One') {
    throw new Error(`Session add/list drifted: ${JSON.stringify(listed)}`)
  }
  if (listNotifications !== 2) throw new Error('list listener identity was duplicated')
  offList()
  let duplicateRejected = false
  try { await sessions.add({ id: 's1' }) } catch { duplicateRejected = true }
  if (!duplicateRejected) throw new Error('duplicate Session was accepted')

  const behavior = sessions.behavior('s1')
  if (behavior.prompt() !== 'overridden' || behavior.getSnapshot().running !== true) {
    throw new Error('fixture override or snapshot drifted')
  }
  let snapshotNotifications = 0
  const offSnapshot = behavior.subscribe(() => { snapshotNotifications += 1 })
  await sessions.updateSnapshot('s1', draft => { draft.blank = true })
  if (!behavior.getSnapshot().blank || snapshotNotifications !== 1) {
    throw new Error('snapshot update did not publish')
  }
  offSnapshot()
  await sessions.updateSummary('s1', { running: true, displayTitle: 'Renamed' })
  if (sessions.list.getSnapshot().byId.s1.displayTitle !== 'Renamed') {
    throw new Error('summary update did not publish')
  }
  await sessions.setCurrent('s2')
  if (sessions.list.getSnapshot().current !== 's2') throw new Error('setCurrent failed')
  let unknownRejected = false
  try { await sessions.setCurrent('missing') } catch { unknownRejected = true }
  if (!unknownRejected) throw new Error('unknown current Session was accepted')

  const scope = sessions.scope('s1')
  if (sessions.scope('s1') !== scope || sessions.scopeOf(scope) !== 's1') {
    throw new Error('Session scope identity drifted')
  }
  const binding = sessions.binding('s1')
  if (binding.ctx !== scope || binding.session !== behavior
      || sessions.sessionOf(scope) !== behavior) throw new Error('Session binding drifted')

  const firstInfo = sessions.provideInfo('s1')
  if (sessions.provideInfo('s1') !== firstInfo || firstInfo.hooks.session !== behavior
      || firstInfo.projections !== behavior.projections) throw new Error('built-in provide bundle drifted')
  sessions.provideInfo('s2')
  const providerResolutionOrder = []
  const probe = { getSnapshot: () => 1, subscribe: () => () => {} }
  const offProvider = sessions.provide({
    hooks: ['probe'], props: ['marker'],
    resolve: candidate => {
      providerResolutionOrder.push(candidate.sessionId)
      return { hooks: { probe }, props: { marker: candidate.sessionId } }
    },
  })
  if (providerResolutionOrder.join('|') !== 's1|s2') {
    throw new Error(`Session provider rebuild order drifted: ${providerResolutionOrder.join('|')}`)
  }
  const rebuilt = sessions.provideInfo('s1')
  if (rebuilt === firstInfo || rebuilt.hooks.probe !== probe || rebuilt.props.marker !== 's1') {
    throw new Error('custom provide roster did not rebuild')
  }
  await sessions.setCurrent('s1')
  if (sessions.currentProvideInfo.getSnapshot() !== rebuilt) {
    throw new Error('current provide projection drifted')
  }
  offProvider()

  sessions.openSubagent({ parentSessionId: 's1', childSessionId: 's2', pluginId: 'p' })
  if (sessions.list.getSnapshot().current !== 's2'
      || sessions.subagentAddress('s2')?.parentSessionId !== 's1') {
    throw new Error('addressed selection drifted')
  }
  sessions.open('s1')
  if (sessions.subagentAddress('s1') !== undefined) throw new Error('ordinary open retained address')
  sessions.setSubagentCatalogOpen('s1', true)
  await sessions.refreshSubagents('s1')
  sessions.noteAgentPreset('s1', 'code')
  if (sessions.list.getSnapshot().byId.s1.agentPreset !== 'code') throw new Error('preset echo failed')

  const signal = { marker: 'signal' }
  const emptySearch = await sessions.search('none', signal)
  if (!emptySearch.ok || emptySearch.value.items.length !== 0) throw new Error('empty search drifted')
  let forwarded = false
  sessions.stubSearch((query, received) => {
    forwarded = query === 'find' && received === signal
    return { items: [{ sessionId: 's1', snippet: 'hit' }], hasMore: true }
  })
  const hits = await sessions.search('find', signal)
  if (!forwarded || hits.value.items[0].snippet !== 'hit' || !hits.value.hasMore) {
    throw new Error('search stub drifted')
  }
  if (await sessions.fork({ sessionId: 's1', increaseTitle: true }) !== 's1') {
    throw new Error('fork echo drifted')
  }
  sessions.clear()
  if (sessions.list.getSnapshot().current !== undefined) throw new Error('clear failed')

  sessions.open('s1')
  control.deferNext = true
  const pendingRemoval = sessions.remove('s1')
  const offRemovalProvider = sessions.provide({ resolve: () => ({}) })
  const removalProjection = sessions.currentProvideInfo.getSnapshot()
  if (removalProjection.sessionId !== undefined || removalProjection.hooks.session !== undefined) {
    throw new Error('unknown current Session did not fall back to the no-session bundle')
  }
  offRemovalProvider()
  await control.resume()
  await pendingRemoval

  const removedScope = sessions.scope('s2')
  await sessions.remove('s2')
  if (sessions.list.getSnapshot().ids.includes('s2') || control.pruned.join('|') !== 's1|s2') {
    throw new Error('remove did not prune list and stores')
  }
  if (removedScope.fiber?.uid !== undefined && removedScope.fiber?.uid !== null) {
    throw new Error('removed scope fiber remained live')
  }
  await sessions.disposeScopes()

  return {
    duplicateRejected,
    unknownRejected,
    listNotifications,
    snapshotNotifications,
    searchResultLimit: sessions.searchResultLimit,
    methods: sessions.calls.map(call => call.method),
    stabilizations: control.stabilizations,
  }
}
"#)]
extern "C" {
    fn sessionsContextWrapper() -> JsValue;
    fn sessionsControls(root: &JsValue) -> JsValue;
    fn exerciseSessions(root: &JsValue, sessions: &JsValue, control: &JsValue) -> Promise;
}

#[wasm_bindgen_test(async)]
async fn list_fixture_scope_provide_and_service_actions_match_source() {
    configure_context_wrapper(sessionsContextWrapper()).unwrap();
    let root = create_context().unwrap();
    let control = sessionsControls(&root);
    let stabilize = Reflect::get(&control, &JsValue::from_str("stabilize")).unwrap();
    let produce = Reflect::get(&control, &JsValue::from_str("produce")).unwrap();
    let sessions = install_test_sessions(root.clone(), stabilize, produce).unwrap();
    let published = Reflect::get(&root, &JsValue::from_str("get"))
        .unwrap()
        .dyn_into::<Function>()
        .unwrap()
        .call1(&root, &JsValue::from_str("sessions"))
        .unwrap();
    assert!(js_sys::Object::is(&published, &sessions));
    let result = JsFuture::from(exerciseSessions(&root, &sessions, &control))
        .await
        .unwrap();
    assert_eq!(
        Reflect::get(&result, &JsValue::from_str("duplicateRejected"))
            .unwrap()
            .as_bool(),
        Some(true)
    );
    assert_eq!(
        Reflect::get(&result, &JsValue::from_str("unknownRejected"))
            .unwrap()
            .as_bool(),
        Some(true)
    );
    assert_eq!(
        Reflect::get(&result, &JsValue::from_str("searchResultLimit"))
            .unwrap()
            .as_f64(),
        Some(100.0)
    );
    let fiber = Reflect::get(&root, &JsValue::from_str("fiber")).unwrap();
    let disposal = Reflect::get(&fiber, &JsValue::from_str("dispose"))
        .unwrap()
        .dyn_into::<Function>()
        .unwrap()
        .call0(&fiber)
        .unwrap();
    JsFuture::from(Promise::resolve(&disposal)).await.unwrap();
}
