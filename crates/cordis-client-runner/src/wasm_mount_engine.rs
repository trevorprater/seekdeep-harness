//! Browser Cordis/Loader adapter for the Rust-owned dynamic Client runtime.

use std::{collections::BTreeMap, sync::Arc};

use futures::{FutureExt, channel::oneshot, future::BoxFuture};
use js_sys::{Array, Function, Object, Promise, Reflect, WeakMap};
use parking_lot::Mutex;
use seekdeep_cordis_dynamic_types::{
    CordisDynamicPluginId, CordisDynamicPluginRunId, DynamicCordisPackage,
    DynamicCordisRenderFailure,
};
use seekdeep_identity::SessionId;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};

use crate::{
    ClientLoadErrorCause, ClientLoadRequest, ClientMountEngine, ClientMountError,
    ClientMountFailure, ClientMountRejection, ClientPriorityAllocator, ClientRenderCrashListener,
    DynamicCordisStyles, MountedClientPackage, WasmClientGuardPolicy, WasmStyleDom,
    begin_evaluate_client_half, classify_evaluated_client_value, create_client_context,
};

const MODULE_LOADER_MISSING: &str =
    "cordis-client-runner: window.__ModuleLoader__ is missing (booted outside the web shell?)";
const MODULE_IMPORT_FAILED: &str = "module import failed (see the browser console)";

#[derive(Clone)]
struct WasmMountedRecord {
    plugin_run_id: CordisDynamicPluginRunId,
    entry_id: String,
    styles: Arc<DynamicCordisStyles>,
}

/// Actual browser adapter: Rust owns sequencing and policy while JavaScript
/// objects remain compatibility bindings for the page Cordis services.
pub struct WasmClientMountEngine {
    ctx: JsValue,
    loader: JsValue,
    modules: JsValue,
    react: JsValue,
    invoke: Function,
    report_guard_failure: Function,
    priorities: Arc<ClientPriorityAllocator>,
    owners: WeakMap<Object, JsValue>,
    records: Arc<Mutex<BTreeMap<CordisDynamicPluginId, WasmMountedRecord>>>,
    crash_listener: Arc<Mutex<Option<ClientRenderCrashListener>>>,
    unwatch: Arc<Mutex<Option<Function>>>,
}

impl std::fmt::Debug for WasmClientMountEngine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WasmClientMountEngine")
            .field("records", &self.records.lock().len())
            .field("watching", &self.unwatch.lock().is_some())
            .finish_non_exhaustive()
    }
}

impl WasmClientMountEngine {
    /// Connects to the real page Context, Loader, module table, and Slot registry.
    ///
    /// `invoke` receives `(pluginId, pluginRunId, method, args)`. The guard
    /// reporter receives `(agentId, pluginId, pluginRunId, errorDetails)`.
    ///
    /// # Errors
    ///
    /// Returns a JavaScript error when Slot crash supervision cannot be installed.
    pub fn new(
        ctx: JsValue,
        loader: JsValue,
        modules: JsValue,
        slots: &JsValue,
        react: JsValue,
        invoke: Function,
        report_guard_failure: Function,
    ) -> Result<Self, JsValue> {
        let owners: WeakMap<Object, JsValue> = WeakMap::new_typed();
        let crash_listener = Arc::new(Mutex::new(None));
        let on_crash = slot_crash_callback(owners.clone(), crash_listener.clone());
        let unwatch = call_method(slots, "onEntryError", &[on_crash])?
            .dyn_into::<Function>()
            .map_err(|value| {
                js_sys::Error::new(&format!(
                    "cordis-client-runner: slots.onEntryError returned {value:?}, expected a disposer"
                ))
            })?;
        Ok(Self {
            ctx,
            loader,
            modules,
            react,
            invoke,
            report_guard_failure,
            priorities: Arc::new(ClientPriorityAllocator::default()),
            owners,
            records: Arc::new(Mutex::new(BTreeMap::new())),
            crash_listener,
            unwatch: Arc::new(Mutex::new(Some(unwatch))),
        })
    }

