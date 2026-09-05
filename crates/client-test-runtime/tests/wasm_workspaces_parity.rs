//! Live browser Cordis parity for the Workspaces test double.

#![cfg(target_arch = "wasm32")]

use js_sys::{Function, Promise, Reflect};
use seekdeep_client_test_runtime::install_test_workspaces;
use seekdeep_cordis::{configure_context_wrapper, create_context};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function workspaceContextWrapper() {
  return core => {
    let ctx
    ctx = new Proxy(core, {
      get(target, key, receiver) {
        if (key === 'emit') return (name, ...args) => target.emitArgs(name, args)
        if (key === 'parallel') return (name, ...args) => target.parallelArgs(name, args)
        if (key === 'serial') return (name, ...args) => target.serialArgs(name, args)
        if (key === 'bail') return (name, ...args) => target.bailArgs(name, args)
        if (key === 'get') return name => target.get(name)
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

export function workspaceControls() {
  const control = { stabilizations: 0 }
  control.stabilize = async callback => { control.stabilizations += 1; callback() }
  control.produce = (base, mutator) => {
    const next = { ...base, items: [...base.items], archivedSessionIds: [...base.archivedSessionIds] }
    mutator(next)
    return next
  }
  return control
}

export async function exerciseWorkspaces(ws, control) {
  if (ws.list.getSnapshot().phase !== 'ready') throw new Error('initial phase is not ready')
  let notifications = 0
  const listener = () => { notifications += 1 }
  const off = ws.list.subscribe(listener)
  ws.list.subscribe(listener)
  await ws.update(draft => { draft.phase = 'pending' })
  if (ws.list.getSnapshot().phase !== 'pending' || control.stabilizations !== 1 || notifications !== 1) {
    throw new Error('stabilized list update did not publish once')
  }
  off()
  await ws.update(draft => { draft.phase = 'ready' })
  if (notifications !== 1) throw new Error('list disposer did not remove duplicate listener identity')

  ws.startSession('w1')
  if (await ws.connectWorkspace('w2') !== 'session-of-w2') throw new Error('default connection echo drifted')
  ws.stub('connectWorkspace', async () => 'other')
  if (await ws.connectWorkspace('w3') !== 'other') throw new Error('connection stub was not used')

  const home = await ws.listDirectory()
  if (home.path !== '/home/test' || home.entries.length !== 0
      || home.crumbs.map(crumb => crumb.path).join('|') !== '/|/home|/home/test') {
    throw new Error('default directory listing drifted')
  }
  if (await ws.createDirectory('/home/test', 'fresh') !== '/home/test/fresh') {
    throw new Error('default directory create drifted')
  }
  const signal = { marker: 'signal' }
  const listing = { path: '/x', home: '/x', crumbs: [], entries: [], truncated: false }
  let forwarded = false
  ws.stub('listDirectory', async (path, received) => {
    forwarded = path === '/x' && received === signal
    return listing
  })
  if (await ws.listDirectory('/x', signal) !== listing || !forwarded) {
    throw new Error('directory stub did not receive exact arguments')
  }

  const created = await ws.create({ path: '/tmp/alpha' })
  if (created.title !== '/tmp/alpha' || created.path !== '/tmp/alpha') throw new Error('create default drifted')
  if (await ws.pickDirectory() !== null) throw new Error('picker default must cancel')
  if ((await ws.rename('w1', 'Renamed')).title !== 'Renamed') throw new Error('rename default drifted')
  await ws.delete('w1')
  await ws.openPath('/project/file.ts')
  await ws.insertBefore('w1', 'w2')
  const moved = await ws.insertSessionBefore('w1', 's1', 's2')
  if (moved.sessionIds.join('|') !== 's1') throw new Error('session move default drifted')
  await ws.archiveSession('s1')
  if (ws.list.getSnapshot().archivedSessionIds.join('|') !== 's1') throw new Error('archive did not publish')
  ws.stub('archiveSession', async () => {})
  await ws.archiveSession('s2')
  if (ws.list.getSnapshot().archivedSessionIds.join('|') !== 's1') throw new Error('archive stub did not replace default')

  return {
    notifications,
    stabilizations: control.stabilizations,
    methods: ws.calls.map(call => call.method),
  }
}
"#)]
extern "C" {
    fn workspaceContextWrapper() -> JsValue;
    fn workspaceControls() -> JsValue;
    fn exerciseWorkspaces(workspaces: &JsValue, control: &JsValue) -> Promise;
}

#[wasm_bindgen_test(async)]
async fn observable_actions_defaults_stubs_and_signals_match_source() {
    configure_context_wrapper(workspaceContextWrapper()).unwrap();
    let root = create_context().unwrap();
    let control = workspaceControls();
    let stabilize = Reflect::get(&control, &JsValue::from_str("stabilize")).unwrap();
    let produce = Reflect::get(&control, &JsValue::from_str("produce")).unwrap();
    let workspaces = install_test_workspaces(root.clone(), stabilize, produce).unwrap();
    let published = Reflect::get(&root, &JsValue::from_str("get"))
        .unwrap()
        .dyn_into::<Function>()
        .unwrap()
        .call1(&root, &JsValue::from_str("workspaces"))
        .unwrap();
    assert!(js_sys::Object::is(&published, &workspaces));
    let result = JsFuture::from(exerciseWorkspaces(&workspaces, &control))
        .await
        .unwrap();
    assert_eq!(
        Reflect::get(&result, &JsValue::from_str("notifications"))
            .unwrap()
            .as_f64(),
        Some(1.0)
    );
    let methods =
        js_sys::Array::from(&Reflect::get(&result, &JsValue::from_str("methods")).unwrap());
    for expected in [
        "startSession",
        "connectWorkspace",
        "listDirectory",
        "createDirectory",
        "create",
        "pickDirectory",
        "rename",
        "delete",
        "openPath",
        "insertBefore",
        "insertSessionBefore",
        "archiveSession",
    ] {
        assert!(
            methods
                .iter()
                .filter_map(|value| value.as_string())
                .any(|method| method == expected),
            "missing call {expected}"
        );
    }
    let fiber = Reflect::get(&root, &JsValue::from_str("fiber")).unwrap();
    let disposal = Reflect::get(&fiber, &JsValue::from_str("dispose"))
        .unwrap()
        .dyn_into::<Function>()
        .unwrap()
        .call0(&fiber)
        .unwrap();
    JsFuture::from(Promise::resolve(&disposal)).await.unwrap();
}
