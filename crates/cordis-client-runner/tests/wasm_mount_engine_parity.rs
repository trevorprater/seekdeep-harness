//! Browser-executed evaluator, Loader, guard, crash, and teardown parity.

#![cfg(target_arch = "wasm32")]

use std::sync::Arc;

use js_sys::{Function, Object, Reflect};
use parking_lot::Mutex;
use seekdeep_cordis_client_runner::*;
use seekdeep_cordis_dynamic_types::{
    CordisDynamicPackageId, CordisDynamicPluginId, CordisDynamicPluginRunId,
    DynamicCordisRenderFailure,
};
use seekdeep_identity::SessionId;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

fn field<T: JsCast>(value: &JsValue, name: &str) -> T {
    Reflect::get(value, &JsValue::from_str(name))
        .unwrap()
        .dyn_into::<T>()
        .unwrap_or_else(|_| panic!("{name} has the wrong JavaScript type"))
}

fn request(run: &str, code: &str) -> ClientLoadRequest {
    ClientLoadRequest {
        plugin_id: CordisDynamicPluginId::new("dyn-1"),
        package_id: CordisDynamicPackageId::new("pkg-1"),
        plugin_run_id: CordisDynamicPluginRunId::new(run),
        agent_id: SessionId::new("session-1"),
        name: "demo".to_owned(),
        code: code.to_owned(),
    }
}

fn error_message(error: &JsValue) -> String {
    Reflect::get(error, &JsValue::from_str("message"))
        .unwrap()
        .as_string()
        .unwrap()
}

#[expect(
    clippy::too_many_lines,
    reason = "One JavaScript expression defines the shared browser fixture realm"
)]
fn browser_bench() -> JsValue {
    Function::new_no_args(
        r"
const factories = new Map();
const fibers = new Map();
const services = Object.create(null);
const entries = [];
const invalidated = [];
const removed = [];
const invoked = [];
const guardFailures = [];
const contextEvents = new Map();
const remoteListeners = new Map();
let crashListener;
let nextEntry = 0;
let dropNextFiber = false;

globalThis.__ModuleLoader__ = {
  load({ id, factory }) { factories.set(id, factory); },
};

const slots = {
  onEntryError(listener) {
    crashListener = listener;
    return () => { crashListener = undefined; };
  },
  spec() { return undefined; },
  register(options, component) {
    const entry = { options, component };
    entries.push(entry);
    return () => {
      const at = entries.indexOf(entry);
      if (at >= 0) entries.splice(at, 1);
    };
  },
};
services.slots = slots;

class TestContext {
  constructor(inject = {}) {
    this.fiber = { inject };
    this.effects = [];
  }
  get(name) { return services[name]; }
  effect(callback) {
    const cleanup = callback();
    let active = true;
    const dispose = () => {
      if (!active) return;
      active = false;
      const at = this.effects.indexOf(dispose);
      if (at >= 0) this.effects.splice(at, 1);
      cleanup?.();
    };
    this.effects.push(dispose);
    return dispose;
  }
  on(name, listener) {
    const bucket = contextEvents.get(name) ?? [];
    bucket.push(listener);
    contextEvents.set(name, bucket);
    return this.effect(() => () => {
      const at = bucket.indexOf(listener);
      if (at >= 0) bucket.splice(at, 1);
    });
  }
  once(name, listener) {
    const dispose = this.on(name, (...args) => { dispose(); listener(...args); });
    return dispose;
  }
  provide(name, value) {
    return this.effect(() => {
      services[name] = value;
      return () => { if (services[name] === value) delete services[name]; };
    });
  }
  mixin(name, methods) {
    return this.effect(() => {
      for (const method of methods) {
        TestContext.prototype[method] = function(...args) {
          return services[name][method](...args);
        };
      }
      return () => { for (const method of methods) delete TestContext.prototype[method]; };
    });
  }
  emit(name, ...args) {
    for (const listener of [...contextEvents.get(name) ?? []]) listener(...args);
  }
  disposeAll() { for (const dispose of [...this.effects].reverse()) dispose(); }
}

const ctx = new TestContext();
const modules = {
  invalidate(id) { invalidated.push(id); },
};
const loader = {
  async create({ name }) {
    const factory = factories.get(name);
    if (factory === undefined) throw new Error(`no factory for ${name}`);
    const plugin = factory();
    const inject = Object.fromEntries((plugin.inject ?? []).map(name => [name, {}]));
    const pluginCtx = new TestContext(inject);
    let activation;
    try { activation = Promise.resolve(plugin.apply(pluginCtx)); }
    catch (error) { activation = Promise.reject(error); }
    const id = `entry-${++nextEntry}`;
    fibers.set(id, {
      fiber: {
        inject,
        await() { return activation; },
        async dispose() {
          for (const dispose of pluginCtx.effects.reverse()) dispose();
        },
      },
    });
    return id;
  },
  resolve(id) {
    if (dropNextFiber) {
      dropNextFiber = false;
      return { fiber: undefined };
    }
    return fibers.get(id) ?? { fiber: undefined };
  },
  async remove(id) {
    removed.push(id);
    const record = fibers.get(id);
    fibers.delete(id);
    await record?.fiber.dispose();
  },
};
const invoke = (pluginId, pluginRunId, method, args) => {
  invoked.push({ pluginId, pluginRunId, method, args });
  return Promise.resolve('pong');
};
const reportGuard = (agentId, pluginId, pluginRunId, error) => {
  guardFailures.push({ agentId, pluginId, pluginRunId, error });
};
services.loader = loader;
services.modules = modules;
services.slots = slots;
return {
  ctx,
  loader,
  modules,
  slots,
  react: {},
  invoke,
  reportGuard,
  entries,
  invalidated,
  removed,
  invoked,
  guardFailures,
  getService(name) { return services[name]; },
  installRemote(namespace) {
    const remote = {
      dynamicCordisRunner: namespace,
      $on(event, listener) {
        const bucket = remoteListeners.get(event) ?? [];
        bucket.push(listener);
        remoteListeners.set(event, bucket);
        return () => {
          const at = bucket.indexOf(listener);
          if (at >= 0) bucket.splice(at, 1);
        };
      },
      $dispatch(event, args) {
        for (const listener of [...remoteListeners.get(event) ?? []]) listener(...args);
      },
    };
    services.remote = remote;
    services['remote.dynamicCordisRunner'] = namespace;
    return remote;
  },
  crash(slot, entry, error, abdicated) {
    crashListener?.(slot, entry, error, { abdicated });
  },
  dropNextFiber() { dropNextFiber = true; },
  watching() { return crashListener !== undefined; },
};
",
    )
    .call0(&JsValue::UNDEFINED)
    .unwrap()
}

fn timer_context() -> JsValue {
    Function::new_no_args(
        r"
const services = Object.create(null);
const effects = [];
const ctx = {
  provide(name, service) { services[name] = service; return () => { delete services[name]; }; },
  mixin(name, methods) {
    for (const method of methods) ctx[method] = (...args) => services[name][method](...args);
    return () => { for (const method of methods) delete ctx[method]; };
  },
  effect(installer, label) {
    const cleanup = installer();
    let active = true;
    const dispose = () => {
      if (!active) return;
      active = false;
      const at = effects.indexOf(dispose);
      if (at >= 0) effects.splice(at, 1);
      cleanup?.();
    };
    effects.push(dispose);
    return dispose;
  },
  disposeAll() { for (const dispose of [...effects].reverse()) dispose(); },
  effectCount() { return effects.length; },
};
return ctx;
",
    )
    .call0(&JsValue::UNDEFINED)
    .unwrap()
}