    #[allow(clippy::too_many_lines)]
    async fn mount_inner(
        &self,
        request: ClientLoadRequest,
    ) -> Result<MountedClientPackage, ClientMountError> {
        let styles = Arc::new(DynamicCordisStyles::new(
            request.plugin_id.clone(),
            Arc::new(WasmStyleDom),
        ));
        let promise = match (|| {
            let styles_binding = styles_binding(styles.clone())?;
            let invoke = scoped_invoke(&self.invoke, &request.plugin_id, &request.plugin_run_id)?;
            let note_error = note_error_callback(&request.plugin_id)?;
            begin_evaluate_client_half(
                request.plugin_id.as_str(),
                &request.code,
                &self.react,
                &styles_binding,
                &invoke,
                &note_error,
            )
        })() {
            Ok(promise) => promise,
            Err(error) => {
                styles.dispose();
                return Err(classified(ClientLoadErrorCause::Evaluate, &error));
            }
        };
        let plugin = match promise_result(&promise).await {
            Ok(value) => match classify_evaluated_client_value(value) {
                Ok(plugin) => plugin,
                Err(error) => {
                    styles.dispose();
                    return Err(classified(ClientLoadErrorCause::Evaluate, &error));
                }
            },
            Err(error) => {
                styles.dispose();
                return Err(classified(ClientLoadErrorCause::Evaluate, &error));
            }
        };

        let package = DynamicCordisPackage {
            plugin_id: request.plugin_id.clone(),
            package_id: request.package_id.clone(),
            plugin_run_id: request.plugin_run_id.clone(),
            name: request.name.clone(),
        };
        let ledger = Array::new();
        let surface = self
            .guarded_surface(&package, &request.agent_id, &plugin, &ledger)
            .map_err(|error| rejected(&error))?;
        let module_id = module_id(&request.plugin_id);
        self.invalidate(&module_id)
            .map_err(|error| rejected(&error))?;
        register_module(&module_id, &surface).map_err(|error| rejected(&error))?;

        let options = Object::new();
        set(&options, "name", &JsValue::from_str(&module_id)).map_err(|error| rejected(&error))?;
        let creation = Promise::resolve(
            &call_method(&self.loader, "create", &[options.into()])
                .map_err(|error| rejected(&error))?,
        );
        let entry_id = promise_result(&creation)
            .await
            .map_err(|error| rejected(&error))?
            .as_string()
            .ok_or_else(|| {
                let error: JsValue = js_sys::Error::new(
                    "cordis-client-runner: loader.create returned a non-string entry id",
                )
                .into();
                rejected(&error)
            })?;

        let resolved = call_method(&self.loader, "resolve", &[JsValue::from_str(&entry_id)])
            .map_err(|error| rejected(&error))?;
        let fiber = Reflect::get(&resolved, &JsValue::from_str("fiber"))
            .map_err(|error| rejected(&error))?;
        if fiber.is_undefined() {
            self.teardown_parts(&request.plugin_id, &entry_id, &styles)
                .await
                .map_err(|error| {
                    let error: JsValue = js_sys::Error::new(&format!("{error:#}")).into();
                    rejected(&error)
                })?;
            return Err(ClientMountError::Classified(ClientMountFailure {
                cause: ClientLoadErrorCause::ModuleImport,
                message: MODULE_IMPORT_FAILED.to_owned(),
                stack: None,
            }));
        }
        let activation = call_method(&fiber, "await", &[]).map_err(|error| rejected(&error))?;
        let activation = Promise::resolve(&activation);
        if let Err(error) = promise_result(&activation).await {
            self.teardown_parts(&request.plugin_id, &entry_id, &styles)
                .await
                .map_err(|cleanup| {
                    let error: JsValue = js_sys::Error::new(&format!("{cleanup:#}")).into();
                    rejected(&error)
                })?;
            return Err(classified(ClientLoadErrorCause::Activate, &error));
        }

        let waiting_for = missing_services(&self.ctx, &fiber).map_err(|error| rejected(&error))?;
        let slots = ledger_slots(&ledger).map_err(|error| rejected(&error))?;
        let style_count = styles.count();
        self.records.lock().insert(
            request.plugin_id,
            WasmMountedRecord {
                plugin_run_id: request.plugin_run_id,
                entry_id,
                styles,
            },
        );
        Ok(MountedClientPackage {
            waiting_for,
            slots,
            style_count,
        })
    }

