//! Final Client Cordis plugin assembly over the Rust/WASM runtime.

use std::{
    any::Any,
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use js_sys::{Array, Function, Map, Object, Promise};
use parking_lot::Mutex;
use seekdeep_cordis_dynamic_types::{
    ApprovalRequestId, CordisDynamicPluginId, CordisInspectQueryRequest,
    CordisInspectQueryResolved, DynamicCordisInventoryRow, DynamicCordisRequestResolved,
    DynamicCordisRetracted, DynamicCordisRunRequest,
};
use serde::Serialize;
use serde_json::{Value, json};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::future_to_promise;

use crate::{
    CLIENT_RUNNER_INJECT, CLIENT_RUNNER_NAME, ClientInspectRegistration, CordisPageFailureReason,
    CordisRunActivity, CordisRunFailure, CordisRunOrchestrator, DynamicCordisClientRuntime,
    DynamicCordisRuntimeRunner, WasmClientInspectHost, WasmClientMicrotaskScheduler,
    WasmClientMountEngine, WasmClientTaskSpawner, WasmCordisRunHost, call_method,
    client_inspect_providers, install_wasm_client_timer, set, to_js_json,
    wasm_client_inspect_sources, wasm_guard_reporter, wasm_host_invoke, wasm_orchestrator_logger,
    wasm_render_reporter,
};

type SnapshotCache = Arc<Mutex<Option<(Arc<dyn Any + Send + Sync>, JsValue)>>>;

struct WasmClientPluginState {
    runtime: Arc<DynamicCordisClientRuntime>,
    providers: Vec<ClientInspectRegistration>,
    subscriptions: Mutex<Vec<Function>>,
    disposed: AtomicBool,
}

impl WasmClientPluginState {
    fn dispose(&self) {
        if self.disposed.swap(true, Ordering::AcqRel) {
            return;
        }
        for subscription in self.subscriptions.lock().drain(..).rev() {
            let _ = subscription.call0(&JsValue::UNDEFINED);
        }
        for provider in self.providers.iter().rev() {
            provider.dispose();
        }
        let runtime = self.runtime.clone();
        wasm_bindgen_futures::spawn_local(async move {
            runtime.dispose().await;
        });
    }
}

/// Builds the Client plugin descriptor consumed by the browser Cordis Loader.
///
/// React must be the page's existing module instance so hooks retain one owner.
///
/// # Errors
///
/// Returns JavaScript construction failures while building the descriptor.
#[wasm_bindgen(js_name = cordisClientRunnerPlugin)]
pub fn client_plugin_descriptor(react: JsValue) -> Result<JsValue, JsValue> {
    let apply = Closure::wrap(Box::new(move |ctx: JsValue| -> Result<(), JsValue> {
        apply_client_plugin(ctx, react.clone())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let plugin = Object::new();
    set(&plugin, "name", &JsValue::from_str(CLIENT_RUNNER_NAME))?;
    let inject = Array::new();
    for service in CLIENT_RUNNER_INJECT {
        inject.push(&JsValue::from_str(service));
    }
    set(&plugin, "inject", &inject.into())?;
    set(&plugin, "apply", &apply.into_js_value())?;
    Ok(plugin.into())
}

/// Applies the complete Client runner into one real browser Cordis Context.
///
/// # Errors
///
/// Returns missing-Service, Remote binding, provider, or Context registration
/// failures. Any effects already registered remain owned by the failing Fiber.
#[wasm_bindgen(js_name = applyCordisClientRunner)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_client_plugin(ctx: JsValue, react: JsValue) -> Result<(), JsValue> {
    install_wasm_client_timer(&ctx)?;
    let remote = required_service(&ctx, "remote")?;
    let namespace = required_service(&ctx, "remote.dynamicCordisRunner")?;
    let loader = required_service(&ctx, "loader")?;
    let modules = required_service(&ctx, "modules")?;
    let slots = required_service(&ctx, "slots")?;

    let inspect = crate::ClientCordisInspectRegistry::new(
        Arc::new(WasmClientInspectHost::new(namespace.clone())),
        Arc::new(WasmClientMicrotaskScheduler),
        Arc::new(WasmClientTaskSpawner),
    );
    let inspect_service = inspect_service(&inspect)?;
    call_method(
        &ctx,
        "provide",
        &[JsValue::from_str("cordisInspect"), inspect_service],
    )?;
    let providers = client_inspect_providers(wasm_client_inspect_sources(ctx.clone()))
        .into_iter()
        .map(|provider| inspect.register(provider))
        .collect::<anyhow::Result<Vec<_>>>()
        .map_err(|error| js_sys::Error::new(&error.to_string()))?;

    let engine = Arc::new(WasmClientMountEngine::new(
        ctx.clone(),
        loader,
        modules,
        &slots,
        react,
        wasm_host_invoke(namespace.clone()),
        wasm_guard_reporter(namespace.clone()),
    )?);
    let runtime = DynamicCordisClientRuntime::new(
        engine,
        Arc::new(WasmClientTaskSpawner),
        wasm_render_reporter(namespace.clone()),
    );
    let orchestrator = CordisRunOrchestrator::new_with_logger(
        Arc::new(DynamicCordisRuntimeRunner::new(runtime.clone())),
        Arc::new(WasmCordisRunHost::new(namespace)),
        Arc::new(WasmClientTaskSpawner),
        wasm_orchestrator_logger(),
    );
    let face = runner_face(&runtime, &orchestrator)?;
    call_method(
        &ctx,
        "provide",
        &[JsValue::from_str("dynamicCordisRunner"), face],
    )?;

    let mut subscriptions = Vec::new();
    subscriptions.push(connection_reset(&ctx, &inspect)?);
    subscriptions.extend(remote_events(&remote, &runtime, &orchestrator, &inspect)?);
    let state = Arc::new(WasmClientPluginState {
        runtime,
        providers,
        subscriptions: Mutex::new(subscriptions),
        disposed: AtomicBool::new(false),
    });
    own_state(&ctx, state)?;
    Ok(())
}

fn required_service(ctx: &JsValue, name: &str) -> Result<JsValue, JsValue> {
    let service = call_method(ctx, "get", &[JsValue::from_str(name)])?;
    if service.is_undefined() {
        Err(js_sys::Error::new(&format!(
            "cordis-client-runner requires Client Service {name:?}"
        ))
        .into())
    } else {
        Ok(service)
    }
}

fn inspect_service(inspect: &Arc<crate::ClientCordisInspectRegistry>) -> Result<JsValue, JsValue> {
    let manifests = {
        let inspect = inspect.clone();
        Closure::wrap(Box::new(move || {
            to_js_json(&inspect.manifests())
                .unwrap_or_else(|error| js_sys::Error::new(&error.to_string()).into())
        }) as Box<dyn FnMut() -> JsValue>)
    };
    let publish = {
        let inspect = inspect.clone();
        Closure::wrap(Box::new(move || inspect.publish()) as Box<dyn FnMut()>)
    };
    let service = Object::new();
    set(&service, "list", &manifests.into_js_value())?;
    set(&service, "publish", &publish.into_js_value())?;
    Ok(service.into())
}

fn runner_face(
    runtime: &Arc<DynamicCordisClientRuntime>,
    orchestrator: &Arc<CordisRunOrchestrator>,
) -> Result<JsValue, JsValue> {
    let live_cache = Arc::new(Mutex::new(None));
    let activity_cache = Arc::new(Mutex::new(None));
    let failure_cache = Arc::new(Mutex::new(None));
    let render_cache = Arc::new(Mutex::new(None));
    let face = Object::new();
    set(
        &face,
        "activeRuns",
        &orchestrator_activity_observable(orchestrator, activity_cache)?,
    )?;
    set(
        &face,
        "lastRunError",
        &orchestrator_failure_observable(orchestrator, failure_cache)?,
    )?;
    set(
        &face,
        "renderFailures",
        &render_failure_observable(runtime, render_cache)?,
    )?;
    set(
        &face,
        "reconcileApprovals",
        &reconcile_function(orchestrator),
    )?;
    set(&face, "approve", &approve_function(orchestrator))?;
    set(&face, "decline", &decline_function(orchestrator))?;
    set(
        &face,
        "startUserRun",
        &start_user_run_function(orchestrator),
    )?;
    set(&face, "subscribe", &runtime_subscribe(runtime))?;
    set(&face, "getSnapshot", &live_snapshot(runtime, live_cache))?;
    set(&face, "isLoaded", &is_loaded(runtime))?;
    Ok(face.into())
}

fn orchestrator_activity_observable(
    orchestrator: &Arc<CordisRunOrchestrator>,
    cache: SnapshotCache,
) -> Result<JsValue, JsValue> {
    let getter = {
        let orchestrator = orchestrator.clone();
        Closure::wrap(Box::new(move || {
            let snapshot = orchestrator.active_runs();
            cached(&cache, snapshot.clone(), || activity_snapshot(&snapshot))
        }) as Box<dyn FnMut() -> JsValue>)
    };
    observable(
        &getter.into_js_value(),
        &orchestrator_subscribe(orchestrator),
    )
}

fn orchestrator_failure_observable(
    orchestrator: &Arc<CordisRunOrchestrator>,
    cache: SnapshotCache,
) -> Result<JsValue, JsValue> {
    let getter = {
        let orchestrator = orchestrator.clone();
        Closure::wrap(Box::new(move || {
            let snapshot = orchestrator.last_run_error();
            cached(&cache, snapshot.clone(), || run_failure_snapshot(&snapshot))
        }) as Box<dyn FnMut() -> JsValue>)
    };
    observable(
        &getter.into_js_value(),
        &orchestrator_subscribe(orchestrator),
    )
}

fn render_failure_observable(
    runtime: &Arc<DynamicCordisClientRuntime>,
    cache: SnapshotCache,
) -> Result<JsValue, JsValue> {
    let getter = {
        let runtime = runtime.clone();
        Closure::wrap(Box::new(move || {
            let snapshot = runtime.render_failures();
            cached(&cache, snapshot.clone(), || serializable_map(&snapshot))
        }) as Box<dyn FnMut() -> JsValue>)
    };
    observable(&getter.into_js_value(), &runtime_subscribe(runtime))
}

fn observable(getter: &JsValue, subscribe: &Function) -> Result<JsValue, JsValue> {
    let value = Object::new();
    set(&value, "getSnapshot", getter)?;
    set(&value, "subscribe", subscribe)?;
    Ok(value.into())
}

fn runtime_subscribe(runtime: &Arc<DynamicCordisClientRuntime>) -> Function {
    let runtime = runtime.clone();
    let subscribe = Closure::wrap(Box::new(move |listener: Function| -> Function {
        let subscription = runtime.subscribe(Arc::new(move || {
            let _ = listener.call0(&JsValue::UNDEFINED);
        }));
        let dispose = Closure::wrap(Box::new(move || subscription.dispose()) as Box<dyn FnMut()>);
        dispose.into_js_value().unchecked_into()
    }) as Box<dyn FnMut(Function) -> Function>);
    subscribe.into_js_value().unchecked_into()
}

fn orchestrator_subscribe(orchestrator: &Arc<CordisRunOrchestrator>) -> Function {
    let orchestrator = orchestrator.clone();
    let subscribe = Closure::wrap(Box::new(move |listener: Function| -> Function {
        let subscription = orchestrator.subscribe(Arc::new(move || {
            let _ = listener.call0(&JsValue::UNDEFINED);
        }));
        let dispose = Closure::wrap(Box::new(move || subscription.dispose()) as Box<dyn FnMut()>);
        dispose.into_js_value().unchecked_into()
    }) as Box<dyn FnMut(Function) -> Function>);
    subscribe.into_js_value().unchecked_into()
}

fn live_snapshot(runtime: &Arc<DynamicCordisClientRuntime>, cache: SnapshotCache) -> Function {
    let runtime = runtime.clone();
    let getter = Closure::wrap(Box::new(move || {
        let snapshot = runtime.snapshot();
        cached(&cache, snapshot.clone(), || {
            let value = snapshot
                .iter()
                .map(|row| {
                    json!({
                        "pluginId": row.plugin_id,
                        "packageId": row.package_id,
                        "pluginRunId": row.plugin_run_id,
                        "name": row.name,
                        "slots": row.slots,
                        "styleCount": row.style_count,
                    })
                })
                .collect::<Vec<_>>();
            to_js_json(&value).unwrap()
        })
    }) as Box<dyn FnMut() -> JsValue>);
    getter.into_js_value().unchecked_into()
}

fn is_loaded(runtime: &Arc<DynamicCordisClientRuntime>) -> Function {
    let runtime = runtime.clone();
    let callback = Closure::wrap(Box::new(move |plugin_id: String| {
        runtime.is_loaded(&CordisDynamicPluginId::new(plugin_id))
    }) as Box<dyn FnMut(String) -> bool>);
    callback.into_js_value().unchecked_into()
}

fn reconcile_function(orchestrator: &Arc<CordisRunOrchestrator>) -> Function {
    let orchestrator = orchestrator.clone();
    let callback = Closure::wrap(Box::new(move |rows: JsValue| -> Result<(), JsValue> {
        let rows = serde_wasm_bindgen::from_value::<Vec<DynamicCordisInventoryRow>>(rows)
            .map_err(|error| js_sys::Error::new(&error.to_string()))?;
        orchestrator.reconcile_approvals(&rows);
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    callback.into_js_value().unchecked_into()
}

fn approve_function(orchestrator: &Arc<CordisRunOrchestrator>) -> Function {
    let orchestrator = orchestrator.clone();
    let callback = Closure::wrap(
        Box::new(move |request_id: String, future: bool| -> Promise {
            let work = orchestrator.approve(&ApprovalRequestId::new(request_id), future);
            future_to_promise(async move {
                work.await;
                Ok(JsValue::UNDEFINED)
            })
        }) as Box<dyn FnMut(String, bool) -> Promise>,
    );
    callback.into_js_value().unchecked_into()
}

fn decline_function(orchestrator: &Arc<CordisRunOrchestrator>) -> Function {
    let orchestrator = orchestrator.clone();
    let callback = Closure::wrap(Box::new(move |request_id: String| -> Promise {
        let orchestrator = orchestrator.clone();
        future_to_promise(async move {
            orchestrator
                .decline(&ApprovalRequestId::new(request_id))
                .await;
            Ok(JsValue::UNDEFINED)
        })
    }) as Box<dyn FnMut(String) -> Promise>);
    callback.into_js_value().unchecked_into()
}

fn start_user_run_function(orchestrator: &Arc<CordisRunOrchestrator>) -> Function {
    let orchestrator = orchestrator.clone();
    let callback = Closure::wrap(
        Box::new(move |request: JsValue| -> Result<Promise, JsValue> {
            let request = serde_wasm_bindgen::from_value(request)
                .map_err(|error| js_sys::Error::new(&error.to_string()))?;
            let work = orchestrator.start_user_run(request);
            Ok(future_to_promise(async move {
                work.await;
                Ok(JsValue::UNDEFINED)
            }))
        }) as Box<dyn FnMut(JsValue) -> Result<Promise, JsValue>>,
    );
    callback.into_js_value().unchecked_into()
}

fn activity_snapshot(snapshot: &BTreeMap<CordisDynamicPluginId, CordisRunActivity>) -> JsValue {
    let output = Map::new();
    for (plugin_id, activity) in snapshot {
        let value = match activity {
            CordisRunActivity::AwaitingApproval {
                request_id,
                agent_id,
                package_id,
                mode,
                name,
                purpose,
            } => json!({
                "phase": "awaiting-approval",
                "requestId": request_id,
                "agentId": agent_id,
                "packageId": package_id,
                "mode": mode,
                "name": name,
                "purpose": purpose,
            }),
            CordisRunActivity::Orchestrating {
                agent_id,
                package_id,
                mode,
            } => json!({
                "phase": "orchestrating",
                "agentId": agent_id,
                "packageId": package_id,
                "mode": mode,
            }),
        };
        output.set(
            &JsValue::from_str(plugin_id.as_str()),
            &to_js_json(&value).unwrap(),
        );
    }
    output.into()
}

fn run_failure_snapshot(snapshot: &BTreeMap<CordisDynamicPluginId, CordisRunFailure>) -> JsValue {
    let output = Map::new();
    for (plugin_id, failure) in snapshot {
        let reason = match failure.reason {
            CordisPageFailureReason::HostHalfFailed => "host-half-failed",
            CordisPageFailureReason::ClientHalfFailed => "client-half-failed",
        };
        let mut value = json!({
            "packageId": failure.package_id,
            "reason": reason,
            "message": failure.message,
        });
        if let Some(stack) = &failure.stack {
            value
                .as_object_mut()
                .unwrap()
                .insert("stack".to_owned(), Value::String(stack.clone()));
        }
        output.set(
            &JsValue::from_str(plugin_id.as_str()),
            &to_js_json(&value).unwrap(),
        );
    }
    output.into()
}

fn serializable_map<T: Serialize>(snapshot: &BTreeMap<CordisDynamicPluginId, T>) -> JsValue {
    let output = Map::new();
    for (plugin_id, value) in snapshot {
        output.set(
            &JsValue::from_str(plugin_id.as_str()),
            &to_js_json(value).unwrap(),
        );
    }
    output.into()
}

fn cached<T>(cache: &SnapshotCache, snapshot: Arc<T>, build: impl FnOnce() -> JsValue) -> JsValue
where
    T: Any + Send + Sync,
{
    let snapshot: Arc<dyn Any + Send + Sync> = snapshot;
    let mut cache = cache.lock();
    if let Some((current, value)) = &*cache
        && Arc::ptr_eq(current, &snapshot)
    {
        return value.clone();
    }
    let value = build();
    *cache = Some((snapshot, value.clone()));
    value
}

fn connection_reset(
    ctx: &JsValue,
    inspect: &Arc<crate::ClientCordisInspectRegistry>,
) -> Result<Function, JsValue> {
    let inspect = inspect.clone();
    let listener = Closure::wrap(Box::new(move || inspect.publish()) as Box<dyn FnMut()>);
    call_method(
        ctx,
        "on",
        &[
            JsValue::from_str("connection/reset"),
            listener.into_js_value(),
        ],
    )?
    .dyn_into::<Function>()
}

fn remote_events(
    remote: &JsValue,
    runtime: &Arc<DynamicCordisClientRuntime>,
    orchestrator: &Arc<CordisRunOrchestrator>,
    inspect: &Arc<crate::ClientCordisInspectRegistry>,
) -> Result<Vec<Function>, JsValue> {
    Ok(vec![
        on_request_run(remote, orchestrator)?,
        on_request_resolved(remote, orchestrator)?,
        on_retract(remote, runtime)?,
        on_inspect_query(remote, inspect)?,
        on_inspect_resolved(remote, inspect)?,
    ])
}

fn subscribe_remote(remote: &JsValue, event: &str, listener: JsValue) -> Result<Function, JsValue> {
    call_method(remote, "$on", &[JsValue::from_str(event), listener])?.dyn_into::<Function>()
}

fn on_request_run(
    remote: &JsValue,
    orchestrator: &Arc<CordisRunOrchestrator>,
) -> Result<Function, JsValue> {
    let orchestrator = orchestrator.clone();
    let listener = Closure::wrap(Box::new(move |request: JsValue| {
        match serde_wasm_bindgen::from_value::<DynamicCordisRunRequest>(request) {
            Ok(request) => orchestrator.open(crate::CordisRunRequest {
                request_id: request.request_id,
                agent_id: request.agent_id,
                plugin_id: request.plugin_id,
                package_id: request.package_id,
                mode: request.mode,
                name: request.name,
                purpose: request.purpose,
                requires_approval: request.requires_approval,
            }),
            Err(error) => log_event_error("cordis/request-run", &error.to_string()),
        }
    }) as Box<dyn FnMut(JsValue)>);
    subscribe_remote(remote, "cordis/request-run", listener.into_js_value())
}

fn on_request_resolved(
    remote: &JsValue,
    orchestrator: &Arc<CordisRunOrchestrator>,
) -> Result<Function, JsValue> {
    let orchestrator = orchestrator.clone();
    let listener = Closure::wrap(Box::new(move |resolved: JsValue| {
        match serde_wasm_bindgen::from_value::<DynamicCordisRequestResolved>(resolved) {
            Ok(resolved) => orchestrator.close(&resolved.request_id),
            Err(error) => log_event_error("cordis/request-run-resolved", &error.to_string()),
        }
    }) as Box<dyn FnMut(JsValue)>);
    subscribe_remote(
        remote,
        "cordis/request-run-resolved",
        listener.into_js_value(),
    )
}

fn on_retract(
    remote: &JsValue,
    runtime: &Arc<DynamicCordisClientRuntime>,
) -> Result<Function, JsValue> {
    let runtime = runtime.clone();
    let listener = Closure::wrap(Box::new(move |retracted: JsValue| {
        match serde_wasm_bindgen::from_value::<DynamicCordisRetracted>(retracted) {
            Ok(retracted) => runtime.retract(retracted.plugin_id, retracted.plugin_run_id),
            Err(error) => log_event_error("cordis/dynamic-retract", &error.to_string()),
        }
    }) as Box<dyn FnMut(JsValue)>);
    subscribe_remote(remote, "cordis/dynamic-retract", listener.into_js_value())
}

fn on_inspect_query(
    remote: &JsValue,
    inspect: &Arc<crate::ClientCordisInspectRegistry>,
) -> Result<Function, JsValue> {
    let inspect = inspect.clone();
    let listener = Closure::wrap(Box::new(move |request: JsValue| {
        match serde_wasm_bindgen::from_value::<CordisInspectQueryRequest>(request) {
            Ok(request) => {
                let inspect = inspect.clone();
                wasm_bindgen_futures::spawn_local(async move { inspect.query(request).await });
            }
            Err(error) => log_event_error("cordis/inspect-query", &error.to_string()),
        }
    }) as Box<dyn FnMut(JsValue)>);
    subscribe_remote(remote, "cordis/inspect-query", listener.into_js_value())
}

fn on_inspect_resolved(
    remote: &JsValue,
    inspect: &Arc<crate::ClientCordisInspectRegistry>,
) -> Result<Function, JsValue> {
    let inspect = inspect.clone();
    let listener = Closure::wrap(Box::new(move |resolved: JsValue| {
        match serde_wasm_bindgen::from_value::<CordisInspectQueryResolved>(resolved) {
            Ok(resolved) => inspect.close(&resolved.request_id),
            Err(error) => log_event_error("cordis/inspect-query-resolved", &error.to_string()),
        }
    }) as Box<dyn FnMut(JsValue)>);
    subscribe_remote(
        remote,
        "cordis/inspect-query-resolved",
        listener.into_js_value(),
    )
}

fn log_event_error(event: &str, message: &str) {
    web_sys::console::error_2(
        &JsValue::from_str(&format!(
            "[cordis-client-runner] forwarded event {event} was malformed:"
        )),
        &JsValue::from_str(message),
    );
}

fn own_state(ctx: &JsValue, state: Arc<WasmClientPluginState>) -> Result<(), JsValue> {
    let installer = Closure::once_into_js(move || -> JsValue {
        let disposer = Closure::wrap(Box::new(move || state.dispose()) as Box<dyn FnMut()>);
        disposer.into_js_value()
    });
    call_method(
        ctx,
        "effect",
        &[
            installer,
            JsValue::from_str("cordis-client-runner: dynamic package runner"),
        ],
    )?;
    Ok(())
}
