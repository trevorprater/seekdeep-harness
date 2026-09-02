//! Live WASM contribution ordering, rollback, and disposal parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Promise, Reflect};
use seekdeep_api_remotes_client::{
    api_remotes_inject, apply_api_remotes, configure_api_remotes, generated_api_remotes,
};
use seekdeep_client_foundation_wasm::{
    client_api_gateway_plugin, client_connection_plugin, client_typert_registry_plugin,
    configure_client_api_gateway,
};
use seekdeep_cordis::{configure_context_wrapper, create_context};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function apiRemotesBench(failAt) {
  const log = []
  const remote = {
    async $mount(contribution) {
      log.push('mount:' + contribution)
      if (contribution === failAt) throw new Error('mount failed: ' + contribution)
      return async () => { log.push('dispose:' + contribution) }
    },
  }
  const ctx = { get(name) { return name === 'remote' ? remote : undefined } }
  return { ctx, log }
}
export function apiRemotesLog(bench) { return bench.log }

export function remoteContextWrapper() {
  const tracker = Symbol.for('cordis.service.tracker')
  const trace = (ctx, value) => {
    if ((typeof value !== 'object' && typeof value !== 'function') || value === null || value[tracker] !== true) return value
    let proxy
    proxy = new Proxy(value, {
      get(target, key, receiver) {
        if (key === 'ctx') return ctx
        const inner = Reflect.get(target, key, receiver)
        return typeof inner === 'function' ? (...args) => Reflect.apply(inner, proxy, args) : inner
      },
    })
    return proxy
  }
  return core => {
    let ctx
    ctx = new Proxy(core, {
      get(target, key, receiver) {
        if (key === 'emit') return (name, ...args) => target.emitArgs(name, args)
        if (key === 'parallel') return (name, ...args) => target.parallelArgs(name, args)
        if (key === 'serial') return (name, ...args) => target.serialArgs(name, args)
        if (key === 'bail') return (name, ...args) => target.bailArgs(name, args)
        if (key === 'get') return name => trace(ctx, target.get(name))
        if (Reflect.has(target, key)) {
          const value = Reflect.get(target, key, receiver)
          return typeof value === 'function' ? value.bind(target) : value
        }
        const metadata = target.metaGet(key)
        if (metadata !== undefined) return metadata
        return typeof key === 'string' ? trace(ctx, target.get(key)) : undefined
      },
    })
    return ctx
  }
}

export function remoteGatewayFactories() {
  const tracker = Symbol.for('cordis.service.tracker')
  const namespaces = ['commands', 'goals', 'dynamicCordisRunner', 'pluginInventory', 'messageFeedback']
  const remoteFactory = (ctx, core) => {
    const service = {
      ctx,
      $mount(contribution) { return core.mount(this.ctx, contribution) },
      $on(event, listener) { return core.on(this.ctx, event, listener) },
      $dispatch(event, args) { return core.dispatch(event, args) },
    }
    Object.defineProperty(service, tracker, { value: true })
    for (const namespace of namespaces) {
      Object.defineProperty(service, namespace, { get() { return this.ctx.get('remote.' + namespace) } })
    }
    ctx.provide('remote', service)
    return service
  }
  const namespaceFactory = (ctx, namespace, invoke) => {
    const service = {
      ctx,
      namespace,
      install(method) {
        Object.defineProperty(this, method, {
          configurable: true,
          value: function (...args) { return invoke(this.ctx, method, args) },
        })
      },
      remove(method) { delete this[method] },
    }
    Object.defineProperty(service, tracker, { value: true })
    Object.defineProperty(service, 'invokeRemote', { value: invoke })
    return { service, dispose: ctx.provide('remote.' + namespace, service) }
  }
  return [remoteFactory, namespaceFactory]
}

export function installGoalFetch() {
  const originalFetch = globalThis.fetch
  const originalLocation = Object.getOwnPropertyDescriptor(globalThis, 'location')
  const calls = []
  Object.defineProperty(globalThis, 'location', {
    configurable: true,
    value: { hostname: '127.0.0.1', origin: 'http://127.0.0.1' },
  })
  globalThis.fetch = async request => {
    const body = await request.json()
    const args = body.payload.args
    calls.push({ url: new URL(request.url).pathname, method: body.method, args })
    let value
    if (body.method === 'goals/create') {
      value = { ref: { id: 'goal-' + args.agentId, revision: 1 } }
    } else if (body.method === 'goals/edit') {
      value = {
        roundsStarted: 0, createdAt: 1, updatedAt: 2, activation: 'armed',
        objective: args.request.objective, phase: 'active', maxGoalRounds: 256,
        id: args.ref.id, revision: 2,
      }
    } else {
      value = null
    }
    return new Response(JSON.stringify({
      type: 'server-response', rpcId: body.rpcId, result: { ok: true, value },
    }), { status: 200, headers: { 'content-type': 'application/json' } })
  }
  return {
    calls,
    restore() {
      globalThis.fetch = originalFetch
      if (originalLocation === undefined) delete globalThis.location
      else Object.defineProperty(globalThis, 'location', originalLocation)
    },
  }
}