    fn guarded_surface(
        &self,
        package: &DynamicCordisPackage,
        agent_id: &SessionId,
        plugin: &JsValue,
        ledger: &Array,
    ) -> Result<JsValue, JsValue> {
        let owner = Object::new();
        set(&owner, "agentId", &JsValue::from_str(agent_id.as_str()))?;
        set(
            &owner,
            "pluginId",
            &JsValue::from_str(package.plugin_id.as_str()),
        )?;
        set(
            &owner,
            "pluginRunId",
            &JsValue::from_str(package.plugin_run_id.as_str()),
        )?;
        let claim = claim_callback(self.owners.clone(), owner.into());
        let report_failure = guard_failure_callback(
            self.report_guard_failure.clone(),
            agent_id.clone(),
            package.plugin_id.clone(),
            package.plugin_run_id.clone(),
        );
        let priorities = self.priorities.clone();
        let package = package.clone();
        let guarded_module_id = module_id(&package.plugin_id);
        let guard_package = package.clone();
        let ledger_for_guard = ledger.clone();
        let claim_for_guard = claim.clone();
        let report_for_guard = report_failure.clone();
        let guard = Closure::wrap(Box::new(move |ctx: JsValue| {
            let declared = declared_services(&ctx)?;
            let is_context = is_context_callback(&ctx)?;
            create_client_context(
                ctx,
                WasmClientGuardPolicy::with_priorities(
                    guard_package.clone(),
                    declared,
                    priorities.clone(),
                ),
                ledger_for_guard.clone(),
                claim_for_guard.clone(),
                report_for_guard.clone(),
                is_context,
            )
        })
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
        let factory = construct_function(
            &["plugin", "name", "guard", "functionForm"],
            r"
if (functionForm) {
  return { name, apply(ctx) { return plugin(guard(ctx)); } };
}
return {
  ...plugin,
  name,
  apply(ctx, config) { return plugin.apply(guard(ctx), config); },
};
",
        )?;
        let arguments = Array::new();
        arguments.push(plugin);
        arguments.push(&JsValue::from_str(&guarded_module_id));
        arguments.push(&guard.into_js_value());
        arguments.push(&JsValue::from_bool(plugin.is_function()));
        factory.apply(&JsValue::UNDEFINED, &arguments)
    }

    fn invalidate(&self, module_id: &str) -> Result<(), JsValue> {
        call_method(&self.modules, "invalidate", &[JsValue::from_str(module_id)])?;
        Ok(())
    }

    async fn teardown_parts(
        &self,
        plugin_id: &CordisDynamicPluginId,
        entry_id: &str,
        styles: &DynamicCordisStyles,
    ) -> anyhow::Result<()> {
        let removal = call_method(&self.loader, "remove", &[JsValue::from_str(entry_id)])
            .map_err(|error| js_anyhow(&error))?;
        let removal = Promise::resolve(&removal);
        promise_result(&removal)
            .await
            .map_err(|error| js_anyhow(&error))?;
        self.invalidate(&module_id(plugin_id))
            .map_err(|error| js_anyhow(&error))?;
        styles.dispose();
        Ok(())
    }
}

impl ClientMountEngine for WasmClientMountEngine {
    fn watch(&self, listener: ClientRenderCrashListener) {
        *self.crash_listener.lock() = Some(listener);
    }

    fn mount(
        &self,
        request: ClientLoadRequest,
    ) -> BoxFuture<'static, Result<MountedClientPackage, ClientMountError>> {
        let engine = self.clone_for_future();
        async move { engine.mount_inner(request).await }.boxed()
    }