fn remote_bench() -> JsValue {
    Function::new_no_args(
        r"
const calls = [];
const reports = [];
let invokeResult = { ok: true, value: 'pong' };
let invokeThrow;
let clientCode = 'return { apply() {} }';
const answered = value => Promise.resolve({ ok: true, value });
const namespace = {
  runHostHalf(...args) {
    calls.push(['runHostHalf', ...args]);
    return answered({
      ok: true,
      pluginId: 'dyn-1',
      packageId: 'pkg-1',
      pluginRunId: 'run-1',
      waitingFor: ['host-missing'],
      startedHere: true,
    });
  },
  getClientCode(...args) {
    calls.push(['getClientCode', ...args]);
    return answered({
      code: clientCode,
      name: 'demo',
      pluginId: 'dyn-1',
      packageId: 'pkg-1',
      pluginRunId: 'run-1',
    });
  },
  resolveRequestRun(...args) {
    calls.push(['resolveRequestRun', ...args]);
    return answered({ accepted: true });
  },
  settleUserRun(...args) {
    calls.push(['settleUserRun', ...args]);
    return answered({
      ok: true,
      status: 'running',
      pluginId: 'dyn-1',
      packageId: 'pkg-1',
      pluginRunId: 'run-1',
      waitingFor: [],
      mode: 'run',
    });
  },
  invoke(...args) {
    calls.push(['invoke', ...args]);
    if (invokeThrow !== undefined) return Promise.reject(invokeThrow);
    return answered(invokeResult);
  },
  syncInspectManifest(...args) {
    calls.push(['syncInspectManifest', ...args]);
    return answered(null);
  },
  resolveInspectQuery(...args) {
    calls.push(['resolveInspectQuery', ...args]);
    return answered({ accepted: true });
  },
  reportRenderFailure(...args) {
    reports.push(['render', ...args]);
    return answered(null);
  },
  reportClientGuardFailure(...args) {
    reports.push(['guard', ...args]);
    return answered(null);
  },
};
return {
  namespace,
  calls,
  reports,
  setInvoke(value) { invokeResult = value; invokeThrow = undefined; },
  rejectInvoke(value) { invokeThrow = value; },
  setClientCode(value) { clientCode = value; },
};
",
    )
    .call0(&JsValue::UNDEFINED)
    .unwrap()
}

fn guard_bench() -> JsValue {
    Function::new_no_args(
        r"
const registrations = [];
const effects = [];
const verbs = [];
const reports = [];
const claims = [];
class TestContext {}
const foreignContext = new TestContext();
const ctx = new TestContext();
ctx.fiber = { inject: {} };
ctx.effect = (installer, label) => {
  const cleanup = installer();
  let active = true;
  const dispose = () => {
    if (!active) return;
    active = false;
    const at = effects.findIndex(row => row.dispose === dispose);
    if (at >= 0) effects.splice(at, 1);
    cleanup?.();
  };
  effects.push({ label, dispose });
  return dispose;
};
for (const verb of ['on', 'once', 'provide', 'timeout', 'interval', 'setTimeout', 'setInterval', 'throttle', 'debounce']) {
  ctx[verb] = function(...args) {
    verbs.push({ verb, receiver: this === ctx, args });
    return `${verb}-result`;
  };
}
const slots = {
  spec(name) { return name === 'chain' ? { kind: 'chain' } : undefined; },
  register(options, component) {
    const row = { options, component };
    registrations.push(row);
    return () => {
      const at = registrations.indexOf(row);
      if (at >= 0) registrations.splice(at, 1);
    };
  },
  entries() { return [...registrations]; },
  contextValue: foreignContext,
};
const themeCalls = [];
const theme = {
  overrideTokens(source, tokens) {
    const row = { source, tokens, disposed: false };
    themeCalls.push(row);
    return () => { row.disposed = true; };
  },
  current() { return 'dark'; },
  async currentAsync() { return 'light'; },
  contextValue() { return foreignContext; },
  async contextAsync() { return foreignContext; },
};
const services = {
  slots,
  theme,
  timer: {},
  primitive: 42,
  contextValue: foreignContext,
  safe: {
    value: 'safe',
    contextValue: foreignContext,
    contextMethod() { return foreignContext; },
    async contextAsync() { return foreignContext; },
    method(value) { return [this === services.safe, value]; },
  },
};
ctx.get = name => services[name];
return {
  ctx,
  registrations,
  effects,
  verbs,
  reports,
  claims,
  themeCalls,
  claim(component) { claims.push(component); },
  report(error) { reports.push(error); },
  isContext(value) { return value instanceof TestContext; },
  disposeAll() { for (const row of [...effects].reverse()) row.dispose(); },
};
",
    )
    .call0(&JsValue::UNDEFINED)
    .unwrap()
}

fn guarded_context(bench: &JsValue, declared: &[&str]) -> (JsValue, js_sys::Array) {
    let declared_values = js_sys::Array::new();
    for service in declared {
        declared_values.push(&JsValue::from_str(service));
    }
    let policy = WasmClientGuardPolicy::new(
        "dyn-1".to_owned(),
        "pkg-1".to_owned(),
        "run-1".to_owned(),
        "demo".to_owned(),
        declared_values,
    );
    let ledger = js_sys::Array::new();
    let facade = create_client_context(
        field::<Object>(bench, "ctx").into(),
        policy,
        ledger.clone(),
        field(bench, "claim"),
        field(bench, "report"),
        field(bench, "isContext"),
    )
    .unwrap();
    (facade, ledger)
}

async fn await_promise(value: JsValue) -> Result<JsValue, JsValue> {
    wasm_bindgen_futures::JsFuture::from(value.dyn_into::<js_sys::Promise>().unwrap()).await
}

fn mounted_runtime(
    bench: &JsValue,
) -> (
    Arc<DynamicCordisClientRuntime>,
    Arc<Mutex<Vec<DynamicCordisRenderFailure>>>,
) {
    let engine = Arc::new(
        WasmClientMountEngine::new(
            field::<Object>(bench, "ctx").into(),
            field::<Object>(bench, "loader").into(),
            field::<Object>(bench, "modules").into(),
            &field::<Object>(bench, "slots").into(),
            field::<Object>(bench, "react").into(),
            field(bench, "invoke"),
            field(bench, "reportGuard"),
        )
        .unwrap(),
    );
    let upstream = Arc::new(Mutex::new(Vec::<DynamicCordisRenderFailure>::new()));
    let observed = upstream.clone();
    let runtime = DynamicCordisClientRuntime::new(
        engine,
        Arc::new(WasmClientTaskSpawner),
        Arc::new(move |_, _, _, failure| observed.lock().push(failure)),
    );
    (runtime, upstream)
}