export function remotePlugin(root, plugin) { return root.plugin(plugin).await() }
export function remoteGet(root, name) { return root.get(name) }
export function remoteRegisterBinder(typert) {
  return typert.contexts.registerClient('agent', { identity: ctx => ctx.builtAgentId })
}
export function remoteInvalidGoal(remote) { return remote.goals.create('root', { objective: 1 }) }
export function remoteCreateGoal(remote, id, objective) { return remote.goals.create(id, { objective }) }
export function remoteEditGoal(remote, id, ref, objective) { return remote.goals.edit(id, ref, { objective }) }
export function remoteScopedGoal(root, objective) {
  const scoped = root.extend({ builtAgentId: 'scoped' })
  return scoped.remote.goals.create({ objective, maxGoalRounds: 3 })
}
export function remoteCalls(control) { return control.calls }
export function remoteRestore(control) { control.restore() }
"#)]
extern "C" {
    fn apiRemotesBench(fail_at: &str) -> JsValue;
    fn apiRemotesLog(bench: &JsValue) -> Array;
    fn remoteContextWrapper() -> JsValue;
    fn remoteGatewayFactories() -> Array;
    fn installGoalFetch() -> JsValue;
    fn remotePlugin(root: &JsValue, plugin: &JsValue) -> Promise;
    fn remoteGet(root: &JsValue, name: &str) -> JsValue;
    fn remoteRegisterBinder(typert: &JsValue) -> Function;
    fn remoteInvalidGoal(remote: &JsValue) -> Promise;
    fn remoteCreateGoal(remote: &JsValue, id: &str, objective: &str) -> Promise;
    fn remoteEditGoal(remote: &JsValue, id: &str, reference: &JsValue, objective: &str) -> Promise;
    fn remoteScopedGoal(root: &JsValue, objective: &str) -> Promise;
    fn remoteCalls(control: &JsValue) -> Array;
    fn remoteRestore(control: &JsValue);
}

fn contributions() -> Array {
    ["commands", "goals", "dynamic", "inventory", "feedback"]
        .into_iter()
        .map(JsValue::from_str)
        .collect()
}

