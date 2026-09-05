//! Live compiled Connection, Typert, and API gateway boot contracts.

#![cfg(target_arch = "wasm32")]

use js_sys::{Function, Object, Promise, Reflect};
use seekdeep_client_foundation_wasm::{
    client_api_gateway_plugin, client_connection_plugin, client_typert_registry_plugin,
    configure_client_api_gateway,
};
use seekdeep_cordis::{configure_context_wrapper, create_context};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function foundationContextWrapper() {
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

export function foundationGatewayFactories() {
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

const identitySchema = { parse(value) { return value } }
const stringSchema = { parse(value) { if (typeof value !== 'string') throw new TypeError('expected string'); return value } }
export function foundationRemoteContribution() {
  const codec = schema => ({ mode: 'strict', schema })
  return {
    package: '@seekdeep-ai/foundation-test-remotes',
    descriptors: [
      {
        namespace: 'dynamicCordisRunner', method: 'inventory', invocation: { kind: 'direct' },
        parameters: [], result: codec(identitySchema),
      },
      {
        namespace: 'commands', method: 'list', invocation: { kind: 'direct' },
        scope: { context: 'agent', wire: 'agentId' },
        parameters: [{ name: 'agent', wire: 'agentId', source: 'lookup', lookup: 'agent', codec: codec(stringSchema) }],
        result: codec(identitySchema),
      },
    ],
  }
}

export function foundationInstallFetch() {
  const original = globalThis.fetch
  const originalWebSocket = globalThis.WebSocket
  const calls = []
  globalThis.WebSocket = class {
    constructor(url) {
      this.url = url
      queueMicrotask(() => this.onopen?.({ type: 'open' }))
    }
    close() { this.onclose?.({ type: 'close' }) }
  }
  globalThis.fetch = async request => {
    const body = await request.json()
    calls.push({ url: new URL(request.url).pathname, body })
    const value = body.method === 'host.describe'
      ? { local: true, canOpenPaths: true }
      : body.method === 'session.list'
        ? []
        : null
    return new Response(JSON.stringify({
      type: 'server-response',
      rpcId: body.rpcId,
      result: { ok: true, value },
    }), { status: 200, headers: { 'content-type': 'application/json' } })
  }
  return {
    calls,
    restore() {
      globalThis.fetch = original
      globalThis.WebSocket = originalWebSocket
    },
  }
}
export function foundationPlugin(root, plugin) { return root.plugin(plugin).await() }
export function foundationGet(root, name) { return root.get(name) }
export function foundationCall(api) { return api.sessions.list({}) }
export function foundationInventory(remote) { return remote.dynamicCordisRunner.inventory() }
export function foundationCommands(remote) { return remote.commands.list('session-one') }
export function foundationStart(connection, calls) {
  return connection.start({
    onStateChange(state) { calls.push(['state', state]) },
    onConnected(description) { calls.push(['connected', description.local]) },
  })
}
export function foundationCalls() { return [] }
export function foundationValues(values) { return [...values] }
export function foundationFetchCalls(control) { return [...control.calls] }
export function foundationRestore(control) { control.restore() }
export function foundationStop(handle) { handle.stop() }
export function foundationResult(response) { return response.result }
export function foundationFlush() { return new Promise(resolve => setTimeout(resolve, 10)) }

export async function foundationSchemaContract(root) {
  const check = (value, message) => { if (!value) throw new Error(message) }
  const fail = action => { try { action() } catch (error) { return error.message } throw new Error('expected rejection') }
  const registry = root.get('typert')
  const changes = []
  const observer = root.plugin({ name: 'reflection-observer', apply(ctx) {
    ctx.get('typert').local.subscribe(change => changes.push(change))
  } })
  await observer.await()
  let projections = 0
  const schema = { parse: value => value, toJSONSchema(params) { projections++; return { type: 'string', title: params.title } } }
  const model = { services: [], events: [], objects: [] }
  const invocation = { id: 'reflection.go', service: 'commands', namespace: 'commands', method: 'go', parameters: [], result: { mode: 'src-json' }, invocation: { kind: 'direct' } }
  const contribution = { package: '@seekdeep-ai/reflection', face: 'client', schemas: [{ name: 'Item', schema, extra: model }], model, invocations: [invocation] }
  const owner = root.plugin({ name: 'reflection-owner', apply(ctx) { ctx.get('typert').register(contribution) } })
  await owner.await()
  const key = '@seekdeep-ai/reflection#Item'
  const record = registry.resolve(key)
  check(record.schema === schema && record.extra === model, 'schema identity or spread fields lost')
  check(registry.get(key) === record, 'get copied the schema record')
  check(registry.getPackage(contribution.package) === undefined, 'default package face is not Host')
  check(registry.getPackage(contribution.package, 'client').model === model, 'package model identity lost')
  check(registry.list({ face: 'host' }).length === 0 && registry.list()[0] === record, 'schema filtering/order differs')
  check(registry.listPackages({ package: contribution.package }).length === 1, 'package filtering differs')
  const first = registry.toJSONSchema(key, { title: 'one' })
  const second = registry.toJSONSchema(key, { title: 'two' })
  check(first !== second && projections === 2 && second.title === 'two', 'schema projection was cached')
  check(registry.local.get('commands/go') === invocation && registry.local.list()[0] === invocation, 'local descriptor identity lost')
  check(registry.local.hasSeen('commands/go'), 'local history missing')
  check(fail(() => registry.register(contribution)) === 'typert: package face "@seekdeep-ai/reflection#client" is already registered', 'duplicate package diagnostic differs')
  const duplicate = { ...contribution, package: '@seekdeep-ai/other', schemas: [{ name: 'New', schema }], invocations: [{ ...invocation, method: 'different' }] }
  check(fail(() => registry.register(duplicate)) === 'typert: local invocation id "reflection.go" is already registered', 'duplicate invocation id accepted')
  check(registry.get('@seekdeep-ai/other#New') === undefined && registry.listPackages().length === 1, 'rejected batch published partial state')
  check(fail(() => registry.resolve('malformed')) === 'typert: invalid schema key "malformed" — expected "<package>#<name>"', 'invalid key diagnostic differs')
  check(fail(() => registry.resolve('@seekdeep-ai/reflection#Missing')).includes('registered but contributes no schema named "Missing"'), 'missing schema diagnostic differs')
  check(fail(() => registry.resolve('@seekdeep-ai/absent#Item')).includes('has no registered contribution'), 'missing package diagnostic differs')
  await owner.dispose()
  check(registry.get(key) === undefined && registry.listPackages().length === 0, 'fiber disposal retained schema/package state')
  check(registry.local.get('commands/go') === undefined && registry.local.hasSeen('commands/go'), 'descriptor withdrawal/history differs')
  check(changes.length === 2 && changes.every(change => change.kind === 'local' && change.key === 'commands/go'), 'local change notifications differ')
  await observer.dispose()
  const dispose = registry.register(contribution)
  dispose()
  check(changes.length === 2, 'disposed observer still received changes')
  return true
}

async function registryObservation(root, schema) {
  const registry = root.get('typert')
  const changes = []
  const observer = root.plugin({ apply(ctx) { ctx.get('typert').local.subscribe(change => changes.push({ ...change, live: registry.local.list().length })) } })
  await observer.await()
  const model = { services: [], events: [], objects: [] }
  const invocation = { id: 'test.read', service: 'test', namespace: 'test', method: 'read', parameters: [], result: { mode: 'src-json' }, invocation: { kind: 'direct' } }
  const contribution = { package: '@seekdeep-ai/differential', face: 'client', schemas: [{ name: 'Value', schema, extra: model }], model, invocations: [invocation] }
  const owner = root.plugin({ apply(ctx) { ctx.get('typert').register(contribution) } })
  await owner.await()
  const key = '@seekdeep-ai/differential#Value'
  const outcome = action => { try { return { value: action() } } catch (error) { return { error: error.message } } }
  const errors = [
    outcome(() => registry.register(contribution)),
    outcome(() => registry.register({ ...contribution, package: '@seekdeep-ai/other' })),
    outcome(() => registry.register({ ...contribution, package: '@seekdeep-ai/other', invocations: [{ ...invocation, method: 'other' }] })),
    outcome(() => registry.register({ ...contribution, package: 'bad#key' })),
    outcome(() => registry.register({ ...contribution, face: 'other' })),
    outcome(() => registry.resolve('bad')),
    outcome(() => registry.resolve('@seekdeep-ai/differential#Missing')),
    outcome(() => registry.resolve('@seekdeep-ai/missing#Value')),
  ]
  let filterReads = 0
  const filtered = registry.list({ get package() { filterReads++; return contribution.package } })
  const before = {
    filterReads, filtered: filtered.length,
    schemaIdentity: registry.get(key).schema === schema,
    extraIdentity: registry.get(key).extra === model,
    modelIdentity: registry.getPackage(contribution.package, 'client').model === model,
    coercedFaceIdentity: registry.getPackage(contribution.package, { toString() { return 'client' } }).model === model,
    defaultHostAbsent: registry.getPackage(contribution.package) === undefined,
    keys: registry.list().map(record => record.key),
    host: registry.list({ face: 'host' }).length,
    packages: registry.listPackages({ package: contribution.package }).map(record => record.key),
    invocationIdentity: registry.local.get('test/read') === invocation,
    schema: registry.toJSONSchema(key), errors,
  }
  await owner.dispose()
  const after = { schemas: registry.list().length, packages: registry.listPackages().length,
    descriptors: registry.local.list().length, history: registry.local.hasSeen('test/read'), emptyNullFilter: registry.list(null).length }
  await observer.dispose()
  const dispose = registry.register(contribution)
  dispose()
  return { before, after, changes }
}

async function providerObservation(root) {
  const registry = root.get('typert')
  const events = []
  const observer = root.plugin({ apply(ctx) {
    ctx.get('typert').lookups.subscribe(change => events.push({ ...change }))
    ctx.get('typert').contexts.subscribe(change => events.push({ ...change }))
  } })
  await observer.await()
  const calls = []
  const resolved = { id: 'resolved' }
  let hostContext
  const resolver = root.plugin({ apply(ctx) {
    hostContext = ctx
    ctx.get('typert').lookups.configure('agent', id => { calls.push(id); return resolved })
    ctx.get('typert').contexts.configureHost('agent', id => { calls.push(id); return ctx })
  } })
  await resolver.await()
  const absentBeforeProvider = registry.lookups.get('agent') === undefined && registry.contexts.getHost('agent') === undefined
  const provider = { parameter: 'agent', wire: 'agentId', hostTypeSymbol: 'Agent', wireTypeSymbol: 'string', resolve: id => id }
  const host = { wire: 'agentId', wireTypeSymbol: 'string', resolve: id => id }
  const binder = { identity: ctx => ctx, marker: resolved }
  let staleLookups
  const owner = root.plugin({ apply(ctx) {
    staleLookups = ctx.get('typert').lookups
    ctx.get('typert').lookups.register('agent', provider)
    ctx.get('typert').contexts.registerHost('agent', host)
    ctx.get('typert').contexts.registerClient('agent', binder)
  } })
  await owner.await()
  const projected = registry.lookups.get('agent')
  const promise = projected.resolve('lookup')
  const synchronousCall = calls.length === 1
  const resultIdentity = await promise === resolved
  const hostResult = await registry.contexts.getHost('agent').resolve('host')
  const outcome = action => { try { action(); return 'accepted' } catch (error) { return error.message } }
  const errors = [
    outcome(() => registry.lookups.register('agent', provider)),
    outcome(() => registry.lookups.configure('agent', () => undefined)),
    outcome(() => registry.contexts.registerHost('agent', host)),
    outcome(() => registry.contexts.registerClient('agent', binder)),
    outcome(() => registry.contexts.configureHost('agent', () => undefined)),
    outcome(() => registry.lookups.register('other', { ...provider, wire: '..' })),
    outcome(() => registry.contexts.registerHost('other', { ...host, wireTypeSymbol: '' })),
    outcome(() => registry.contexts.registerClient('bad#key', binder)),
  ]
  const before = { absentBeforeProvider, synchronousCall, resultIdentity, hostResolved: hostResult === hostContext,
    projected: projected !== provider, fields: Object.keys(projected),
    binderIdentity: registry.contexts.getClient('agent') === binder,
    keys: registry.lookups.keys(), definitions: registry.lookups.definitions(), errors }
  await resolver.dispose()
  const restored = registry.lookups.get('agent') === provider && registry.contexts.getHost('agent') === host
  const capturedResolver = await projected.resolve('captured') === resolved
  const removeInvalid = registry.lookups.configure('agent', null)
  const invalidResolver = await registry.lookups.get('agent').resolve('invalid').then(() => 'accepted', error => `${error.name}: ${error.message}`)
  removeInvalid()
  let recursive
  const removeRecursive = registry.lookups.configure('agent', id => id === 'outer' ? recursive.resolve('inner') : id)
  recursive = registry.lookups.get('agent')
  const reentrant = await recursive.resolve('outer')
  removeRecursive()
  await owner.dispose()
  const inactive = outcome(() => staleLookups.register('late', provider))
  const after = { restored, capturedResolver, invalidResolver, reentrant, inactive, keys: registry.lookups.keys(), definitions: registry.lookups.definitions(),
    absent: registry.lookups.get('agent') === undefined && registry.contexts.getHost('agent') === undefined && registry.contexts.getClient('agent') === undefined,
    wireDrift: outcome(() => registry.lookups.register('agent', { ...provider, wire: 'other' })) }
  await observer.dispose()
  registry.lookups.register('agent', provider)()
  registry.contexts.registerClient('agent', {})()
  return { before, after, events, calls }
}

export async function foundationSchemaSourceParity(root, pin) {
  const source = process.env.SEEKDEEP_PARITY_SOURCE
  if (!source) throw new Error('SEEKDEEP_PARITY_SOURCE is required')
  const { execFileSync } = await import('node:child_process')
  if (execFileSync('git', ['-C', source, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim() !== pin) throw new Error('oracle differs from SOURCE_SNAPSHOT')
  const { register } = await import(`${source}/node_modules/tsx/dist/esm/api/index.mjs`)
  const unregister = register()
  try {
    const [{ TypertRegistry }, { Context }, { z }] = await Promise.all([
      import(`${source}/packages/typert/registry/src/service.ts`),
      import(`${source}/vendor/cordis/lib/index.js`),
      import(`${source}/packages/typert/registry/node_modules/zod/index.js`),
    ])
    const oracle = new Context()
    const registry = oracle.plugin(TypertRegistry)
    await registry.await()
    const schema = z.string().describe('live schema')
    const expected = { reflection: await registryObservation(oracle, schema), providers: await providerObservation(oracle) }
    const actual = { reflection: await registryObservation(root, schema), providers: await providerObservation(root) }
    await registry.dispose()
    if (JSON.stringify(actual) !== JSON.stringify(expected)) throw new Error(`registry mismatch: ${JSON.stringify({ expected, actual })}`)
    return true
  } finally { unregister() }
}
export async function foundationProviderContract(root) {
  const result = await providerObservation(root)
  if (!result.before.absentBeforeProvider || !result.before.synchronousCall || !result.before.resultIdentity
    || !result.before.binderIdentity || !result.after.restored || !result.after.capturedResolver || !result.after.absent
    || result.after.keys.length !== 0 || result.after.definitions.length !== 1 || result.events.length !== 14
    || result.after.reentrant !== 'inner' || result.after.invalidResolver !== 'TypeError: resolver is not a function'
    || result.after.inactive !== 'cannot create effect on inactive context'
    || result.before.errors.includes('accepted') || !result.after.wireDrift.includes('changed its wire declaration')) {
    throw new Error(`provider contract failed: ${JSON.stringify(result)}`)
  }
  return true
}
"#)]
extern "C" {
    fn foundationContextWrapper() -> JsValue;
    fn foundationGatewayFactories() -> js_sys::Array;
    fn foundationRemoteContribution() -> JsValue;
    fn foundationInstallFetch() -> JsValue;
    fn foundationPlugin(root: &JsValue, plugin: &JsValue) -> Promise;
    fn foundationGet(root: &JsValue, name: &str) -> JsValue;
    fn foundationCall(api: &JsValue) -> Promise;
    fn foundationInventory(remote: &JsValue) -> Promise;
    fn foundationCommands(remote: &JsValue) -> Promise;
    fn foundationStart(connection: &JsValue, calls: &JsValue) -> JsValue;
    fn foundationCalls() -> JsValue;
    fn foundationValues(values: &JsValue) -> js_sys::Array;
    fn foundationFetchCalls(control: &JsValue) -> js_sys::Array;
    fn foundationRestore(control: &JsValue);
    fn foundationStop(handle: &JsValue);
    fn foundationResult(response: &JsValue) -> JsValue;
    fn foundationFlush() -> Promise;
    fn foundationSchemaContract(root: &JsValue) -> Promise;
    fn foundationSchemaSourceParity(root: &JsValue, pin: &str) -> Promise;
    fn foundationProviderContract(root: &JsValue) -> Promise;
}

#[wasm_bindgen_test(async)]
async fn providers_preserve_resolver_ownership_and_wire_history() {
    configure_context_wrapper(foundationContextWrapper()).unwrap();
    let root = create_context().unwrap();
    JsFuture::from(foundationPlugin(
        &root,
        &client_typert_registry_plugin().unwrap(),
    ))
    .await
    .unwrap();
    assert_eq!(
        JsFuture::from(foundationProviderContract(&root))
            .await
            .unwrap()
            .as_bool(),
        Some(true)
    );
}

#[wasm_bindgen_test(async)]
#[ignore = "requires the pinned source checkout and its test dependencies"]
async fn reflection_matches_source_with_live_zod_and_cordis() {
    let pin = include_str!("../../../SOURCE_SNAPSHOT")
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .unwrap();
    configure_context_wrapper(foundationContextWrapper()).unwrap();
    let root = create_context().unwrap();
    JsFuture::from(foundationPlugin(
        &root,
        &client_typert_registry_plugin().unwrap(),
    ))
    .await
    .unwrap();
    assert_eq!(
        JsFuture::from(foundationSchemaSourceParity(&root, pin))
            .await
            .unwrap()
            .as_bool(),
        Some(true)
    );
}

#[wasm_bindgen_test(async)]
async fn reflection_registration_is_atomic_and_owned_by_the_calling_fiber() {
    configure_context_wrapper(foundationContextWrapper()).unwrap();
    let root = create_context().unwrap();
    JsFuture::from(foundationPlugin(
        &root,
        &client_typert_registry_plugin().unwrap(),
    ))
    .await
    .unwrap();
    assert_eq!(
        JsFuture::from(foundationSchemaContract(&root))
            .await
            .unwrap()
            .as_bool(),
        Some(true)
    );
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)]
async fn foundations_publish_services_and_route_unary_calls() {
    configure_context_wrapper(foundationContextWrapper()).unwrap();
    let factories = foundationGatewayFactories();
    configure_client_api_gateway(factories.get(0), factories.get(1)).unwrap();
    let fetch = foundationInstallFetch();
    let root = create_context().unwrap();
    for plugin in [
        client_connection_plugin().unwrap(),
        client_typert_registry_plugin().unwrap(),
        client_api_gateway_plugin().unwrap(),
    ] {
        JsFuture::from(foundationPlugin(&root, &plugin))
            .await
            .unwrap();
    }

    let connection = foundationGet(&root, "connection");
    let api = Reflect::get(&connection, &JsValue::from_str("api")).unwrap();
    let response = JsFuture::from(foundationCall(&api)).await.unwrap();
    assert_eq!(
        Reflect::get(&foundationResult(&response), &JsValue::from_str("ok"))
            .unwrap()
            .as_bool(),
        Some(true)
    );
    let fetch_calls = foundationFetchCalls(&fetch);
    assert_eq!(fetch_calls.length(), 1);
    assert_eq!(
        Reflect::get(&fetch_calls.get(0), &JsValue::from_str("url"))
            .unwrap()
            .as_string()
            .as_deref(),
        Some("/api/session.list")
    );

    let remote = foundationGet(&root, "remote");
    let mount = Reflect::get(&remote, &JsValue::from_str("$mount"))
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    JsFuture::from(Promise::resolve(
        &mount
            .call1(&remote, &foundationRemoteContribution())
            .unwrap(),
    ))
    .await
    .unwrap();
    JsFuture::from(foundationInventory(&remote)).await.unwrap();
    JsFuture::from(foundationCommands(&remote)).await.unwrap();
    let fetch_calls = foundationFetchCalls(&fetch);
    assert_eq!(fetch_calls.length(), 3);
    assert_eq!(
        Reflect::get(&fetch_calls.get(1), &JsValue::from_str("url"))
            .unwrap()
            .as_string()
            .as_deref(),
        Some("/api/dynamicCordisRunner/inventory")
    );
    let command = Reflect::get(&fetch_calls.get(2), &JsValue::from_str("body")).unwrap();
    assert_eq!(
        Reflect::get(&command, &JsValue::from_str("method"))
            .unwrap()
            .as_string()
            .as_deref(),
        Some("commands/list")
    );
    let payload = Reflect::get(&command, &JsValue::from_str("payload")).unwrap();
    let args = Reflect::get(&payload, &JsValue::from_str("args")).unwrap();
    assert_eq!(
        Reflect::get(&args, &JsValue::from_str("agentId"))
            .unwrap()
            .as_string()
            .as_deref(),
        Some("session-one")
    );

    let callbacks = foundationCalls();
    let handle = foundationStart(&connection, &callbacks);
    JsFuture::from(foundationFlush()).await.unwrap();
    assert_eq!(
        foundationValues(&callbacks)
            .iter()
            .map(|value| js_sys::JSON::stringify(&value)
                .unwrap()
                .as_string()
                .unwrap())
            .collect::<Vec<_>>(),
        ["[\"state\",\"connected\"]", "[\"connected\",true]"]
    );
    assert!(!remote.is_undefined());
    assert!(!foundationGet(&root, "remote.commands").is_undefined());
    foundationStop(&handle);
    foundationRestore(&fetch);

    let typert = foundationGet(&root, "typert");
    assert!(typert.is_object());
    let contexts = Reflect::get(&typert, &JsValue::from_str("contexts")).unwrap();
    let register = Reflect::get(&contexts, &JsValue::from_str("registerClient"))
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    let get = Reflect::get(&contexts, &JsValue::from_str("getClient"))
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    let identity =
        Closure::wrap(Box::new(|context: JsValue| context) as Box<dyn Fn(JsValue) -> JsValue>);
    let descriptor = Object::new();
    Reflect::set(
        &descriptor,
        &JsValue::from_str("identity"),
        &identity.into_js_value(),
    )
    .unwrap();
    let dispose = register
        .call2(&contexts, &JsValue::from_str("agent"), &descriptor)
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    assert!(Object::is(
        &get.call1(&contexts, &JsValue::from_str("agent")).unwrap(),
        descriptor.as_ref(),
    ));
    assert!(
        register
            .call2(&contexts, &JsValue::from_str("agent"), &descriptor)
            .is_err()
    );
    dispose.call0(&JsValue::UNDEFINED).unwrap();
    assert!(
        get.call1(&contexts, &JsValue::from_str("agent"))
            .unwrap()
            .is_undefined()
    );
    assert!(foundationGet(&root, "connection").is_object());
    let start = Reflect::get(&connection, &JsValue::from_str("start"))
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    assert!(start.is_function());
}