#[wasm_bindgen_test]
async fn evaluator_executes_both_forms_and_the_harness_trap_is_constructible() {
    let react = Object::new();
    let styles = Object::new();
    let invoke = Function::new_with_args("method,args", "return Promise.resolve([method, args]);");
    let note_error = Function::new_with_args("message", "globalThis.__note = message;");

    let object = evaluate_client_half(
        "dyn-1".to_owned(),
        "return { inject: ['slots'], apply(ctx) { return ctx; } }".to_owned(),
        react.clone().into(),
        styles.clone().into(),
        invoke.clone(),
        note_error.clone(),
    )
    .await
    .unwrap();
    assert!(object.is_object());
    assert!(
        Reflect::get(&object, &JsValue::from_str("apply"))
            .unwrap()
            .is_function()
    );

    let function = evaluate_client_half(
        "dyn-1".to_owned(),
        "return ctx => ctx".to_owned(),
        react.into(),
        styles.into(),
        invoke,
        note_error,
    )
    .await
    .unwrap();
    assert!(function.is_function());

    let trapped = evaluate_client_half(
        "dyn-1".to_owned(),
        "return { apply() { return harness.define; } }".to_owned(),
        Object::new().into(),
        Object::new().into(),
        Function::new_no_args("return Promise.resolve(null)"),
        Function::new_no_args(""),
    )
    .await
    .unwrap();
    let apply: Function = field(&trapped, "apply");
    let error = apply.call0(&trapped).unwrap_err();
    let message = error_message(&error);
    assert!(message.contains("HOST half (`code`)"));
}

#[wasm_bindgen_test]
#[expect(
    clippy::too_many_lines,
    reason = "One browser scenario keeps setup, assertions, and teardown together"
)]
async fn evaluator_symbols_failures_console_and_styles_execute_in_the_browser() {
    let react = Object::new();
    Reflect::set(&react, &JsValue::from_str("pageCopy"), &JsValue::TRUE).unwrap();
    let invoke = Function::new_with_args(
        "method,args",
        "globalThis.__evaluatorInvoke = [method, args]; return Promise.resolve(args);",
    );
    let notes = js_sys::Array::new();
    Reflect::set(
        &js_sys::global(),
        &JsValue::from_str("__evaluatorNotes"),
        &notes,
    )
    .unwrap();
    let note_error =
        Function::new_with_args("message", "globalThis.__evaluatorNotes.push(message);");
    let evaluated = evaluate_client_half(
        "dyn-1".to_owned(),
        r"
return {
  apply(ctx) {
    console.log('quiet');
    console.error('text', new Error('boom'), { a: 1 }, undefined, ctx.circular);
    return React;
  },
};
"
        .to_owned(),
        react.clone().into(),
        Object::new().into(),
        invoke.clone(),
        note_error.clone(),
    )
    .await
    .unwrap();
    let circular =
        Function::new_no_args("const value = {}; value.self = value; return { circular: value };")
            .call0(&JsValue::UNDEFINED)
            .unwrap();
    let apply: Function = field(&evaluated, "apply");
    let returned = apply.call1(&evaluated, &circular).unwrap();
    assert!(Object::is(&returned, &react));
    assert_eq!(notes.length(), 1);
    assert_eq!(
        notes.get(0).as_string().as_deref(),
        Some("text boom {\"a\":1} undefined [unserializable console argument]")
    );

    let long = evaluate_client_half(
        "dyn-1".to_owned(),
        "return { apply() { console.error('x'.repeat(900)) } }".to_owned(),
        react.clone().into(),
        Object::new().into(),
        invoke.clone(),
        note_error.clone(),
    )
    .await
    .unwrap();
    let long_apply: Function = field(&long, "apply");
    long_apply.call0(&long).unwrap();
    assert_eq!(
        notes.get(1).as_string().unwrap().encode_utf16().count(),
        500
    );

    for (source, expected) in [
        (
            "return () => setTimeout(() => {}, 1)",
            "browser timer globals",
        ),
        (
            "return () => setInterval(() => {}, 1)",
            "browser timer globals",
        ),
        ("return () => clearTimeout(1)", "browser timer globals"),
        ("return () => clearInterval(1)", "browser timer globals"),
        (
            "return () => fetch('/x')",
            "network belongs to the HOST half",
        ),
        (
            "return () => require('react')",
            "React arrives as the `React` closure symbol",
        ),
    ] {
        let plugin = evaluate_client_half(
            "dyn-1".to_owned(),
            source.to_owned(),
            react.clone().into(),
            Object::new().into(),
            invoke.clone(),
            note_error.clone(),
        )
        .await
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
        assert!(error_message(&plugin.call0(&JsValue::UNDEFINED).unwrap_err()).contains(expected));
    }

    let host = evaluate_client_half(
        "dyn-1".to_owned(),
        "return { apply: () => host.call('ping', { a: 1 }) }".to_owned(),
        react.clone().into(),
        Object::new().into(),
        invoke.clone(),
        note_error.clone(),
    )
    .await
    .unwrap();
    let host_apply: Function = field(&host, "apply");
    let promise = host_apply
        .call0(&host)
        .unwrap()
        .dyn_into::<js_sys::Promise>()
        .unwrap();
    wasm_bindgen_futures::JsFuture::from(promise).await.unwrap();
    let invoked = Reflect::get(&js_sys::global(), &JsValue::from_str("__evaluatorInvoke"))
        .unwrap()
        .dyn_into::<js_sys::Array>()
        .unwrap();
    assert_eq!(invoked.get(0).as_string().as_deref(), Some("ping"));
    assert_eq!(
        Reflect::get(&invoked.get(1), &JsValue::from_str("a"))
            .unwrap()
            .as_f64(),
        Some(1.0)
    );

    let omitted = evaluate_client_half(
        "dyn-1".to_owned(),
        "return { apply: () => host.call('listServices') }".to_owned(),
        react.clone().into(),
        Object::new().into(),
        invoke.clone(),
        note_error.clone(),
    )
    .await
    .unwrap();
    let omitted_apply: Function = field(&omitted, "apply");
    let promise = omitted_apply
        .call0(&omitted)
        .unwrap()
        .dyn_into::<js_sys::Promise>()
        .unwrap();
    wasm_bindgen_futures::JsFuture::from(promise).await.unwrap();
    let invoked = Reflect::get(&js_sys::global(), &JsValue::from_str("__evaluatorInvoke"))
        .unwrap()
        .dyn_into::<js_sys::Array>()
        .unwrap();
    assert!(invoked.get(1).is_null());

    let parse = evaluate_client_half(
        "dyn-1".to_owned(),
        "return (".to_owned(),
        react.clone().into(),
        Object::new().into(),
        invoke.clone(),
        note_error.clone(),
    )
    .await
    .unwrap_err();
    assert!(error_message(&parse).contains("no JSX, no TypeScript"));
    let missing = evaluate_client_half(
        "dyn-1".to_owned(),
        "const x = 1".to_owned(),
        react.clone().into(),
        Object::new().into(),
        invoke.clone(),
        note_error.clone(),
    )
    .await
    .unwrap_err();
    assert!(error_message(&missing).contains("did you forget `return`"));
    let invalid = evaluate_client_half(
        "dyn-1".to_owned(),
        "return 42".to_owned(),
        react.clone().into(),
        Object::new().into(),
        invoke.clone(),
        note_error.clone(),
    )
    .await
    .unwrap_err();
    assert!(error_message(&invalid).contains("must `return` a plugin"));

    let global = js_sys::global();
    let original_function = Reflect::get(&global, &JsValue::from_str("Function")).unwrap();
    let boom = js_sys::TypeError::new("engine refused");
    let stub = Function::new_with_args("boom", "return function() { throw boom; };")
        .call1(&JsValue::UNDEFINED, &boom)
        .unwrap();
    Reflect::set(&global, &JsValue::from_str("Function"), &stub).unwrap();
    let construction = evaluate_client_half(
        "dyn-1".to_owned(),
        "return () => {}".to_owned(),
        react.into(),
        Object::new().into(),
        invoke,
        note_error,
    )
    .await
    .unwrap_err();
    Reflect::set(&global, &JsValue::from_str("Function"), &original_function).unwrap();
    assert!(Object::is(&construction, &boom));

    let styles = WasmDynamicCordisStyles::new("dyn-style-test".to_owned());
    let first = styles.insert(JsValue::from_str(".a {}")).unwrap();
    styles.insert(JsValue::from_str(".b {}")).unwrap();
    assert_eq!(styles.count(), 2);
    first.call0(&JsValue::UNDEFINED).unwrap();
    assert_eq!(styles.count(), 1);
    assert!(styles.insert(JsValue::from_f64(42.0)).is_err());
    styles.dispose();
    assert_eq!(styles.count(), 0);
}