    fn teardown(
        &self,
        plugin_id: CordisDynamicPluginId,
        plugin_run_id: CordisDynamicPluginRunId,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        let engine = self.clone_for_future();
        async move {
            let record = {
                let mut records = engine.records.lock();
                let Some(record) = records.get(&plugin_id) else {
                    return Ok(());
                };
                if record.plugin_run_id != plugin_run_id {
                    return Ok(());
                }
                records.remove(&plugin_id).expect("record existed")
            };
            engine
                .teardown_parts(&plugin_id, &record.entry_id, &record.styles)
                .await
        }
        .boxed()
    }

    fn unwatch(&self) {
        self.crash_listener.lock().take();
        if let Some(unwatch) = self.unwatch.lock().take() {
            let _ = unwatch.call0(&JsValue::UNDEFINED);
        }
    }
}

impl WasmClientMountEngine {
    fn clone_for_future(&self) -> Self {
        Self {
            ctx: self.ctx.clone(),
            loader: self.loader.clone(),
            modules: self.modules.clone(),
            react: self.react.clone(),
            invoke: self.invoke.clone(),
            report_guard_failure: self.report_guard_failure.clone(),
            priorities: self.priorities.clone(),
            owners: self.owners.clone(),
            records: self.records.clone(),
            crash_listener: self.crash_listener.clone(),
            unwatch: self.unwatch.clone(),
        }
    }
}

fn module_id(plugin_id: &CordisDynamicPluginId) -> String {
    format!("dyn/{plugin_id}")
}

fn classified(cause: ClientLoadErrorCause, error: &JsValue) -> ClientMountError {
    let (message, stack) = error_details(error);
    ClientMountError::Classified(ClientMountFailure {
        cause,
        message,
        stack,
    })
}

fn rejected(error: &JsValue) -> ClientMountError {
    let (message, stack) = error_details(error);
    ClientMountError::Rejected(ClientMountRejection { message, stack })
}

pub(crate) fn error_details(error: &JsValue) -> (String, Option<String>) {
    if !error.is_object() {
        return (js_string(error), None);
    }
    let message = Reflect::get(error, &JsValue::from_str("message"))
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_else(|| js_string(error));
    let stack = Reflect::get(error, &JsValue::from_str("stack"))
        .ok()
        .and_then(|value| value.as_string());
    (message, stack)
}

fn js_string(value: &JsValue) -> String {
    js_sys::JsString::from(value.clone())
        .as_string()
        .unwrap_or_else(|| format!("{value:?}"))
}

pub(crate) fn js_anyhow(error: &JsValue) -> anyhow::Error {
    let (message, _) = error_details(error);
    anyhow::anyhow!(message)
}

pub(crate) fn call_method(
    target: &JsValue,
    name: &str,
    arguments: &[JsValue],
) -> Result<JsValue, JsValue> {
    let method = Reflect::get(target, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    Reflect::apply(&method, target, &args)
}

pub(crate) fn set(target: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(target, &JsValue::from_str(key), value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new(&format!("could not assign {key:?}")).into())
    }
}

pub(crate) fn construct_function(parameters: &[&str], body: &str) -> Result<Function, JsValue> {
    let constructor =
        Reflect::get(&js_sys::global(), &JsValue::from_str("Function"))?.dyn_into::<Function>()?;
    let arguments = Array::new();
    for parameter in parameters {
        arguments.push(&JsValue::from_str(parameter));
    }
    arguments.push(&JsValue::from_str(body));
    Reflect::construct(&constructor, &arguments)?.dyn_into::<Function>()
}