fn log(bench: &JsValue) -> Vec<String> {
    apiRemotesLog(bench)
        .iter()
        .filter_map(|value| value.as_string())
        .collect()
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

#[wasm_bindgen_test]
fn generated_contributions_are_complete_and_goal_codecs_reject_invalid_requests() {
    let contributions = generated_api_remotes().unwrap();
    assert_eq!(contributions.length(), 5);
    assert_eq!(
        property(&contributions.get(0), "package")
            .as_string()
            .as_deref(),
        Some("@seekdeep-ai/seekdeep-commands")
    );
    let goals = contributions.get(1);
    assert_eq!(
        property(&goals, "package").as_string().as_deref(),
        Some("@seekdeep-ai/seekdeep-goal")
    );
    let descriptors = Array::from(&property(&goals, "descriptors"));
    assert_eq!(descriptors.length(), 6);
    let create = descriptors
        .iter()
        .find(|descriptor| property(descriptor, "method").as_string().as_deref() == Some("create"))
        .unwrap();
    let parameters = Array::from(&property(&create, "parameters"));
    let request_codec = property(&parameters.get(1), "codec");
    let schema = property(&request_codec, "schema");
    let parse = property(&schema, "parse").dyn_into::<Function>().unwrap();
    let invalid = js_sys::JSON::parse(r#"{"objective":1}"#).unwrap();
    assert!(parse.call1(&schema, &invalid).is_err());
    let valid =
        js_sys::JSON::parse(r#"{"objective":"ship it","maxGoalRounds":3,"ignored":"strip me"}"#)
            .unwrap();
    let parsed = parse.call1(&schema, &valid).unwrap();
    assert_eq!(
        property(&parsed, "objective").as_string().as_deref(),
        Some("ship it")
    );
    assert_eq!(property(&parsed, "maxGoalRounds").as_f64(), Some(3.0));
    assert!(property(&parsed, "ignored").is_undefined());
}

#[wasm_bindgen_test(async)]
async fn generated_goal_remote_supports_explicit_and_agent_context_calls() {
    configure_context_wrapper(remoteContextWrapper()).unwrap();
    let factories = remoteGatewayFactories();
    configure_client_api_gateway(factories.get(0), factories.get(1)).unwrap();
    let fetch = installGoalFetch();
    let root = create_context().unwrap();
    for plugin in [
        client_connection_plugin().unwrap(),
        client_typert_registry_plugin().unwrap(),
        client_api_gateway_plugin().unwrap(),
    ] {
        JsFuture::from(remotePlugin(&root, &plugin)).await.unwrap();
    }
    configure_api_remotes(generated_api_remotes().unwrap().into()).unwrap();
    let disposer = JsFuture::from(apply_api_remotes(root.clone()))
        .await
        .unwrap();
    let remote = remoteGet(&root, "remote");
    let typert = remoteGet(&root, "typert");
    let dispose_binder = remoteRegisterBinder(&typert);

    assert!(JsFuture::from(remoteInvalidGoal(&remote)).await.is_err());
    let created = JsFuture::from(remoteCreateGoal(&remote, "root", "root goal"))
        .await
        .unwrap();
    assert_eq!(property(&created, "ok").as_bool(), Some(true));
    let reference = property(&property(&created, "value"), "ref");
    let edited = JsFuture::from(remoteEditGoal(
        &remote,
        "root",
        &reference,
        "edited root goal",
    ))
    .await
    .unwrap();
    assert_eq!(
        property(&property(&edited, "value"), "objective")
            .as_string()
            .as_deref(),
        Some("edited root goal")
    );
    let scoped = JsFuture::from(remoteScopedGoal(&root, "scoped goal"))
        .await
        .unwrap();
    assert_eq!(property(&scoped, "ok").as_bool(), Some(true));
    let calls = remoteCalls(&fetch);
    assert_eq!(calls.length(), 3);
    assert_eq!(
        property(&property(&calls.get(0), "args"), "agentId")
            .as_string()
            .as_deref(),
        Some("root")
    );
    assert_eq!(
        property(&property(&calls.get(2), "args"), "agentId")
            .as_string()
            .as_deref(),
        Some("scoped")
    );

    dispose_binder.call0(&JsValue::UNDEFINED).unwrap();
    let result = disposer
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    JsFuture::from(Promise::resolve(&result)).await.unwrap();
    assert!(property(&remote, "goals").is_undefined());
    let root_fiber = property(&root, "fiber");
    let root_dispose = property(&root_fiber, "dispose")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&root_fiber)
        .unwrap();
    JsFuture::from(Promise::resolve(&root_dispose))
        .await
        .unwrap();
    remoteRestore(&fetch);
}

#[wasm_bindgen_test(async)]
async fn mounts_in_declaration_order_and_disposes_in_reverse() {
    assert!(configure_api_remotes(JsValue::from_str("abcde")).is_err());
    assert!(
        configure_api_remotes(
            Array::of2(&JsValue::from_str("one"), &JsValue::from_str("two")).into()
        )
        .is_err()
    );
    configure_api_remotes(contributions().into()).unwrap();
    assert_eq!(
        api_remotes_inject()
            .iter()
            .filter_map(|value| value.as_string())
            .collect::<Vec<_>>(),
        ["remote"]
    );
    let bench = apiRemotesBench("");
    let disposer = JsFuture::from(apply_api_remotes(
        Reflect::get(&bench, &JsValue::from_str("ctx")).unwrap(),
    ))
    .await
    .unwrap();
    assert_eq!(
        log(&bench),
        [
            "mount:commands",
            "mount:goals",
            "mount:dynamic",
            "mount:inventory",
            "mount:feedback",
        ]
    );
    let disposer = disposer.dyn_into::<js_sys::Function>().unwrap();
    let result = disposer.call0(&JsValue::UNDEFINED).unwrap();
    JsFuture::from(Promise::resolve(&result)).await.unwrap();
    assert_eq!(
        &log(&bench)[5..],
        [
            "dispose:feedback",
            "dispose:inventory",
            "dispose:dynamic",
            "dispose:goals",
            "dispose:commands",
        ]
    );
    let result = disposer.call0(&JsValue::UNDEFINED).unwrap();
    JsFuture::from(Promise::resolve(&result)).await.unwrap();
    assert_eq!(log(&bench).len(), 10);
}

#[wasm_bindgen_test(async)]
async fn a_mount_failure_rolls_back_only_committed_predecessors() {
    configure_api_remotes(contributions().into()).unwrap();
    let bench = apiRemotesBench("dynamic");
    assert!(
        JsFuture::from(apply_api_remotes(
            Reflect::get(&bench, &JsValue::from_str("ctx")).unwrap(),
        ))
        .await
        .is_err()
    );
    assert_eq!(
        log(&bench),
        [
            "mount:commands",
            "mount:goals",
            "mount:dynamic",
            "dispose:goals",
            "dispose:commands",
        ]
    );
}