#[wasm_bindgen_test]
async fn real_wasm_engine_mounts_projects_crashes_and_tears_down() {
    let bench = browser_bench();
    let (runtime, upstream) = mounted_runtime(&bench);

    let result = runtime
        .load(request(
            "run-1",
            r"
return {
  inject: ['slots', 'absent'],
  apply(ctx) {
    styles.insert('.cordis-wasm-test {}');
    ctx.slots.register({ name: 'root' }, () => null);
    globalThis.__cordisHostCall = host.call('ping', { a: 1 });
  },
};
",
        ))
        .await
        .unwrap();
    assert!(
        matches!(
            result,
            ClientLoadResult::Success { ref waiting_for, .. }
                if waiting_for.as_deref() == Some(&["absent".to_owned()][..])
        ),
        "unexpected mount result: {result:?}"
    );
    let snapshot = runtime.snapshot();
    assert_eq!(snapshot[0].slots, ["root"]);
    assert_eq!(snapshot[0].style_count, 1);
    let invalidated: js_sys::Array = field(&bench, "invalidated");
    assert_eq!(invalidated.length(), 1);
    let host_call = Reflect::get(&js_sys::global(), &JsValue::from_str("__cordisHostCall"))
        .unwrap()
        .dyn_into::<js_sys::Promise>()
        .unwrap();
    assert_eq!(
        wasm_bindgen_futures::JsFuture::from(host_call)
            .await
            .unwrap()
            .as_string()
            .as_deref(),
        Some("pong")
    );

    let entries: js_sys::Array = field(&bench, "entries");
    let crash: Function = field(&bench, "crash");
    crash
        .call4(
            &bench,
            &JsValue::from_str("root"),
            &entries.get(0),
            &js_sys::Error::new("window.setInterval is not a function"),
            &JsValue::TRUE,
        )
        .unwrap();
    assert_eq!(upstream.lock().len(), 1);
    assert!(upstream.lock()[0].message.contains("browser timer globals"));
    assert!(
        runtime
            .render_failures()
            .contains_key(&CordisDynamicPluginId::new("dyn-1"))
    );

    runtime.dispose().await;
    assert!(runtime.snapshot().is_empty());
    let removed: js_sys::Array = field(&bench, "removed");
    assert_eq!(removed.length(), 1);
    assert_eq!(invalidated.length(), 2);
    let watching: Function = field(&bench, "watching");
    assert_eq!(watching.call0(&bench).unwrap().as_bool(), Some(false));
    let style_count = Function::new_no_args(
        "return document.querySelectorAll('style[data-dyn=\"dyn-1\"]').length;",
    )
    .call0(&JsValue::UNDEFINED)
    .unwrap()
    .as_f64();
    assert_eq!(style_count, Some(0.0));
}

#[wasm_bindgen_test]
async fn real_wasm_engine_classifies_every_stage_and_recovers_after_rejection() {
    let bench = browser_bench();
    let (runtime, _) = mounted_runtime(&bench);

    let evaluated = runtime
        .load(request(
            "run-evaluate",
            "styles.insert('.evaluate-leak {}'); return 42",
        ))
        .await
        .unwrap();
    assert!(matches!(
        evaluated,
        ClientLoadResult::Failure {
            cause: ClientLoadErrorCause::Evaluate,
            ..
        }
    ));
    let style_count = Function::new_no_args(
        "return document.querySelectorAll('style[data-dyn=\"dyn-1\"]').length;",
    )
    .call0(&JsValue::UNDEFINED)
    .unwrap()
    .as_f64();
    assert_eq!(style_count, Some(0.0));

    let raw = runtime
        .load(request("run-raw", "throw 'raw rejection'"))
        .await
        .unwrap();
    assert!(matches!(
        raw,
        ClientLoadResult::Failure {
            cause: ClientLoadErrorCause::Evaluate,
            ref error,
        } if error.message == "raw rejection" && error.stack.is_none()
    ));

    let activation = runtime
        .load(request(
            "run-activate",
            "return { apply() { throw new Error('apply exploded') } }",
        ))
        .await
        .unwrap();
    assert!(matches!(
        activation,
        ClientLoadResult::Failure {
            cause: ClientLoadErrorCause::Activate,
            ref error,
        } if error.message == "apply exploded" && error.stack.is_some()
    ));
    let removed: js_sys::Array = field(&bench, "removed");
    assert_eq!(removed.length(), 1);

    let drop_next: Function = field(&bench, "dropNextFiber");
    drop_next.call0(&bench).unwrap();
    let imported = runtime
        .load(request("run-import", "return { apply() {} }"))
        .await
        .unwrap();
    assert!(matches!(
        imported,
        ClientLoadResult::Failure {
            cause: ClientLoadErrorCause::ModuleImport,
            ref error,
        } if error.message == "module import failed (see the browser console)"
    ));
    assert_eq!(removed.length(), 2);

    let global = js_sys::global();
    let sink = Reflect::get(&global, &JsValue::from_str("__ModuleLoader__")).unwrap();
    assert!(Reflect::delete_property(&global, &JsValue::from_str("__ModuleLoader__")).unwrap());
    let rejection = runtime
        .load(request("run-rejected", "return { apply() {} }"))
        .await
        .unwrap_err();
    assert!(rejection.message.contains("__ModuleLoader__ is missing"));
    assert!(Reflect::set(&global, &JsValue::from_str("__ModuleLoader__"), &sink).unwrap());

    let function_form = runtime
        .load(request(
            "run-recovered",
            "return ctx => { globalThis.__cordisFunctionForm = ctx !== undefined }",
        ))
        .await
        .unwrap();
    assert!(matches!(function_form, ClientLoadResult::Success { .. }));
    assert_eq!(
        Reflect::get(
            &js_sys::global(),
            &JsValue::from_str("__cordisFunctionForm")
        )
        .unwrap()
        .as_bool(),
        Some(true)
    );
    runtime.dispose().await;
}