pub(crate) fn promise_result(promise: &Promise) -> BoxFuture<'static, Result<JsValue, JsValue>> {
    let (sender, receiver) = oneshot::channel();
    let sender = Arc::new(Mutex::new(Some(sender)));
    let resolved = {
        let sender = sender.clone();
        Closure::once_into_js(move |value: JsValue| {
            if let Some(sender) = sender.lock().take() {
                let _ = sender.send(Ok(value));
            }
        })
    };
    let rejected = {
        let sender = sender.clone();
        Closure::once_into_js(move |value: JsValue| {
            if let Some(sender) = sender.lock().take() {
                let _ = sender.send(Err(value));
            }
        })
    };
    let registration = Reflect::get(promise, &JsValue::from_str("then"))
        .and_then(wasm_bindgen::JsCast::dyn_into::<Function>)
        .and_then(|then| then.call2(promise, &resolved, &rejected));
    if let Err(error) = registration
        && let Some(sender) = sender.lock().take()
    {
        let _ = sender.send(Err(error));
    }
    async move {
        receiver.await.unwrap_or_else(|_| {
            Err(js_sys::Error::new("JavaScript Promise settlement channel closed").into())
        })
    }
    .boxed()
}

fn scoped_invoke(
    invoke: &Function,
    plugin_id: &CordisDynamicPluginId,
    plugin_run_id: &CordisDynamicPluginRunId,
) -> Result<Function, JsValue> {
    let factory = construct_function(
        &["invoke", "pluginId", "pluginRunId"],
        "return (method, args = null) => invoke(pluginId, pluginRunId, method, args);",
    )?;
    factory
        .call3(
            &JsValue::UNDEFINED,
            invoke,
            &JsValue::from_str(plugin_id.as_str()),
            &JsValue::from_str(plugin_run_id.as_str()),
        )?
        .dyn_into::<Function>()
}

fn note_error_callback(plugin_id: &CordisDynamicPluginId) -> Result<Function, JsValue> {
    let factory = construct_function(
        &["pluginId"],
        "return message => console.error(`[cordis-client-runner] ${pluginId} logged an error:`, message);",
    )?;
    factory
        .call1(&JsValue::UNDEFINED, &JsValue::from_str(plugin_id.as_str()))?
        .dyn_into::<Function>()
}

fn styles_binding(styles: Arc<DynamicCordisStyles>) -> Result<JsValue, JsValue> {
    let insert_styles = styles.clone();
    let insert = Closure::wrap(Box::new(move |css: JsValue| -> Result<Function, JsValue> {
        let css = css
            .as_string()
            .ok_or_else(|| js_sys::Error::new("styles.insert(css) needs a CSS string"))?;
        let disposer = insert_styles
            .insert(&css)
            .map_err(|error| js_sys::Error::new(&error.to_string()))?;
        let dispose = Closure::wrap(Box::new(move || disposer.dispose()) as Box<dyn FnMut()>);
        Ok(dispose.into_js_value().unchecked_into())
    })
        as Box<dyn FnMut(JsValue) -> Result<Function, JsValue>>);
    let count = Closure::wrap(Box::new(move || styles.count()) as Box<dyn FnMut() -> usize>);
    let factory = construct_function(
        &["insert", "count"],
        "return { insert, get count() { return count(); } };",
    )?;
    factory.call2(
        &JsValue::UNDEFINED,
        &insert.into_js_value(),
        &count.into_js_value(),
    )
}

fn register_module(module_id: &str, surface: &JsValue) -> Result<(), JsValue> {
    let sink = Reflect::get(&js_sys::global(), &JsValue::from_str("__ModuleLoader__"))?;
    if sink.is_undefined() {
        return Err(js_sys::Error::new(MODULE_LOADER_MISSING).into());
    }
    let factory_builder = construct_function(&["surface"], "return () => surface;")?;
    let factory = factory_builder.call1(&JsValue::UNDEFINED, surface)?;
    let handoff = Object::new();
    set(&handoff, "id", &JsValue::from_str(module_id))?;
    set(&handoff, "factory", &factory)?;
    call_method(&sink, "load", &[handoff.into()])?;
    Ok(())
}

fn declared_services(ctx: &JsValue) -> Result<Vec<String>, JsValue> {
    let fiber = Reflect::get(ctx, &JsValue::from_str("fiber"))?;
    let inject = Reflect::get(&fiber, &JsValue::from_str("inject"))?;
    Ok(Object::keys(&Object::from(inject))
        .iter()
        .filter_map(|value| value.as_string())
        .collect())
}