#[wasm_bindgen_test]
async fn component_identity_is_the_only_slot_crash_attribution_key() {
    let bench = browser_bench();
    let (runtime, upstream) = mounted_runtime(&bench);
    runtime
        .load(request(
            "run-identity",
            r"
return {
  inject: ['slots'],
  apply(ctx) {
    ctx.slots.register({ name: 'root' }, () => null);
    ctx.slots.register({ name: 'root' }, 'not-a-component');
    ctx.slots.register({ name: 'root' }, null);
  },
};
",
        ))
        .await
        .unwrap();
    let entries: js_sys::Array = field(&bench, "entries");
    let crash: Function = field(&bench, "crash");
    let unrelated = Object::new();
    Reflect::set(
        &unrelated,
        &JsValue::from_str("component"),
        &Function::new_no_args("return null"),
    )
    .unwrap();
    for entry in [unrelated.into(), entries.get(1), entries.get(2)] {
        crash
            .call4(
                &bench,
                &JsValue::from_str("root"),
                &entry,
                &js_sys::Error::new("unowned"),
                &JsValue::TRUE,
            )
            .unwrap();
    }
    assert!(upstream.lock().is_empty());

    crash
        .call4(
            &bench,
            &JsValue::from_str("root"),
            &entries.get(0),
            &js_sys::Error::new("owned"),
            &JsValue::FALSE,
        )
        .unwrap();
    assert_eq!(upstream.lock().len(), 1);
    assert!(!upstream.lock()[0].abdicated);
    runtime.dispose().await;
}

#[wasm_bindgen_test]
#[expect(
    clippy::too_many_lines,
    reason = "One browser scenario keeps setup, assertions, and teardown together"
)]
async fn rust_wasm_timer_service_preserves_callbacks_promises_iterators_and_wrappers() {
    let ctx = timer_context();
    let service = install_wasm_client_timer(&ctx).unwrap();
    for method in [
        "timeout",
        "interval",
        "throttle",
        "debounce",
        "setTimeout",
        "setInterval",
    ] {
        assert!(
            Reflect::get(&ctx, &JsValue::from_str(method))
                .unwrap()
                .is_function()
        );
    }
    let timeout: Function = field(&service, "timeout");
    let interval: Function = field(&service, "interval");

    let counter = Object::new();
    Reflect::set(
        &counter,
        &JsValue::from_str("value"),
        &JsValue::from_f64(0.0),
    )
    .unwrap();
    let callback = Function::new_with_args("counter", "return () => { counter.value += 1; };")
        .call1(&JsValue::UNDEFINED, &counter)
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    timeout
        .call2(&service, &callback, &JsValue::from_f64(2.0))
        .unwrap();
    await_promise(timeout.call1(&service, &JsValue::from_f64(15.0)).unwrap())
        .await
        .unwrap();
    assert_eq!(
        Reflect::get(&counter, &JsValue::from_str("value"))
            .unwrap()
            .as_f64(),
        Some(1.0)
    );

    let cancelled = timeout
        .call2(&service, &callback, &JsValue::from_f64(2.0))
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    cancelled.call0(&JsValue::UNDEFINED).unwrap();
    await_promise(timeout.call1(&service, &JsValue::from_f64(10.0)).unwrap())
        .await
        .unwrap();
    assert_eq!(
        Reflect::get(&counter, &JsValue::from_str("value"))
            .unwrap()
            .as_f64(),
        Some(1.0)
    );

    let interval_disposer = interval
        .call2(&service, &callback, &JsValue::from_f64(2.0))
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    await_promise(timeout.call1(&service, &JsValue::from_f64(12.0)).unwrap())
        .await
        .unwrap();
    interval_disposer.call0(&JsValue::UNDEFINED).unwrap();
    let after_interval = Reflect::get(&counter, &JsValue::from_str("value"))
        .unwrap()
        .as_f64()
        .unwrap();
    assert!(after_interval > 1.0);
    await_promise(timeout.call1(&service, &JsValue::from_f64(8.0)).unwrap())
        .await
        .unwrap();
    assert_eq!(
        Reflect::get(&counter, &JsValue::from_str("value"))
            .unwrap()
            .as_f64(),
        Some(after_interval)
    );

    let iterator = interval.call1(&service, &JsValue::from_f64(2.0)).unwrap();
    let next: Function = field(&iterator, "next");
    let tick = await_promise(next.call0(&iterator).unwrap()).await.unwrap();
    assert_eq!(
        Reflect::get(&tick, &JsValue::from_str("done"))
            .unwrap()
            .as_bool(),
        Some(false)
    );
    let return_value: Function = field(&iterator, "return");
    let returned = await_promise(
        return_value
            .call1(&iterator, &JsValue::from_str("finished"))
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        Reflect::get(&returned, &JsValue::from_str("value"))
            .unwrap()
            .as_string()
            .as_deref(),
        Some("finished")
    );
    let replay = await_promise(next.call0(&iterator).unwrap()).await.unwrap();
    assert_eq!(
        Reflect::get(&replay, &JsValue::from_str("value"))
            .unwrap()
            .as_string()
            .as_deref(),
        Some("finished")
    );
    let same_iterator = Function::new_with_args(
        "iterator",
        "return iterator[Symbol.asyncIterator]() === iterator;",
    )
    .call1(&JsValue::UNDEFINED, &iterator)
    .unwrap();
    assert_eq!(same_iterator.as_bool(), Some(true));

    let thrown_iterator = interval.call1(&service, &JsValue::from_f64(100.0)).unwrap();
    let throw_value: Function = field(&thrown_iterator, "throw");
    let reason = Object::new();
    await_promise(throw_value.call1(&thrown_iterator, &reason).unwrap())
        .await
        .unwrap();
    let thrown_next: Function = field(&thrown_iterator, "next");
    let observed = await_promise(thrown_next.call0(&thrown_iterator).unwrap())
        .await
        .unwrap_err();
    assert!(Object::is(&observed, &reason));

    let calls = js_sys::Array::new();
    let recorder = Function::new_with_args("calls", "return value => calls.push(value);")
        .call1(&JsValue::UNDEFINED, &calls)
        .unwrap();
    let throttle: Function = field(&service, "throttle");
    let throttled = throttle
        .call2(&service, &recorder, &JsValue::from_f64(8.0))
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    throttled
        .call1(&JsValue::UNDEFINED, &JsValue::from_str("first"))
        .unwrap();
    throttled
        .call1(&JsValue::UNDEFINED, &JsValue::from_str("trailing"))
        .unwrap();
    await_promise(timeout.call1(&service, &JsValue::from_f64(20.0)).unwrap())
        .await
        .unwrap();
    assert_eq!(calls.length(), 2);
    assert_eq!(calls.get(0).as_string().as_deref(), Some("first"));
    assert_eq!(calls.get(1).as_string().as_deref(), Some("trailing"));

    let debounce: Function = field(&service, "debounce");
    let debounced = debounce
        .call2(&service, &recorder, &JsValue::from_f64(8.0))
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    debounced
        .call1(&JsValue::UNDEFINED, &JsValue::from_str("discarded"))
        .unwrap();
    debounced
        .call1(&JsValue::UNDEFINED, &JsValue::from_str("debounced"))
        .unwrap();
    await_promise(timeout.call1(&service, &JsValue::from_f64(20.0)).unwrap())
        .await
        .unwrap();
    assert_eq!(calls.length(), 3);
    assert_eq!(calls.get(2).as_string().as_deref(), Some("debounced"));

    let pending = timeout
        .call1(&service, &JsValue::from_f64(1_000.0))
        .unwrap();
    let dispose_all: Function = field(&ctx, "disposeAll");
    dispose_all.call0(&ctx).unwrap();
    let disposed = await_promise(pending).await.unwrap_err();
    assert_eq!(error_message(&disposed), CONTEXT_DISPOSED_FOR_TEST);
}

const CONTEXT_DISPOSED_FOR_TEST: &str = "Context has been disposed";

#[wasm_bindgen_test]
#[expect(
    clippy::too_many_lines,
    reason = "One browser scenario keeps setup, assertions, and teardown together"
)]
async fn rust_remote_bindings_fold_carriers_and_preserve_teaching_failures() {
    let bench = remote_bench();
    let namespace: Object = field(&bench, "namespace");
    let host = WasmCordisRunHost::new(namespace.clone().into());
    let plan = CordisUserRunRequest {
        agent_id: SessionId::new("session-1"),
        plugin_id: CordisDynamicPluginId::new("dyn-1"),
        package_id: CordisDynamicPackageId::new("pkg-1"),
        mode: seekdeep_cordis_dynamic_types::DynamicCordisRunMode::Run,
        has_client_half: true,
    };
    let started = host.run_host_half(plan.clone(), None, false).await.unwrap();
    assert!(matches!(
        started,
        seekdeep_cordis_dynamic_types::DynamicCordisHostHalfResult::Success {
            ref waiting_for,
            started_here: true,
            ..
        } if waiting_for == &["host-missing"]
    ));
    let source = host
        .get_client_code(
            plan.agent_id.clone(),
            plan.plugin_id.clone(),
            CordisDynamicPluginRunId::new("run-1"),
        )
        .await
        .unwrap();
    assert_eq!(source.name, "demo");
    let resolution = seekdeep_cordis_dynamic_types::DynamicCordisRunResolution::Success {
        plugin_run_id: CordisDynamicPluginRunId::new("run-1"),
        waiting_for: None,
    };
    assert!(
        host.resolve_request_run(
            seekdeep_cordis_dynamic_types::ApprovalRequestId::new("request-1"),
            resolution.clone(),
        )
        .await
        .unwrap()
        .accepted
    );
    assert!(matches!(
        host.settle_user_run(plan.agent_id.clone(), plan.plugin_id.clone(), resolution,)
            .await
            .unwrap(),
        seekdeep_cordis_dynamic_types::DynamicCordisRunResponse::Success { .. }
    ));

    let inspect = WasmClientInspectHost::new(namespace.clone().into());
    inspect.sync(Vec::new()).await.unwrap();
    inspect
        .resolve(
            SessionId::new("session-1"),
            seekdeep_cordis_dynamic_types::CordisInspectRequestId::new("inspect-1"),
            seekdeep_cordis_dynamic_types::CordisInspectQueryResolution::Success {
                data: serde_json::json!({"ok": 1}),
            },
        )
        .await
        .unwrap();

    let invoke = wasm_host_invoke(namespace.clone().into());
    let success = await_promise(
        invoke
            .call4(
                &JsValue::UNDEFINED,
                &JsValue::from_str("dyn-1"),
                &JsValue::from_str("run-1"),
                &JsValue::from_str("ping"),
                &serde_wasm_bindgen::to_value(&serde_json::json!({"a": 1})).unwrap(),
            )
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(success.as_string().as_deref(), Some("pong"));

    let set_invoke: Function = field(&bench, "setInvoke");
    for (code, expected) in [
        ("plugin-not-running", "found no active Host half"),
        ("stale-run", "activation that has already been replaced"),
        ("method-not-found", "must declare it with harness.handle"),
        ("handler-error", "failed inside the host handler"),
    ] {
        let failure = Function::new_with_args(
            "code",
            "return { ok: false, code, message: 'boom', stack: 'host-stack' };",
        )
        .call1(&JsValue::UNDEFINED, &JsValue::from_str(code))
        .unwrap();
        set_invoke.call1(&bench, &failure).unwrap();
        let error = await_promise(
            invoke
                .call4(
                    &JsValue::UNDEFINED,
                    &JsValue::from_str("dyn-1"),
                    &JsValue::from_str("run-1"),
                    &JsValue::from_str("ping"),
                    &JsValue::NULL,
                )
                .unwrap(),
        )
        .await
        .unwrap_err();
        assert!(error_message(&error).contains(expected));
        let stack = Reflect::get(&error, &JsValue::from_str("stack"))
            .unwrap()
            .as_string()
            .unwrap();
        assert!(stack.contains("Host stack:\nhost-stack"));
    }

    let reject_invoke: Function = field(&bench, "rejectInvoke");
    reject_invoke
        .call1(&bench, &JsValue::from_str("stream gone"))
        .unwrap();
    let wire = await_promise(
        invoke
            .call4(
                &JsValue::UNDEFINED,
                &JsValue::from_str("dyn-1"),
                &JsValue::from_str("run-1"),
                &JsValue::from_str("ping"),
                &JsValue::NULL,
            )
            .unwrap(),
    )
    .await
    .unwrap_err();
    assert!(error_message(&wire).contains("did not complete: stream gone"));
    assert!(error_message(&wire).contains("Both directions carry JSON only"));

    let render = wasm_render_reporter(namespace.clone().into());
    render(
        SessionId::new("session-1"),
        CordisDynamicPluginId::new("dyn-1"),
        CordisDynamicPluginRunId::new("run-1"),
        DynamicCordisRenderFailure {
            slot: "root".to_owned(),
            message: "boom".to_owned(),
            stack: None,
            abdicated: true,
        },
    );
    let guard = wasm_guard_reporter(namespace.into());
    guard
        .call4(
            &JsValue::UNDEFINED,
            &JsValue::from_str("session-1"),
            &JsValue::from_str("dyn-1"),
            &JsValue::from_str("run-1"),
            &serde_wasm_bindgen::to_value(&serde_json::json!({"message": "guarded"})).unwrap(),
        )
        .unwrap();
    await_promise(
        Function::new_no_args("return new Promise(resolve => setTimeout(resolve, 0));")
            .call0(&JsValue::UNDEFINED)
            .unwrap(),
    )
    .await
    .unwrap();
    let reports: js_sys::Array = field(&bench, "reports");
    assert_eq!(reports.length(), 2);
}

#[wasm_bindgen_test]
#[expect(
    clippy::too_many_lines,
    reason = "One browser scenario keeps setup, assertions, and teardown together"
)]
async fn final_wasm_client_plugin_composes_face_events_remote_and_teardown() {
    let browser = browser_bench();
    let remote_state = remote_bench();
    let namespace: Object = field(&remote_state, "namespace");
    let install_remote: Function = field(&browser, "installRemote");
    let remote = install_remote.call1(&browser, &namespace).unwrap();
    let set_client_code: Function = field(&remote_state, "setClientCode");
    set_client_code
        .call1(
            &remote_state,
            &JsValue::from_str(
                "return { apply() { globalThis.__pluginHostCall = host.call('ping') } }",
            ),
        )
        .unwrap();

    let descriptor = client_plugin_descriptor(Object::new().into()).unwrap();
    assert_eq!(
        Reflect::get(&descriptor, &JsValue::from_str("name"))
            .unwrap()
            .as_string()
            .as_deref(),
        Some("cordis-client-runner")
    );
    let inject = Reflect::get(&descriptor, &JsValue::from_str("inject"))
        .unwrap()
        .dyn_into::<js_sys::Array>()
        .unwrap();
    assert_eq!(inject.length(), 5);
    let apply: Function = field(&descriptor, "apply");
    let ctx: Object = field(&browser, "ctx");
    apply.call1(&descriptor, &ctx).unwrap();
    await_promise(
        Function::new_no_args("return Promise.resolve();")
            .call0(&JsValue::UNDEFINED)
            .unwrap(),
    )
    .await
    .unwrap();

    let get_service: Function = field(&browser, "getService");
    let face = get_service
        .call1(&browser, &JsValue::from_str("dynamicCordisRunner"))
        .unwrap();
    assert!(face.is_object());
    let get_snapshot: Function = field(&face, "getSnapshot");
    let empty = get_snapshot.call0(&face).unwrap();
    assert!(Object::is(&empty, &get_snapshot.call0(&face).unwrap()));
    let active: Object = field(&face, "activeRuns");
    let active_snapshot: Function = field(&active, "getSnapshot");
    let no_activity = active_snapshot.call0(&active).unwrap();
    assert!(Object::is(
        &no_activity,
        &active_snapshot.call0(&active).unwrap()
    ));

    let changes = Object::new();
    Reflect::set(
        &changes,
        &JsValue::from_str("count"),
        &JsValue::from_f64(0.0),
    )
    .unwrap();
    let listener = Function::new_with_args("state", "return () => { state.count += 1; };")
        .call1(&JsValue::UNDEFINED, &changes)
        .unwrap();
    let subscribe: Function = field(&face, "subscribe");
    let unsubscribe = subscribe
        .call1(&face, &listener)
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();

    let start: Function = field(&face, "startUserRun");
    let user_run = CordisUserRunRequest {
        agent_id: SessionId::new("session-1"),
        plugin_id: CordisDynamicPluginId::new("dyn-1"),
        package_id: CordisDynamicPackageId::new("pkg-1"),
        mode: seekdeep_cordis_dynamic_types::DynamicCordisRunMode::Run,
        has_client_half: true,
    };
    await_promise(
        start
            .call1(&face, &serde_wasm_bindgen::to_value(&user_run).unwrap())
            .unwrap(),
    )
    .await
    .unwrap();
    let host_call =
        Reflect::get(&js_sys::global(), &JsValue::from_str("__pluginHostCall")).unwrap();
    assert_eq!(
        await_promise(host_call)
            .await
            .unwrap()
            .as_string()
            .as_deref(),
        Some("pong")
    );
    let is_loaded: Function = field(&face, "isLoaded");
    assert_eq!(
        is_loaded
            .call1(&face, &JsValue::from_str("dyn-1"))
            .unwrap()
            .as_bool(),
        Some(true)
    );
    let loaded = get_snapshot.call0(&face).unwrap();
    assert_eq!(loaded.dyn_ref::<js_sys::Array>().unwrap().length(), 1);
    assert!(
        Reflect::get(&changes, &JsValue::from_str("count"))
            .unwrap()
            .as_f64()
            .unwrap()
            > 0.0
    );

    let dispatch: Function = field(&remote, "$dispatch");
    let retract =
        serde_wasm_bindgen::to_value(&seekdeep_cordis_dynamic_types::DynamicCordisRetracted {
            plugin_id: CordisDynamicPluginId::new("dyn-1"),
            package_id: CordisDynamicPackageId::new("pkg-1"),
            plugin_run_id: CordisDynamicPluginRunId::new("run-1"),
        })
        .unwrap();
    let args = js_sys::Array::new();
    args.push(&retract);
    dispatch
        .call2(&remote, &JsValue::from_str("cordis/dynamic-retract"), &args)
        .unwrap();
    await_promise(
        Function::new_no_args("return new Promise(resolve => setTimeout(resolve, 0));")
            .call0(&JsValue::UNDEFINED)
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        is_loaded
            .call1(&face, &JsValue::from_str("dyn-1"))
            .unwrap()
            .as_bool(),
        Some(false)
    );

    let approval = seekdeep_cordis_dynamic_types::DynamicCordisRunRequest {
        request_id: seekdeep_cordis_dynamic_types::ApprovalRequestId::new("approval-1"),
        agent_id: SessionId::new("session-1"),
        plugin_id: CordisDynamicPluginId::new("dyn-1"),
        package_id: CordisDynamicPackageId::new("pkg-1"),
        mode: seekdeep_cordis_dynamic_types::DynamicCordisRunMode::Run,
        name: "demo".to_owned(),
        purpose: "show a clock".to_owned(),
        requires_approval: true,
    };
    let args = js_sys::Array::new();
    args.push(&serde_wasm_bindgen::to_value(&approval).unwrap());
    dispatch
        .call2(&remote, &JsValue::from_str("cordis/request-run"), &args)
        .unwrap();
    let activity = active_snapshot.call0(&active).unwrap();
    let activity = activity.dyn_into::<js_sys::Map>().unwrap();
    let activity_debug = Function::new_with_args(
        "activity",
        "return JSON.stringify([...activity.entries()]);",
    )
    .call1(&JsValue::UNDEFINED, &activity)
    .unwrap()
    .as_string()
    .unwrap();
    assert_eq!(
        activity.size(),
        1,
        "activity after request: {activity_debug}"
    );
    let row = activity.get(&JsValue::from_str("dyn-1"));
    assert_eq!(
        Reflect::get(&row, &JsValue::from_str("phase"))
            .unwrap()
            .as_string()
            .as_deref(),
        Some("awaiting-approval"),
        "activity after request: {activity_debug}"
    );
    let approve: Function = field(&face, "approve");
    await_promise(
        approve
            .call2(&face, &JsValue::from_str("approval-1"), &JsValue::FALSE)
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        active_snapshot
            .call0(&active)
            .unwrap()
            .dyn_into::<js_sys::Map>()
            .unwrap()
            .size(),
        0
    );
    assert_eq!(
        is_loaded
            .call1(&face, &JsValue::from_str("dyn-1"))
            .unwrap()
            .as_bool(),
        Some(true)
    );

    unsubscribe.call0(&JsValue::UNDEFINED).unwrap();
    let dispose_all: Function = field(&ctx, "disposeAll");
    dispose_all.call0(&ctx).unwrap();
    await_promise(
        Function::new_no_args("return new Promise(resolve => setTimeout(resolve, 0));")
            .call0(&JsValue::UNDEFINED)
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        get_snapshot
            .call0(&face)
            .unwrap()
            .dyn_into::<js_sys::Array>()
            .unwrap()
            .length(),
        0
    );
    let watching: Function = field(&browser, "watching");
    assert_eq!(watching.call0(&browser).unwrap().as_bool(), Some(false));

    let calls: js_sys::Array = field(&remote_state, "calls");
    assert!(
        Function::new_with_args(
            "calls",
            "return calls.some(call => call[0] === 'syncInspectManifest')"
        )
        .call1(&JsValue::UNDEFINED, &calls)
        .unwrap()
        .as_bool()
        .unwrap()
    );
}