fn missing_services(ctx: &JsValue, fiber: &JsValue) -> Result<Vec<String>, JsValue> {
    let inject = Reflect::get(fiber, &JsValue::from_str("inject"))?;
    let mut missing = Vec::new();
    for name in Object::keys(&Object::from(inject)).iter() {
        let Some(name) = name.as_string() else {
            continue;
        };
        if call_method(ctx, "get", &[JsValue::from_str(&name)])?.is_undefined() {
            missing.push(name);
        }
    }
    Ok(missing)
}

fn ledger_slots(ledger: &Array) -> Result<Vec<String>, JsValue> {
    ledger
        .iter()
        .map(|row| {
            Reflect::get(&row, &JsValue::from_str("slot"))?
                .as_string()
                .ok_or_else(|| {
                    js_sys::Error::new("dynamic Slot ledger row has no string slot").into()
                })
        })
        .collect()
}

fn claim_callback(owners: WeakMap<Object, JsValue>, owner: JsValue) -> Function {
    let claim = Closure::wrap(Box::new(move |component: JsValue| {
        if let Ok(component) = component.dyn_into::<Object>() {
            owners.set(&component, &owner);
        }
    }) as Box<dyn FnMut(JsValue)>);
    claim.into_js_value().unchecked_into()
}

fn guard_failure_callback(
    report: Function,
    agent_id: SessionId,
    plugin_id: CordisDynamicPluginId,
    plugin_run_id: CordisDynamicPluginRunId,
) -> Function {
    let callback = Closure::wrap(Box::new(move |error: JsValue| {
        let (message, stack) = error_details(&error);
        let details = Object::new();
        let _ = set(&details, "message", &JsValue::from_str(&message));
        if let Some(stack) = stack {
            let _ = set(&details, "stack", &JsValue::from_str(&stack));
        }
        let _ = report.call4(
            &JsValue::UNDEFINED,
            &JsValue::from_str(agent_id.as_str()),
            &JsValue::from_str(plugin_id.as_str()),
            &JsValue::from_str(plugin_run_id.as_str()),
            &details,
        );
    }) as Box<dyn FnMut(JsValue)>);
    callback.into_js_value().unchecked_into()
}

fn is_context_callback(ctx: &JsValue) -> Result<Function, JsValue> {
    let factory = construct_function(
        &["ctx"],
        "return value => value instanceof ctx.constructor;",
    )?;
    factory
        .call1(&JsValue::UNDEFINED, ctx)?
        .dyn_into::<Function>()
}

fn slot_crash_callback(
    owners: WeakMap<Object, JsValue>,
    listener: Arc<Mutex<Option<ClientRenderCrashListener>>>,
) -> JsValue {
    let callback = Closure::wrap(Box::new(
        move |slot: JsValue, entry: JsValue, error: JsValue, info: JsValue| {
            let Ok(component) = Reflect::get(&entry, &JsValue::from_str("component")) else {
                return;
            };
            let Ok(component) = component.dyn_into::<Object>() else {
                return;
            };
            let owner = owners.get(&component);
            if owner.is_undefined() {
                return;
            }
            let read = |name: &str| {
                Reflect::get(&owner, &JsValue::from_str(name))
                    .ok()
                    .and_then(|value| value.as_string())
            };
            let (Some(agent_id), Some(plugin_id), Some(plugin_run_id), Some(slot)) = (
                read("agentId"),
                read("pluginId"),
                read("pluginRunId"),
                slot.as_string(),
            ) else {
                return;
            };
            let (message, stack) = error_details(&error);
            let abdicated = Reflect::get(&info, &JsValue::from_str("abdicated"))
                .ok()
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            if let Some(listener) = listener.lock().clone() {
                listener(
                    SessionId::new(agent_id),
                    CordisDynamicPluginId::new(plugin_id),
                    CordisDynamicPluginRunId::new(plugin_run_id),
                    DynamicCordisRenderFailure {
                        slot,
                        message,
                        stack,
                        abdicated,
                    },
                );
            }
        },
    ) as Box<dyn FnMut(JsValue, JsValue, JsValue, JsValue)>);
    callback.into_js_value()
}