#[wasm_bindgen_test]
#[expect(
    clippy::too_many_lines,
    reason = "One browser scenario keeps setup, assertions, and teardown together"
)]
async fn live_guard_proxy_enforces_context_verbs_service_declarations_and_slots() {
    let bench = guard_bench();
    let (bare, _) = guarded_context(&bench, &[]);
    let on = Reflect::get(&bare, &JsValue::from_str("on"))
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    assert_eq!(
        on.call2(
            &bare,
            &JsValue::from_str("event"),
            &Function::new_no_args(""),
        )
        .unwrap()
        .as_string()
        .as_deref(),
        Some("on-result")
    );
    let verbs: js_sys::Array = field(&bench, "verbs");
    assert_eq!(
        Reflect::get(&verbs.get(0), &JsValue::from_str("receiver"))
            .unwrap()
            .as_bool(),
        Some(true)
    );
    let timeout = Reflect::get(&bare, &JsValue::from_str("timeout"))
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    assert!(error_message(&timeout.call0(&bare).unwrap_err()).contains("inject: ['timer'"));
    assert!(
        error_message(&Reflect::get(&bare, &JsValue::from_str("slots")).unwrap_err())
            .contains("service \"slots\" is not declared")
    );
    let get = Reflect::get(&bare, &JsValue::from_str("get"))
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    assert!(
        get.call1(&bare, &JsValue::from_str("slots"))
            .unwrap()
            .is_object()
    );
    assert_eq!(
        Function::new_with_args("ctx", "return 'on' in ctx && !('slots' in ctx);")
            .call1(&JsValue::UNDEFINED, &bare)
            .unwrap()
            .as_bool(),
        Some(true)
    );
    let assignment = Function::new_with_args(
        "ctx",
        "'use strict'; try { ctx.hidden = 1; } catch (error) { return error.message; }",
    )
    .call1(&JsValue::UNDEFINED, &bare)
    .unwrap()
    .as_string()
    .unwrap();
    assert!(assignment.contains("dynamic ctx is read-only"));

    let (guarded, ledger) = guarded_context(&bench, &["timer", "slots", "primitive"]);
    let timeout = Reflect::get(&guarded, &JsValue::from_str("timeout"))
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    assert_eq!(
        timeout.call0(&guarded).unwrap().as_string().as_deref(),
        Some("timeout-result")
    );
    assert_eq!(
        Reflect::get(&guarded, &JsValue::from_str("primitive"))
            .unwrap()
            .as_f64(),
        Some(42.0)
    );
    let slots = Reflect::get(&guarded, &JsValue::from_str("slots")).unwrap();
    let register: Function = field(&slots, "register");
    let component = Function::new_no_args("return null");
    for name in ["root", "root", "chain"] {
        let options = Object::new();
        Reflect::set(
            &options,
            &JsValue::from_str("name"),
            &JsValue::from_str(name),
        )
        .unwrap();
        if name == "chain" {
            Reflect::set(
                &options,
                &JsValue::from_str("priority"),
                &JsValue::from_f64(7.0),
            )
            .unwrap();
        }
        register.call2(&slots, &options, &component).unwrap();
    }
    assert_eq!(ledger.length(), 3);
    for (index, priority) in [(0, -1.0), (1, -2.0), (2, 7.0)] {
        assert_eq!(
            Reflect::get(&ledger.get(index), &JsValue::from_str("priority"))
                .unwrap()
                .as_f64(),
            Some(priority)
        );
    }
    assert!(
        error_message(
            &register
                .call2(&slots, &JsValue::NULL, &component)
                .unwrap_err()
        )
        .contains("needs an options object")
    );
    let entries: Function = field(&slots, "entries");
    assert_eq!(
        entries
            .call0(&slots)
            .unwrap()
            .dyn_into::<js_sys::Array>()
            .unwrap()
            .length(),
        3
    );
    let claims: js_sys::Array = field(&bench, "claims");
    assert_eq!(claims.length(), 3);
}

#[wasm_bindgen_test]
async fn live_guard_proxy_denies_context_returns_and_owns_theme_layers() {
    let bench = guard_bench();
    let (guarded, _) = guarded_context(&bench, &["slots", "theme", "safe", "contextValue"]);
    assert!(
        error_message(&Reflect::get(&guarded, &JsValue::from_str("contextValue")).unwrap_err())
            .contains("returned a cordis Context")
    );
    let safe = Reflect::get(&guarded, &JsValue::from_str("safe")).unwrap();
    let method: Function = field(&safe, "method");
    let result = method
        .call1(&safe, &JsValue::from_str("value"))
        .unwrap()
        .dyn_into::<js_sys::Array>()
        .unwrap();
    assert_eq!(result.get(0).as_bool(), Some(true));
    let context_method: Function = field(&safe, "contextMethod");
    assert!(
        error_message(&context_method.call0(&safe).unwrap_err())
            .contains("returned a cordis Context")
    );
    let context_async: Function = field(&safe, "contextAsync");
    let async_error = await_promise(context_async.call0(&safe).unwrap())
        .await
        .unwrap_err();
    assert!(error_message(&async_error).contains("returned a cordis Context"));
    let slots = Reflect::get(&guarded, &JsValue::from_str("slots")).unwrap();
    assert!(
        error_message(&Reflect::get(&slots, &JsValue::from_str("contextValue")).unwrap_err())
            .contains("returned a cordis Context")
    );

    let theme = Reflect::get(&guarded, &JsValue::from_str("theme")).unwrap();
    let override_tokens: Function = field(&theme, "overrideTokens");
    let tokens = Object::new();
    let early = override_tokens
        .call2(&theme, &JsValue::from_str("impostor"), &tokens)
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    let theme_calls: js_sys::Array = field(&bench, "themeCalls");
    assert_eq!(
        Reflect::get(&theme_calls.get(0), &JsValue::from_str("source"))
            .unwrap()
            .as_string()
            .as_deref(),
        Some("dyn-1.pkg-1")
    );
    early.call0(&JsValue::UNDEFINED).unwrap();
    assert_eq!(
        Reflect::get(&theme_calls.get(0), &JsValue::from_str("disposed"))
            .unwrap()
            .as_bool(),
        Some(true)
    );
    assert!(
        error_message(&override_tokens.call1(&theme, &tokens).unwrap_err())
            .contains("takes two arguments")
    );
    let current: Function = field(&theme, "current");
    assert_eq!(
        current.call0(&theme).unwrap().as_string().as_deref(),
        Some("dark")
    );
    let current_async: Function = field(&theme, "currentAsync");
    assert_eq!(
        await_promise(current_async.call0(&theme).unwrap())
            .await
            .unwrap()
            .as_string()
            .as_deref(),
        Some("light")
    );
    let theme_context: Function = field(&theme, "contextAsync");
    assert!(
        error_message(
            &await_promise(theme_context.call0(&theme).unwrap())
                .await
                .unwrap_err()
        )
        .contains("returned a cordis Context")
    );
    let reports: js_sys::Array = field(&bench, "reports");
    assert!(reports.length() >= 6);
    let dispose_all: Function = field(&bench, "disposeAll");
    dispose_all.call0(&bench).unwrap();
}
