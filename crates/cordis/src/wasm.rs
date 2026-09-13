//! JavaScript object bindings over the Rust-owned browser Cordis core.

use std::{
    cell::{Cell, RefCell},
    sync::Arc,
};

use js_sys::{Array, Function, Object, Promise, Reflect, WeakMap};
use parking_lot::Mutex;
use serde_json::Value;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use crate::{
    BailReply, Context, EventArgs, EventOptions, EventReply, EventValue, FiberState, Plugin,
    PluginFiber, fiber::EffectHandle,
};

mod browser_config;
mod browser_context;
pub(crate) mod browser_effects;
mod browser_errors;
pub(crate) mod browser_events;
mod browser_fiber;
mod browser_fiber_api;
mod browser_inject;
mod browser_logger;
mod browser_plugins;
mod browser_reflect;
mod browser_registry;
mod browser_runner;
mod browser_service;
pub(crate) mod browser_services;
mod browser_stack;
mod browser_symbols;
mod browser_values;
mod context_proxy;
mod test_invariants;
mod tracing;
mod update_hooks;

pub use browser_config::{
    configure_validation_error_prototype, resolve_config, validation_error_message,
};
pub use browser_context::{
    extend as context_extend, intercept as context_intercept, isolate as context_isolate,
};
pub use browser_errors::{
    configure_cordis_error_constructor, cordis_error_codes, cordis_error_message,
};
pub use browser_fiber_api::invoke as fiber_invoke;
pub use browser_fiber_api::{
    configure_fiber_prototype, create_fiber, fiber_assert_active, fiber_check_impl, fiber_effect,
    fiber_get_effects, fiber_name,
};
pub use browser_inject::inject_decorator;
pub use browser_logger::format::{
    code as logger_code, color as logger_color, colors16, colors256, default_formatters,
    format as logger_format,
};
pub use browser_logger::{
    configure_logger_boundary, initialize_logger, initialize_logger_service, logger_config,
    logger_exporter, logger_forward, logger_invoke, logger_method,
};
pub use browser_plugins::is_constructor;
pub use browser_reflect::{
    configure_prototype as configure_reflect_prototype,
    initialize_service as initialize_reflect_service, prototype as reflect_service_prototype,
};
pub use browser_registry::{
    configure_registry_service_prototype, create_registry_service, registry_service_prototype,
    resolve_inject,
};
pub use browser_runner::refresh as fiber_refresh;
pub use browser_runner::resolve_config as fiber_resolve_config;
pub use browser_runner::{
    execute as fiber_execute, get_state as fiber_get_state, update_state as fiber_update_state,
};
pub use browser_service::{
    create_callable, extend_service, initialize_service, resolve_service_config, service_filter,
    service_has_instance,
};
pub use browser_stack::{
    compose_stack_result, configure_stack_builder, handle_stack_error, outer_stack_frames,
    stack_info,
};
pub use browser_symbols::{configure_context_constructor, configure_service_constructor, symbols};
pub use browser_values::{is_object, join_prototype, property_descriptor};
pub use context_proxy::{
    get as context_reflected_get, has as context_has, inspect as context_inspect,
    set as context_reflected_set, special_property as context_special_property,
};
pub use test_invariants::{
    companion_paths as browser_test_invariant_companion_paths,
    create_attachment_store as create_test_invariant_attachment_store,
    install as install_test_invariant_host, readiness_service as test_invariant_readiness_service,
    uses_manual_tree as browser_uses_manual_invariant_tree,
};
pub use tracing::{get_traceable, with_props};
pub use update_hooks::{
    configure_disposable_list_prototype, create_disposable_list, disposable_list_prototype,
};

thread_local! {
    static CONTEXT_WRAPPER: RefCell<Option<Function>> = const { RefCell::new(None) };
    static CONTEXT_CORES: WeakMap = WeakMap::new();
}

/// Creates the mutable numeric lifecycle enum, including number-to-name entries.
///
/// # Errors
/// Propagates property construction failures.
#[wasm_bindgen(js_name = fiberStates)]
pub fn fiber_states() -> Result<Object, JsValue> {
    let states = Object::new();
    for (name, state) in [
        ("PENDING", FiberState::Pending),
        ("LOADING", FiberState::Loading),
        ("ACTIVE", FiberState::Active),
        ("FAILED", FiberState::Failed),
        ("DISPOSED", FiberState::Disposed),
        ("UNLOADING", FiberState::Unloading),
    ] {
        let number = fiber_state_number(state);
        browser_values::set(&states, &name.into(), &number.into())?;
        browser_values::set(&states, &number.to_string().into(), &name.into())?;
    }
    Ok(states)
}

/// Resolves the compiled Context backing an exact face or its metadata descendants.
///
/// # Errors
/// Propagates prototype lookup failures.
#[wasm_bindgen(js_name = contextCore)]
pub fn context_core(owner: &JsValue, fallback: &JsValue) -> Result<JsValue, JsValue> {
    let mut current = owner.clone();
    while current.is_object() || current.is_function() {
        let core = CONTEXT_CORES.with(|cores| cores.get(current.unchecked_ref::<Object>()));
        if !core.is_undefined() {
            return Ok(core);
        }
        current = Reflect::get_prototype_of(&current)?.into();
    }
    Ok(fallback.clone())
}

fn remember_context_alias(target: &JsValue, alias: &JsValue) {
    let core = CONTEXT_CORES.with(|cores| cores.get(target.unchecked_ref::<Object>()));
    if !core.is_undefined() {
        CONTEXT_CORES.with(|cores| cores.set(alias.unchecked_ref::<Object>(), &core));
    }
}

type FaceSlot = Arc<Mutex<Option<JsValue>>>;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = Object)]
    fn boxed_object(value: &JsValue) -> Object;

    #[wasm_bindgen(js_namespace = Reflect, js_name = get, catch)]
    fn get_with_receiver(
        target: &JsValue,
        key: &JsValue,
        receiver: &JsValue,
    ) -> Result<JsValue, JsValue>;
}

/// Tests the source Context brand, including inherited markers and primitive receivers.
///
/// # Errors
/// Propagates brand-key conversion and property-getter failures unchanged.
#[wasm_bindgen(js_name = contextIs)]
pub fn context_is(value: &JsValue, brand_key: &JsValue) -> Result<bool, JsValue> {
    if value.is_null() || value.is_undefined() {
        return Ok(false);
    }
    get_with_receiver(boxed_object(value).as_ref(), brand_key, value)
        .map(|marker| marker.is_truthy())
}

/// Configures the package wrapper that adds reflected service-property access.
///
/// # Errors
///
/// Returns when the supplied value is not callable.
#[wasm_bindgen(js_name = configureContextWrapper)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_context_wrapper(wrapper: JsValue) -> Result<(), JsValue> {
    let wrapper = wrapper
        .dyn_into::<Function>()
        .map_err(|_| js_sys::Error::new("cordis context wrapper must be a function"))?;
    CONTEXT_WRAPPER.with(|configured| *configured.borrow_mut() = Some(wrapper));
    Ok(())
}

/// Configures the public `EventsService` class prototype used by browser roots.
#[wasm_bindgen(js_name = configureEventsServicePrototype)]
pub fn configure_events_service_prototype(prototype: Object) {
    browser_events::configure_service_prototype(prototype);
}

/// Builds the Rust-backed event method descriptors for the public class.
///
/// # Errors
/// Returns JavaScript descriptor construction failures.
#[wasm_bindgen(js_name = eventsServicePrototype)]
pub fn events_service_prototype() -> Result<Object, JsValue> {
    browser_events::service_prototype()
}

/// Constructs an independent event service owned by the supplied Context.
///
/// # Errors
/// Returns constructor hook, Context, or Fiber registration failures unchanged.
#[wasm_bindgen(js_name = createEventsService)]
pub fn create_events_service(
    context: &JsValue,
    prototype: Option<Object>,
) -> Result<Object, JsValue> {
    browser_events::create_service(context, prototype)
}

/// Returns the source event-dispatch bail predicate for arbitrary JavaScript values.
#[wasm_bindgen(js_name = isBailed)]
pub fn is_bailed(value: &JsValue) -> bool {
    browser_events::is_bailed(value)
}

/// Creates one root browser Context backed by the portable Rust core.
///
/// # Errors
///
/// Returns JavaScript wrapper construction failures.
#[wasm_bindgen(js_name = createContext)]
pub fn create_context() -> Result<JsValue, JsValue> {
    create_context_inner(None)
}

/// Creates a root using the public constructor's actual prototype.
///
/// # Errors
/// Propagates Context construction and built-in service failures.
#[wasm_bindgen(js_name = createContextWithPrototype)]
pub fn create_context_with_prototype(prototype: Object) -> Result<JsValue, JsValue> {
    create_context_inner(Some(prototype))
}

fn create_context_inner(prototype: Option<Object>) -> Result<JsValue, JsValue> {
    let root_face = empty_face_slot();
    let fiber_face = empty_face_slot();
    let context = WasmContext::new(
        Context::new_with_clock_and_disposal(
            Arc::new(crate::SystemCordisClock),
            crate::DisposalScheduling::Concurrent,
        ),
        prototype
            .map_or_else(Object::new, |prototype| Object::create(&prototype))
            .into(),
        root_face.clone(),
        fiber_face.clone(),
    );
    for name in ["isolate", "intercept"] {
        Reflect::set(
            &context.metadata,
            &browser_symbols::get(name)?,
            &Object::create(&Object::from(JsValue::NULL)),
        )?;
    }
    let root_fiber = context.inner.fiber().clone();
    let binding = context.clone_for_binding();
    let face = wrap_context(context)?;
    *root_face.lock() = Some(face.clone());
    browser_values::set(&binding.metadata, &"root".into(), &face)?;
    browser_values::set(&binding.metadata, &"baseUrl".into(), &JsValue::UNDEFINED)?;
    *fiber_face.lock() = Some(root_fiber_face(&face, &root_fiber)?);
    browser_values::set(&binding.metadata, &"fiber".into(), &binding.fiber())?;
    binding.inner.fiber().set_browser_context(face.clone());
    browser_reflect::install(&binding)?;
    browser_values::set(
        &binding.metadata,
        &"reflect".into(),
        &browser_reflect::root(&binding)?,
    )?;
    browser_values::set(
        &binding.metadata,
        &"registry".into(),
        &browser_registry::root(&face)?,
    )?;
    browser_events::install(&binding, &face)?;
    browser_values::set(
        &binding.metadata,
        &"events".into(),
        &browser_events::table(&binding).service,
    )?;
    let logger = browser_logger::create_service(&face)?;
    if !logger.is_undefined() {
        browser_values::set(&binding.metadata, &"logger".into(), &logger)?;
    }
    browser_effects::finish_root_initialization(&binding.fiber())?;
    Ok(face)
}

/// Browser Context handle. The ESM Proxy captures caller errors and delegates
/// accessor and service resolution to the compiled reflection layer.
#[wasm_bindgen]
pub struct WasmContext {
    inner: Context,
    metadata: JsValue,
    root_face: FaceSlot,
    fiber_face: FaceSlot,
}

impl WasmContext {
    fn new(inner: Context, metadata: JsValue, root_face: FaceSlot, fiber_face: FaceSlot) -> Self {
        Self {
            inner,
            metadata,
            root_face,
            fiber_face,
        }
    }

    fn child(&self, inner: Context, metadata: JsValue, fiber_face: FaceSlot) -> Self {
        Self::new(inner, metadata, self.root_face.clone(), fiber_face)
    }

    fn source_scope_key(&self, name: &str) -> Result<JsValue, JsValue> {
        let scopes = browser_values::get(&self.metadata, &browser_symbols::get("isolate")?)?;
        browser_values::get(&scopes, &name.into())
    }

    fn source_implementation(&self, name: &str, strict: bool) -> Result<JsValue, JsValue> {
        let key = self.source_scope_key(name)?;
        let record = Reflect::get(&self.inner.browser_services().store, &key)?;
        if record.is_truthy() && strict {
            let fiber = Reflect::get(&record, &"fiber".into())?;
            if Reflect::get(&fiber, &"state".into())?.as_f64() != Some(2.0) {
                return Ok(JsValue::UNDEFINED);
            }
        }
        Ok(record)
    }
}

#[wasm_bindgen]
impl WasmContext {
    /// Reads a service without resolving mixed-in or reflected properties.
    ///
    /// # Errors
    /// Returns implementation-record getter failures unchanged.
    #[wasm_bindgen(js_name = serviceGet)]
    pub fn service_get(&self, name: &str, strict: &JsValue) -> Result<JsValue, JsValue> {
        let record =
            self.source_implementation(name, strict.is_undefined() || strict.is_truthy())?;
        if record.is_truthy() {
            return Reflect::get(&record, &"value".into());
        }
        let key = self.source_scope_key(name)?;
        let native_key = self.inner.browser_services().key(&self.inner.slot(name))?;
        if key != native_key
            && (!key.is_undefined()
                || Reflect::get(&self.inner.browser_services().store, &native_key)?.is_truthy())
        {
            return Ok(JsValue::UNDEFINED);
        }
        let value = if strict.is_undefined() || strict.is_truthy() {
            self.inner.get_named::<JsValue>(name)
        } else {
            self.inner.get_named_relaxed::<JsValue>(name)
        };
        Ok(value.as_deref().cloned().unwrap_or(JsValue::UNDEFINED))
    }

    /// Reads a reflected service or accessor by runtime name.
    ///
    /// # Errors
    ///
    /// Returns a reflected accessor failure.
    #[allow(clippy::needless_pass_by_value)]
    pub fn get(&self, name: String) -> Result<JsValue, JsValue> {
        if !self.inner.is_accessor(&name) {
            let mut fiber = self.fiber();
            if !fiber.is_undefined() {
                if Reflect::get(&fiber, &"uid".into())?.as_f64() == Some(0.0) {
                    return self.service_get(&name, &JsValue::FALSE);
                }
                let key = self.inner.browser_services().key(&self.inner.slot(&name))?;
                loop {
                    let store = Reflect::get(&fiber, &"store".into())?;
                    if !store.is_null() && !store.is_undefined() {
                        let record = Reflect::get(&store, &name.clone().into())?;
                        if record.is_truthy() {
                            return Reflect::get(&record, &"value".into());
                        }
                    }
                    let inject = Reflect::get(&fiber, &"inject".into())?;
                    if inject.is_object() && Reflect::has(&inject, &name.clone().into())? {
                        return Err(js_sys::Error::new(&format!(
                            "cannot get required service {name:?} in inactive context"
                        ))
                        .into());
                    }
                    if Reflect::get(&fiber, &"uid".into())?.as_f64() == Some(0.0) {
                        break;
                    }
                    let parent = Reflect::get(&fiber, &"parent".into())?;
                    if parent.is_null() || parent.is_undefined() {
                        break;
                    }
                    let native = Reflect::get(&parent, &"__seekdeepContext".into())?;
                    if required_function(&native, "serviceScope")?
                        .call1(&native, &name.clone().into())?
                        != key
                    {
                        break;
                    }
                    fiber = Reflect::get(&parent, &"fiber".into())?;
                }
                return Err(js_sys::Error::new(&format!(
                    "cannot get property {name:?} without inject"
                ))
                .into());
            }
        }
        self.inner
            .property::<JsValue>(&name)
            .map(|value| value.as_deref().cloned().unwrap_or(JsValue::UNDEFINED))
            .map_err(js_error)
    }

    /// Publishes one JavaScript service under this Fiber's ownership.
    ///
    /// # Errors
    ///
    /// Returns duplicate-provider or inactive-Fiber failures.
    #[allow(clippy::needless_pass_by_value)]
    pub fn provide(&self, name: String, value: JsValue) -> Result<Function, JsValue> {
        self.provide_browser(&name, value, JsValue::UNDEFINED, None)
    }

    /// Writes a reflected accessor or provider-owned service.
    ///
    /// # Errors
    ///
    /// Returns missing-provider, ownership, or setter failures.
    #[wasm_bindgen(js_name = setProperty)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn set_property(&self, name: String, value: JsValue) -> Result<bool, JsValue> {
        let record = self
            .inner
            .browser_services()
            .get_impl(&self.inner, &name, false)?;
        if record.is_truthy() {
            if Reflect::get(&record, &"fiber".into())? != self.fiber() {
                return Err(js_sys::Error::new(&format!(
                    "cannot set property {name:?} in multiple fibers"
                ))
                .into());
            }
            if !Reflect::set(&record, &"value".into(), &value)? {
                return Err(
                    js_sys::TypeError::new("Cannot assign to read only property 'value'").into(),
                );
            }
            return Ok(true);
        }
        self.inner
            .set_property(&name, Arc::new(value))
            .map_err(js_error)
    }

    /// Assigns an existing reflected property through the source `ctx.set` entry.
    ///
    /// # Errors
    /// Returns the same ownership and accessor errors as property assignment.
    pub fn set(&self, name: String, value: JsValue) -> Result<bool, JsValue> {
        self.set_property(name, value)
    }

    /// Loads one JavaScript plugin descriptor through Rust lifecycle ownership.
    ///
    /// # Errors
    ///
    /// Returns malformed descriptor, config, publication, or inactive-parent failures.
    #[allow(clippy::needless_pass_by_value)]
    pub fn plugin(
        &self,
        descriptor: JsValue,
        config: JsValue,
        parent: Option<JsValue>,
    ) -> Result<JsValue, JsValue> {
        let parent = match parent {
            Some(parent) => parent,
            None => wrap_context(self.clone_for_binding())?,
        };
        browser_registry::method(
            &self.registry_face(&parent)?,
            "plugin",
            &Array::of2(&descriptor, &config),
        )
    }

    /// Mounts a normalized runtime under the supplied browser Context.
    ///
    /// # Errors
    /// Propagates constructor metadata, publication, and dependency failures.
    #[wasm_bindgen(js_name = mountRuntime)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn mount_runtime(
        &self,
        runtime: JsValue,
        inject: JsValue,
        config: JsValue,
        parent: JsValue,
        stack: JsValue,
        prototype: Option<Object>,
    ) -> Result<JsValue, JsValue> {
        let registry = Reflect::get(&parent, &"registry".into())?;
        let uid = Reflect::get(&registry, &"counter".into())?;
        let metadata = parent.clone();
        let context_face = empty_face_slot();
        let fiber_face = empty_face_slot();
        let browser_config = browser_config::BrowserConfig::new();
        let result_face = empty_face_slot();
        let returned = result_face.clone();
        let root_face = self.root_face.clone();
        let dependencies = Object::keys(inject.unchecked_ref::<Object>())
            .iter()
            .filter_map(|name| name.as_string())
            .collect::<Vec<_>>();
        let plugin = plugin_from_js(
            &runtime,
            dependencies,
            metadata.clone(),
            self.root_face.clone(),
            context_face.clone(),
            fiber_face.clone(),
            browser_config.clone(),
        )
        .with_browser_binding(move |native, phase| {
            let bind = || -> Result<(), JsValue> {
                match phase {
                    crate::plugin::BrowserMountPhase::CreateFace => {
                        let fiber: JsValue = WasmFiber::new(
                            native.clone(),
                            context_face.clone(),
                            fiber_face.clone(),
                            uid.clone(),
                        )
                        .into();
                        *fiber_face.lock() = Some(fiber.clone());
                        let (core, handle) = wrap_fiber(
                            &fiber,
                            &config,
                            &native,
                            &fiber_face,
                            prototype.clone().or_else(browser_fiber_api::prototype),
                        )?;
                        browser_fiber::data_field(core.unchecked_ref(), "parent", parent.clone())?;
                        browser_fiber::data_field(
                            core.unchecked_ref(),
                            "runtime",
                            runtime.clone(),
                        )?;
                        browser_fiber::data_field(core.unchecked_ref(), "inject", inject.clone())?;
                        *fiber_face.lock() = Some(core.clone());
                        let child = create_fiber_context(
                            &parent,
                            &core,
                            native.context().clone(),
                            root_face.clone(),
                            fiber_face.clone(),
                        )?;
                        *context_face.lock() = Some(child.clone());
                        browser_fiber::data_field(core.unchecked_ref(), "ctx", child.clone())?;
                        browser_fiber::data_field(core.unchecked_ref(), "context", child.clone())?;
                        browser_registry::inherit_intercepts(&parent, &child, &inject)?;
                        native.fiber().set_browser_context(child);
                        browser_stack::remember_owner(&core, &stack)?;
                        browser_runner::install(&core, &runtime, &stack)?;
                        native.set_browser_face(core.clone());
                        *fiber_face.lock() = Some(core.clone());
                        *returned.lock() = Some(handle);
                        browser_config.attach(&native, fiber_face.clone());
                    }
                    crate::plugin::BrowserMountPhase::Own(structural) => {
                        own_browser_plugin(&parent, &native, &runtime, structural)?;
                    }
                    crate::plugin::BrowserMountPhase::AfterPublication => {
                        let inject = Reflect::get(&native.browser_face(), &"inject".into())?;
                        native.set_browser_inject(
                            Object::keys(&Object::from(inject))
                                .iter()
                                .filter_map(|name| name.as_string())
                                .collect(),
                        );
                        browser_config.initial()?;
                    }
                }
                Ok(())
            };
            bind().map_err(|error| js_anyhow(&error))
        });
        self.inner
            .plugin(plugin, Value::Null)
            .map_err(|error| match error {
                crate::CordisError::BrowserPublication(error)
                | crate::CordisError::BrowserEffect(error) => error,
                error => js_error(error),
            })?;
        result_face
            .lock()
            .clone()
            .ok_or_else(|| js_sys::Error::new("plugin mount omitted its browser Fiber face").into())
    }

    /// Shorthand plugin registration for one dependency declaration.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`WasmContext::plugin`].
    #[allow(clippy::needless_pass_by_value)]
    pub fn inject(
        &self,
        dependencies: JsValue,
        callback: JsValue,
        parent: Option<JsValue>,
    ) -> Result<JsValue, JsValue> {
        let descriptor = Object::new();
        set(&descriptor, "name", &JsValue::from_str("inject"))?;
        set(&descriptor, "inject", &dependencies)?;
        set(&descriptor, "apply", &callback)?;
        self.plugin(descriptor.into(), JsValue::UNDEFINED, parent)
    }

    /// Registers an owned listener.
    ///
    /// # Errors
    ///
    /// Returns malformed option or inactive-Fiber failures.
    #[allow(clippy::needless_pass_by_value)]
    pub fn on(
        &self,
        name: &JsValue,
        listener: JsValue,
        options: JsValue,
        owner: Option<JsValue>,
    ) -> Result<JsValue, JsValue> {
        browser_events::register(self, name, &listener, &options, owner)
    }

    /// Registers a listener removed before its first invocation, including reentry.
    ///
    /// # Errors
    /// Returns interception, malformed option, or inactive-Fiber failures.
    pub fn once(
        &self,
        name: &JsValue,
        listener: &JsValue,
        options: &JsValue,
        owner: Option<JsValue>,
    ) -> Result<JsValue, JsValue> {
        browser_events::once(self, name, listener, options, owner)
    }

    /// Emits one event and detaches asynchronous listener work.
    ///
    /// # Errors
    ///
    /// Returns a synchronous listener or dispatch failure.
    #[wasm_bindgen(js_name = emitArgs)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn emit_args(&self, name: String, args: Array) -> Result<(), JsValue> {
        self.inner
            .events()
            .emit(&self.inner, &name, &event_args_from_js(&args))
            .map_err(js_error)
    }

    /// Awaits every selected listener.
    #[wasm_bindgen(js_name = parallelArgs)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn parallel_args(&self, name: String, args: Array) -> Promise {
        let context = self.inner.clone();
        let args = event_args_from_js(&args);
        future_to_promise(async move {
            context
                .events()
                .parallel(&context, &name, &args)
                .await
                .map_err(js_error)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Awaits listeners in order and returns the first bail value.
    #[wasm_bindgen(js_name = serialArgs)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn serial_args(&self, name: String, args: Array) -> Promise {
        let context = self.inner.clone();
        let args = event_args_from_js(&args);
        future_to_promise(async move {
            let reply = context
                .events()
                .serial(&context, &name, &args)
                .await
                .map_err(js_error)?;
            Ok(event_reply_to_js(reply))
        })
    }

    /// Synchronously dispatches until one listener bails.
    ///
    /// # Errors
    ///
    /// Returns immediate dispatch failures.
    #[wasm_bindgen(js_name = bailArgs)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn bail_args(&self, name: String, args: Array) -> Result<JsValue, JsValue> {
        match self
            .inner
            .events()
            .bail(&self.inner, &name, &event_args_from_js(&args))
            .map_err(js_error)?
        {
            BailReply::Settled(reply) => Ok(event_reply_to_js(reply)),
            BailReply::Pending(future) => Ok(future_to_promise(async move {
                future.await.map(event_reply_to_js).map_err(js_error)
            })
            .into()),
        }
    }

    /// Dispatches raw browser arguments, including the optional listener receiver.
    ///
    /// # Errors
    /// Returns malformed event names, filter errors, or synchronous listener errors.
    #[wasm_bindgen(js_name = eventArgs)]
    pub fn event_args(&self, mode: &str, args: &Array) -> Result<JsValue, JsValue> {
        browser_events::invoke(self, mode, args)
    }

    /// Consumes an optional receiver and event name, returning bound callbacks.
    ///
    /// # Errors
    /// Returns malformed event names, filter errors, or dispatch-observer errors.
    pub fn dispatch(&self, mode: &str, args: &Array) -> Result<Array, JsValue> {
        browser_events::dispatch(self, mode, args)
    }

    /// Event service traced to the caller while sharing its mutable hook table.
    ///
    /// # Errors
    /// Returns service tracing failures unchanged.
    #[wasm_bindgen(js_name = eventsFace)]
    pub fn events_face(&self, owner: &JsValue) -> Result<JsValue, JsValue> {
        let service = tracing::Tracer::new(owner.clone())
            .trace(&browser_events::table(self).service.into())?;
        browser_events::remember_context(owner, self.clone_for_binding());
        browser_events::remember_service(&service, owner);
        Ok(service)
    }

    /// Registers an exact callback in a caller-supplied event list.
    ///
    /// # Errors
    /// Returns list mutation failures or inactive-owner errors.
    #[wasm_bindgen(js_name = eventRegister)]
    pub fn event_register(
        &self,
        label: String,
        hooks: JsValue,
        callback: JsValue,
        options: &JsValue,
        owner: &JsValue,
    ) -> Result<JsValue, JsValue> {
        browser_events::register_explicit(self, label, hooks, callback, options, owner)
    }

    /// Removes the first record with the supplied callback identity.
    ///
    /// # Errors
    /// Returns list lookup and mutation failures unchanged.
    #[wasm_bindgen(js_name = unregister)]
    pub fn event_unregister(
        &self,
        hooks: &JsValue,
        callback: &JsValue,
    ) -> Result<JsValue, JsValue> {
        browser_events::unregister(hooks, callback)
    }

    /// Registers an arbitrary setup result in the current Fiber ledger.
    ///
    /// # Errors
    ///
    /// Returns setup or inactive-Fiber failures.
    #[allow(clippy::needless_pass_by_value)]
    pub fn effect(&self, setup: JsValue, label: JsValue) -> Result<Function, JsValue> {
        context_effect(self.fiber(), &setup, label, browser_stack::capture_outer()?)
    }

    /// Creates a metadata-bearing child context with the same lifecycle owner.
    ///
    /// # Errors
    ///
    /// Returns malformed metadata or wrapper failures.
    #[allow(clippy::needless_pass_by_value)]
    pub fn extend(&self, extension: JsValue, parent: Option<JsValue>) -> Result<JsValue, JsValue> {
        let parent = parent.unwrap_or_else(|| self.metadata.clone());
        let metadata = browser_context::extend_metadata(&parent, &extension)?;
        wrap_context(self.child(self.inner.clone(), metadata, self.fiber_face.clone()))
    }

    /// Associates source-shaped metadata with the current native Context.
    #[wasm_bindgen(js_name = bindContextMetadata)]
    pub fn bind_context_metadata(&self, face: &JsValue) {
        bind_context_face(
            face,
            self.child(self.inner.clone(), face.clone(), self.fiber_face.clone()),
        );
    }

    /// Rebinds a custom wrapper's raw Context handle to its constructed Fiber owner.
    #[wasm_bindgen(js_name = adoptContext)]
    pub fn adopt_context(&mut self, binding: &Self) {
        self.inner = binding.inner.clone();
        self.root_face = binding.root_face.clone();
        self.fiber_face = binding.fiber_face.clone();
    }

    /// Associates an extended source scope with its corresponding native isolation realm.
    ///
    /// # Errors
    /// Propagates isolation-label construction failures.
    #[wasm_bindgen(js_name = bindContextIsolation)]
    pub fn bind_context_isolation(
        &self,
        face: &JsValue,
        name: &str,
        label: &JsValue,
    ) -> Result<(), JsValue> {
        let inner = self
            .inner
            .browser_services()
            .isolate(&self.inner, name, label)?;
        bind_context_face(
            face,
            self.child(inner, face.clone(), self.fiber_face.clone()),
        );
        Ok(())
    }

    /// Creates a child with an independent service scope.
    ///
    /// # Errors
    ///
    /// Returns wrapper failures.
    #[allow(clippy::needless_pass_by_value)]
    pub fn isolate(
        &self,
        name: String,
        label: JsValue,
        parent: Option<JsValue>,
    ) -> Result<JsValue, JsValue> {
        let label = if label.is_null() || label.is_undefined() {
            required_function(&js_sys::global(), "Symbol")?
                .call1(&JsValue::UNDEFINED, &name.clone().into())?
        } else {
            label
        };
        let inner = self
            .inner
            .browser_services()
            .isolate(&self.inner, &name, &label)?;
        let parent = parent.unwrap_or_else(|| self.metadata.clone());
        let metadata = Object::create(&Object::from(parent.clone()));
        let key = browser_symbols::get("isolate")?;
        let inherited = Reflect::get(&parent, &key)?;
        let scopes = Object::create(inherited.unchecked_ref::<Object>());
        Reflect::set(&scopes, &name.into(), &label)?;
        Reflect::set(&metadata, &key, &scopes)?;
        wrap_context(self.child(inner, metadata.into(), self.fiber_face.clone()))
    }

    /// Creates a child carrying one service intercept.
    ///
    /// # Errors
    ///
    /// Returns metadata, prototype, or wrapper failures.
    #[allow(clippy::needless_pass_by_value)]
    pub fn intercept(
        &self,
        name: String,
        config: JsValue,
        parent: Option<JsValue>,
    ) -> Result<JsValue, JsValue> {
        let native_config = serde_wasm_bindgen::from_value(config.clone()).unwrap_or(Value::Null);
        let parent = parent.unwrap_or_else(|| self.metadata.clone());
        let metadata = Object::create(&Object::from(parent.clone()));
        let key = browser_symbols::get("intercept")?;
        let inherited = Reflect::get(&parent, &key)?;
        let intercepts = Object::create(inherited.unchecked_ref::<Object>());
        Reflect::set(&intercepts, &name.clone().into(), &config)?;
        Reflect::set(&metadata, &key, &intercepts)?;
        wrap_context(self.child(
            self.inner.intercept(&name, native_config),
            metadata.into(),
            self.fiber_face.clone(),
        ))
    }

    /// Exposes service members as reflected Context properties.
    ///
    /// # Errors
    ///
    /// Returns duplicate-accessor or inactive-Fiber failures.
    #[allow(clippy::needless_pass_by_value)]
    pub fn mixin(&self, source: String, members: Array) -> Result<Function, JsValue> {
        let mut effects = Vec::new();
        for member in members.iter() {
            let name = member
                .as_string()
                .ok_or_else(|| js_sys::Error::new("ctx.mixin members must be strings"))?;
            let source = source.clone();
            let property = name.clone();
            let effect = self
                .inner
                .accessor_read_only::<JsValue, _>(name, move |context| {
                    let Some(service) = context.get_named::<JsValue>(&source) else {
                        return Ok(None);
                    };
                    Reflect::get(service.as_ref(), &JsValue::from_str(&property))
                        .map(Arc::new)
                        .map(Some)
                        .map_err(|error| js_anyhow(&error))
                })
                .map_err(|error| js_sys::Error::new(&error.to_string()))?;
            effects.push(effect);
        }
        let closure = Closure::wrap(Box::new(move || -> Promise {
            future_to_promise({
                let effects = effects.clone();
                async move {
                    let failures =
                        futures::future::join_all(effects.iter().map(EffectHandle::dispose))
                            .await
                            .into_iter()
                            .filter_map(Result::err)
                            .map(|error| format!("{error:#}"))
                            .collect::<Vec<_>>();
                    if failures.is_empty() {
                        Ok(JsValue::UNDEFINED)
                    } else {
                        Err(js_sys::Error::new(&failures.join("\n")).into())
                    }
                }
            })
        }) as Box<dyn FnMut() -> Promise>);
        Ok(closure.into_js_value().unchecked_into())
    }

    /// Reads inherited metadata for the ESM Proxy binding.
    ///
    /// # Errors
    ///
    /// Returns JavaScript property-access failures.
    #[wasm_bindgen(js_name = metaGet)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn meta_get(&self, key: JsValue, receiver: Option<JsValue>) -> Result<JsValue, JsValue> {
        let receiver = receiver.unwrap_or_else(|| self.metadata.clone());
        let value = get_with_receiver(&self.metadata, &key, &receiver)?;
        let special = key.is_symbol()
            || key.as_string().is_some_and(|key| {
                key.starts_with('_') || matches!(key.as_str(), "prototype" | "then")
            });
        if special
            || Function::new_with_args("key", "return parseInt(key).toString() === key;")
                .call1(&JsValue::UNDEFINED, &key)?
                .is_truthy()
        {
            Ok(value)
        } else {
            tracing::Tracer::new(receiver).trace(&value)
        }
    }

    /// Canonical property storage used as the public Proxy target.
    #[wasm_bindgen(getter, js_name = contextData)]
    pub fn context_data(&self) -> JsValue {
        self.metadata.clone()
    }

    /// Tests metadata membership without evaluating accessors.
    ///
    /// # Errors
    /// Propagates property-descriptor or prototype access failures.
    #[wasm_bindgen(js_name = metaHas)]
    pub fn meta_has(&self, key: &JsValue, boundary: &JsValue) -> Result<bool, JsValue> {
        let mut current = self.metadata.clone();
        while !current.is_null() && !Object::is(&current, boundary) {
            if !Reflect::get_own_property_descriptor(&Object::from(current.clone()), key)?
                .is_undefined()
            {
                return Ok(true);
            }
            current = Reflect::get_prototype_of(&current)?.into();
        }
        Ok(false)
    }

    /// Writes metadata with the calling context as the accessor receiver.
    ///
    /// # Errors
    /// Propagates setter failures; returns false for a readonly property.
    #[wasm_bindgen(js_name = metaSet)]
    pub fn meta_set(
        &self,
        key: &JsValue,
        value: &JsValue,
        receiver: &JsValue,
    ) -> Result<bool, JsValue> {
        Reflect::set_with_receiver(&self.metadata, key, value, receiver)
    }

    /// Whether a service or accessor name has been declared, independent of availability.
    ///
    /// # Errors
    /// Propagates getters on the mutable property-definition table.
    #[wasm_bindgen(js_name = propertyDefined)]
    pub fn property_defined(&self, name: &str) -> Result<bool, JsValue> {
        Reflect::get(&self.inner.browser_services().props, &name.into())
            .map(|value| value.is_truthy())
    }

    /// Root Context face shared by every child.
    #[wasm_bindgen(getter)]
    pub fn root(&self) -> JsValue {
        self.root_face.lock().clone().unwrap_or(JsValue::UNDEFINED)
    }

    /// Fiber face that owns this Context.
    #[wasm_bindgen(getter)]
    pub fn fiber(&self) -> JsValue {
        self.fiber_face.lock().clone().unwrap_or(JsValue::UNDEFINED)
    }

    /// Reflection face used by source-compatible package bindings.
    ///
    /// # Errors
    ///
    /// Returns JavaScript object-construction failures.
    #[wasm_bindgen(getter)]
    pub fn reflect(&self) -> Result<JsValue, JsValue> {
        self.reflection_face(wrap_context(self.clone_for_binding())?)
    }

    /// Builds the reflection binding for the exact calling JavaScript Context.
    ///
    /// # Errors
    /// Returns property construction or binding failures.
    #[wasm_bindgen(js_name = reflectionFace)]
    pub fn reflection_face(&self, owner: JsValue) -> Result<JsValue, JsValue> {
        tracing::Tracer::new(owner).trace(&browser_reflect::root(self)?)
    }

    /// Creates a provider resource whose wrapper owns browser notification and cleanup.
    ///
    /// # Errors
    /// Preserves declaration, ownership, and registration failures.
    #[wasm_bindgen(js_name = provideResource)]
    pub fn provide_resource(
        &self,
        name: &str,
        value: JsValue,
        check: JsValue,
        reflect: JsValue,
    ) -> Result<Function, JsValue> {
        self.provide_browser(name, value, check, Some(reflect))
    }

    /// Tests whether reflection registration retains this native table, scope, and owner.
    ///
    /// # Errors
    /// Propagates Context root, isolation-map, and Fiber getter failures.
    #[wasm_bindgen(js_name = reflectionRecordsMatch)]
    pub fn reflection_records_match(
        &self,
        store: &JsValue,
        props: &JsValue,
        name: &str,
        context: &JsValue,
    ) -> Result<bool, JsValue> {
        let records = self.inner.browser_services();
        if store != records.store.as_ref() as &JsValue
            || props != records.props.as_ref() as &JsValue
        {
            return Ok(false);
        }
        let root = browser_values::get(context, &"root".into())?;
        if root != self.root() {
            return Ok(false);
        }
        let isolate = browser_symbols::get("isolate")?;
        let root_scopes = browser_values::get(&root, &isolate)?;
        let scopes = browser_values::get(context, &isolate)?;
        let label = browser_values::get(&scopes, &name.into())?;
        if (label.is_null() || label.is_undefined())
            && scopes != root_scopes
            && Reflect::has(&scopes, &name.into())?
        {
            return Ok(false);
        }
        if !records.scope_matches(&self.inner, name, &label) {
            return Ok(false);
        }
        Ok(browser_values::get(context, &"fiber".into())? == self.fiber())
    }

    /// Traces a service value to the calling JavaScript Context.
    ///
    /// # Errors
    /// Returns tracker or property lookup failures unchanged.
    #[wasm_bindgen(js_name = traceService)]
    pub fn trace_service(&self, value: &JsValue, owner: &JsValue) -> Result<JsValue, JsValue> {
        let native_events = self
            .inner
            .events()
            .browser_table
            .lock()
            .as_ref()
            .is_some_and(|table| value == table.service.as_ref() as &JsValue);
        let traced = tracing::Tracer::new(owner.clone()).trace(value)?;
        if native_events && (owner.is_object() || owner.is_function()) {
            browser_events::remember_context(owner, self.clone_for_binding());
            browser_events::remember_service(&traced, owner);
        }
        Ok(traced)
    }

    /// Registry compatibility face. Lifecycle still runs in the Rust registry.
    ///
    /// # Errors
    ///
    /// Returns JavaScript object-construction failures.
    #[wasm_bindgen(getter)]
    pub fn registry(&self) -> Result<JsValue, JsValue> {
        self.registry_face(&wrap_context(self.clone_for_binding())?)
    }

    /// Registry methods traced to the caller while retaining the root runtime table.
    ///
    /// # Errors
    /// Propagates service construction and tracing failures.
    #[wasm_bindgen(js_name = registryFace)]
    pub fn registry_face(&self, owner: &JsValue) -> Result<JsValue, JsValue> {
        let registry = browser_registry::root(&self.root())?;
        tracing::Tracer::new(owner.clone()).trace(&registry)
    }

    /// Event service for a Context accessed without the package wrapper.
    ///
    /// # Errors
    /// Returns Context construction or service tracing failures.
    #[wasm_bindgen(getter)]
    pub fn events(&self) -> Result<JsValue, JsValue> {
        self.events_face(&wrap_context(self.clone_for_binding())?)
    }

    /// Returns the shared implementation-table key for this service scope.
    ///
    /// # Errors
    /// Returns Symbol construction failures unchanged.
    #[wasm_bindgen(js_name = serviceScope)]
    pub fn service_scope(&self, name: &str) -> Result<JsValue, JsValue> {
        self.inner.browser_services().key(&self.inner.slot(name))
    }
}

impl WasmContext {
    fn provide_browser(
        &self,
        name: &str,
        value: JsValue,
        check: JsValue,
        notification: Option<JsValue>,
    ) -> Result<Function, JsValue> {
        if !self.inner.fiber().can_register_effect() {
            return Err(browser_errors::inactive());
        }
        let records = self.inner.browser_services();
        browser_reflect::check_service(&records.props, name)?;
        browser_services::initialize_root_key(&self.root(), name)?;
        let key = records.key(&self.inner.slot(name))?;
        let previous = Reflect::get(&records.store, &key)?;
        if previous.is_truthy() {
            let fiber = Reflect::get(&previous, &"fiber".into())?;
            let label = Reflect::get(&fiber, &"name".into())?
                .as_string()
                .unwrap_or_else(|| "anonymous".to_owned());
            return Err(js_sys::Error::new(&format!(
                "service \"{name}\" has been registered at <{label}>"
            ))
            .into());
        }
        let fiber = self.fiber();
        let implementation = object(&[
            ("name", name.into()),
            ("value", value.clone()),
            ("fiber", fiber.clone()),
            ("check", check),
        ])?;
        Reflect::set(&records.store, &key, &implementation)?;
        let store = Reflect::get(&fiber, &"store".into())?;
        if store.is_null() || store.is_undefined() {
            records.retain_failed(&self.inner, name, &implementation);
            let kind = if store.is_null() { "null" } else { "undefined" };
            return Err(js_sys::TypeError::new(&format!(
                "Cannot set properties of {kind} (setting '{name}')"
            ))
            .into());
        }
        Reflect::set(&store, &name.into(), &implementation)?;
        let native = if notification.is_some() {
            self.inner.provide_browser_named(name, Arc::new(value))
        } else {
            self.inner.provide_named(name, Arc::new(value))
        };
        let native = match native {
            Ok(native) => native,
            Err(error) => {
                records.remove(&self.inner, name, &implementation)?;
                Reflect::delete_property(store.unchecked_ref::<Object>(), &name.into())?;
                return Err(js_sys::Error::new(&error.to_string()).into());
            }
        };
        self.inner
            .attach_browser_service(name, implementation.clone().into());
        browser_effects::remove_native(&fiber, &native)?;
        if let Some(service) = &notification
            && Reflect::get(&fiber, &"state".into())?.as_f64() == Some(2.0)
        {
            browser_registry::method(service, "notify", &Array::of1(&Array::of1(&name.into())))?;
        }
        let browser_owned = notification.is_some();
        let effect = BrowserProvider {
            context: self.inner.clone(),
            records,
            name: name.to_owned(),
            implementation: implementation.into(),
            fiber,
            native,
            notification,
        }
        .cleanup();
        if browser_owned {
            return Ok(provider_disposer(effect));
        }
        self.inner
            .own(effect.clone())
            .map_err(|error| js_sys::Error::new(&error.to_string()))?;
        Ok(effect_disposer(effect))
    }

    fn clone_for_binding(&self) -> Self {
        Self::new(
            self.inner.clone(),
            self.metadata.clone(),
            self.root_face.clone(),
            self.fiber_face.clone(),
        )
    }
}

struct BrowserProvider {
    context: Context,
    records: Arc<browser_services::BrowserServices>,
    name: String,
    implementation: JsValue,
    fiber: JsValue,
    native: EffectHandle,
    notification: Option<JsValue>,
}

impl BrowserProvider {
    fn cleanup(self) -> EffectHandle {
        EffectHandle::new(format!("ctx.provide({:?})", self.name), move || {
            Box::pin(async move {
                self.records
                    .remove(&self.context, &self.name, &self.implementation)
                    .map_err(|error| js_anyhow(&error))?;
                self.native.dispose().await?;
                let pending = if let Some(service) = &self.notification {
                    browser_reflect::notify_waiters(service, &self.name)
                        .map_err(|error| js_anyhow(&error))?
                } else {
                    native_provider_waiters(&self.context, &self.name)
                        .map_err(|error| js_anyhow(&error))?
                };
                JsFuture::from(Promise::all_settled(pending.unchecked_ref()))
                    .await
                    .map_err(|error| js_anyhow(&error))?;
                let store = Reflect::get(&self.fiber, &"store".into())
                    .map_err(|error| js_anyhow(&error))?;
                if !store.is_null() && !store.is_undefined() {
                    Reflect::delete_property(store.unchecked_ref::<Object>(), &self.name.into())
                        .map_err(|error| js_anyhow(&error))?;
                }
                Ok(())
            })
        })
    }
}

fn native_provider_waiters(context: &Context, name: &str) -> Result<JsValue, JsValue> {
    let pending = Array::new();
    for dependent in context.registry().service_dependents(context, name) {
        let face = dependent.browser_face();
        if face.is_undefined() {
            pending.push(&future_to_promise(async move {
                dependent.await_settled().await.map_err(js_error)?;
                Ok(JsValue::UNDEFINED)
            }));
        } else {
            pending.push(&required_function(&face, "await")?.call0(&face)?);
        }
    }
    Ok(pending.into())
}

fn provider_disposer(effect: EffectHandle) -> Function {
    let dispose = native_effect_disposer(effect);
    Closure::wrap(Box::new(move || match dispose.call0(&JsValue::UNDEFINED) {
        Ok(result) => Promise::resolve(&result),
        Err(error) => Promise::reject(&error),
    }) as Box<dyn Fn() -> Promise>)
    .into_js_value()
    .unchecked_into()
}

/// JavaScript Fiber face backed by one exact Rust plugin generation.
#[wasm_bindgen]
pub struct WasmFiber {
    inner: Arc<PluginFiber>,
    context: FaceSlot,
    entry: FaceSlot,
    face: FaceSlot,
    hooks: Object,
    uid: JsValue,
}

#[wasm_bindgen]
impl WasmFiber {
    /// Executes a runner for raw WASM callers.
    ///
    /// # Errors
    /// Propagates runner and result-shape failures.
    #[wasm_bindgen(js_name = _execute)]
    pub fn execute_runner(&self, runner: &JsValue) -> Result<JsValue, JsValue> {
        let owner = self.face.lock().clone().unwrap_or(JsValue::UNDEFINED);
        browser_runner::execute(&owner, runner)
    }

    /// Recomputes the receiver's dependency epoch from its current snapshot.
    ///
    /// # Errors
    /// Propagates snapshot, state, and lifecycle observer failures.
    #[wasm_bindgen(js_name = _refresh)]
    pub fn refresh(&self, owner: Option<JsValue>) -> Result<(), JsValue> {
        let owner = owner.unwrap_or_else(|| self.face.lock().clone().unwrap_or(JsValue::UNDEFINED));
        browser_fiber::BrowserLifecycle::refresh(&owner)
    }

    /// Checks one current implementation for raw WASM Fiber callers.
    ///
    /// # Errors
    /// Propagates reflection and logging failures.
    #[wasm_bindgen(js_name = _checkImpl)]
    pub fn check_impl(&self, name: &JsValue, owner: Option<JsValue>) -> Result<JsValue, JsValue> {
        let owner = owner.unwrap_or_else(|| self.face.lock().clone().unwrap_or(JsValue::UNDEFINED));
        browser_fiber_api::fiber_check_impl(&owner, name)
    }
}

impl WasmFiber {
    fn new(inner: Arc<PluginFiber>, context: FaceSlot, face: FaceSlot, uid: JsValue) -> Self {
        Self {
            inner,
            context,
            entry: empty_face_slot(),
            face,
            hooks: Object::create(&Object::from(JsValue::NULL)),
            uid,
        }
    }
}

#[wasm_bindgen]
impl WasmFiber {
    /// Checks whether this Fiber can accept a new effect.
    ///
    /// # Errors
    /// Rejects permanently disposed Fibers.
    #[wasm_bindgen(js_name = assertActive)]
    pub fn assert_active(&self) -> Result<(), JsValue> {
        if self.inner.is_disposed() {
            Err(browser_errors::inactive())
        } else {
            Ok(())
        }
    }

    /// Registers setup and cleanup on this Fiber's Context.
    ///
    /// # Errors
    /// Returns setup or inactive-owner failures.
    pub fn effect(
        &self,
        setup: &JsValue,
        label: JsValue,
        owner: Option<JsValue>,
    ) -> Result<Function, JsValue> {
        self.assert_active()?;
        let owner = owner.unwrap_or_else(|| self.face.lock().clone().unwrap_or(JsValue::UNDEFINED));
        if Reflect::get(&owner, &"state".into())?.as_f64() == Some(5.0) {
            return Err(browser_errors::inactive());
        }
        context_effect(owner, setup, label, browser_stack::capture_outer()?)
    }

    /// Metadata of the currently owned browser effects.
    ///
    /// # Errors
    /// Propagates disposable-list and metadata getter failures.
    #[wasm_bindgen(js_name = getEffects)]
    pub fn get_effects(&self, owner: Option<JsValue>) -> Result<Array, JsValue> {
        let owner = owner.unwrap_or_else(|| self.face.lock().clone().unwrap_or(JsValue::UNDEFINED));
        browser_effects::diagnostics(&owner)
    }

    /// Persistent source hook lists, separate from activation-owned effects.
    #[wasm_bindgen(getter, js_name = _hooks)]
    pub fn hooks(&self) -> Object {
        self.hooks.clone()
    }

    /// Plugin-scoped Context.
    #[wasm_bindgen(getter, js_name = ctx)]
    pub fn context(&self) -> JsValue {
        self.context.lock().clone().unwrap_or(JsValue::UNDEFINED)
    }

    /// Source-compatible numeric lifecycle state.
    #[wasm_bindgen(getter)]
    pub fn state(&self) -> u8 {
        fiber_state_number(self.inner.fiber().state())
    }

    /// Plugin name used by source service-registration diagnostics.
    ///
    /// # Errors
    /// Returns name or parent getter failures unchanged.
    #[wasm_bindgen(getter)]
    pub fn name(&self) -> Result<JsValue, JsValue> {
        let core = self.face.lock().clone().unwrap_or(JsValue::UNDEFINED);
        browser_fiber_api::fiber_name(&core)
    }

    /// Monotonic runtime identity; null after disposal.
    #[wasm_bindgen(getter)]
    pub fn uid(&self) -> JsValue {
        if self.inner.is_disposed() {
            return JsValue::NULL;
        }
        self.uid.clone()
    }

    /// Required service map used by Loader diagnostics.
    ///
    /// # Errors
    ///
    /// Returns JavaScript object-construction failures.
    #[wasm_bindgen(getter)]
    pub fn inject(&self) -> Result<JsValue, JsValue> {
        let inject = Object::new();
        for name in self.inner.inject() {
            set(&inject, &name, &JsValue::NULL)?;
        }
        Ok(inject.into())
    }

    /// Loader entry associated with this Fiber.
    #[wasm_bindgen(getter)]
    pub fn entry(&self) -> JsValue {
        self.entry.lock().clone().unwrap_or(JsValue::UNDEFINED)
    }

    /// Associates the Loader entry before activation diagnostics run.
    #[wasm_bindgen(setter)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn set_entry(&self, entry: JsValue) {
        *self.entry.lock() = Some(entry);
    }

    /// Waits until currently admitted lifecycle work settles.
    #[wasm_bindgen(js_name = await)]
    pub fn wait(&self, owner: Option<JsValue>) -> Promise {
        browser_fiber::BrowserLifecycle::wait(
            owner.unwrap_or_else(|| self.face.lock().clone().unwrap_or(JsValue::UNDEFINED)),
        )
    }

    /// Permanently disposes this plugin generation.
    pub fn dispose(&self) -> JsValue {
        if self.inner.is_disposed() {
            return JsValue::UNDEFINED;
        }
        let fiber = self.inner.clone();
        let mut future = Box::pin(async move {
            fiber.dispose().await.map_err(js_error)?;
            Ok(JsValue::UNDEFINED)
        });
        let mut task = std::task::Context::from_waker(std::task::Waker::noop());
        match std::future::Future::poll(future.as_mut(), &mut task) {
            std::task::Poll::Ready(Ok(value)) => Promise::resolve(&value).into(),
            std::task::Poll::Ready(Err(error)) => Promise::reject(&error).into(),
            std::task::Poll::Pending => future_to_promise(future).into(),
        }
    }

    /// Stages raw config and runs the source update waterfall before restarting.
    ///
    /// # Errors
    /// Throws synchronous validation or hook failures and rejects disposed Fibers.
    /// A hook may veto or return any value; the default continuation returns a restart Promise.
    pub fn update(
        &self,
        config: &JsValue,
        no_save: &JsValue,
        owner: Option<JsValue>,
    ) -> Result<JsValue, JsValue> {
        if self.inner.is_disposed() {
            return Err(browser_errors::inactive());
        }
        let face = owner.unwrap_or_else(|| self.face.lock().clone().unwrap_or(JsValue::UNDEFINED));
        Reflect::set(&face, &"_config".into(), config)?;
        if Reflect::get(&face, &"state".into())?.as_f64() != Some(2.0) {
            Reflect::set(&face, &"_error".into(), &JsValue::UNDEFINED)?;
            browser_fiber::BrowserLifecycle::request(&face)?;
            return Ok(JsValue::UNDEFINED);
        }
        let config = browser_config::resolve_fiber_config(&self.context(), &face)?;
        let accepted = config.clone();
        let activation = face.clone();
        let next = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
            Reflect::set(&activation, &"config".into(), &accepted)?;
            Reflect::set(&activation, &"_error".into(), &JsValue::UNDEFINED)?;
            Ok(browser_fiber::BrowserLifecycle::restart(&activation)
                .unwrap_or_else(|error| Promise::reject(&error))
                .into())
        }) as Box<dyn Fn() -> Result<JsValue, JsValue>>)
        .into_js_value();
        let no_save = if no_save.is_undefined() {
            JsValue::FALSE
        } else {
            no_save.clone()
        };
        let args = Array::of4(&face, &"internal/update".into(), &config, &no_save);
        args.push(&next);
        required_function(&self.context(), "waterfall")?.apply(&self.context(), &args)
    }

    /// Restarts with the most recent raw config, without native transactional rollback.
    pub fn restart(&self, owner: Option<JsValue>) -> Promise {
        if let Err(error) = self.assert_active() {
            return Promise::reject(&error);
        }
        let face = owner.unwrap_or_else(|| self.face.lock().clone().unwrap_or(JsValue::UNDEFINED));
        browser_fiber::BrowserLifecycle::restart(&face)
            .unwrap_or_else(|error| Promise::reject(&error))
    }
}

fn wrap_fiber(
    raw: &JsValue,
    config: &JsValue,
    native: &Arc<PluginFiber>,
    face: &FaceSlot,
    prototype: Option<Object>,
) -> Result<(JsValue, JsValue), JsValue> {
    let target = prototype.map_or_else(Object::new, |prototype| Object::create(&prototype));
    let context = Reflect::get(raw, &"ctx".into())?;
    // The source returns Object.create(fiber): writes on the awaitable handle may
    // shadow raw fields, while dependency callbacks retain the original Fiber.
    for (name, value) in [
        ("parent", JsValue::UNDEFINED),
        ("inject", JsValue::UNDEFINED),
        ("runtime", JsValue::UNDEFINED),
        ("uid", Reflect::get(raw, &"uid".into())?),
        ("ctx", context.clone()),
        ("config", JsValue::UNDEFINED),
        ("_config", config.clone()),
        ("state", fiber_state_number(native.fiber().state()).into()),
        ("dispose", JsValue::UNDEFINED),
        ("store", JsValue::UNDEFINED),
        ("inertia", JsValue::UNDEFINED),
        ("_hooks", Reflect::get(raw, &"_hooks".into())?),
        ("_disposables", JsValue::UNDEFINED),
        ("context", context),
        ("_error", JsValue::UNDEFINED),
        ("_runner", JsValue::UNDEFINED),
        (
            "_store",
            Object::create(&Object::from(JsValue::NULL)).into(),
        ),
    ] {
        browser_fiber::data_field(&target, name, value)?;
    }
    native
        .fiber()
        .set_browser_lookup(native.fiber().state() == FiberState::Active);
    let updated = Arc::downgrade(native.fiber());
    let core_face = face.clone();
    let assign_state = Closure::wrap(Box::new(
        move |target: JsValue,
              key: JsValue,
              value: JsValue,
              receiver: JsValue|
              -> Result<bool, JsValue> {
            let changed = Reflect::set_with_receiver(&target, &key, &value, &receiver)?;
            if changed
                && key.as_string().as_deref() == Some("state")
                && core_face.lock().as_ref() == Some(&receiver)
                && let Some(native) = updated.upgrade()
            {
                native.set_browser_lookup(value.as_f64() == Some(2.0));
            }
            Ok(changed)
        },
    )
        as Box<dyn Fn(JsValue, JsValue, JsValue, JsValue) -> Result<bool, JsValue>>)
    .into_js_value();
    let core: JsValue = js_sys::Proxy::new(&target, &object(&[("set", assign_state)])?).into();
    browser_fiber_api::bind_backend(&core, raw);
    native.fiber().install_browser_effects(&core)?;
    let wrapped = Object::create(core.unchecked_ref());
    let then = Function::new_with_args(
        "core",
        "return function(fulfilled,rejected) { return core.await().then(fulfilled,rejected); }",
    )
    .call1(&JsValue::UNDEFINED, &core)?;
    set(&wrapped, "then", &then)?;
    Ok((core, wrapped.into()))
}

fn create_fiber_context(
    parent: &JsValue,
    fiber: &JsValue,
    native: Context,
    root: FaceSlot,
    owner: FaceSlot,
) -> Result<JsValue, JsValue> {
    let extension = object(&[("fiber", fiber.clone())])?;
    let child = browser_registry::method(parent, "extend", &Array::of1(&extension))?;
    let known = CONTEXT_CORES.with(|cores| cores.get(child.unchecked_ref::<Object>()));
    if known.is_undefined() || browser_context::contains_extension(&child, &"fiber".into(), fiber)?
    {
        let binding = WasmContext::new(native, child.clone(), root, owner);
        if !known.is_undefined() {
            let source: JsValue = binding.clone_for_binding().into();
            browser_registry::method(&known, "adoptContext", &Array::of1(&source))?;
        }
        bind_context_face(&child, binding);
    }
    Ok(child)
}

fn own_browser_plugin(
    parent: &JsValue,
    native: &Arc<PluginFiber>,
    runtime: &JsValue,
    structural: EffectHandle,
) -> Result<(), JsValue> {
    let owner = native.browser_face();
    let cleanup = provider_disposer(structural);
    let (attached, runtime) = (owner.clone(), runtime.clone());
    let setup = Closure::wrap(Box::new(move || {
        browser_registry::attach(&runtime, &attached)?;
        Ok(cleanup.clone().into())
    }) as Box<dyn Fn() -> Result<JsValue, JsValue>>)
    .into_js_value();
    let dispose = browser_registry::method(
        &Reflect::get(parent, &"fiber".into())?,
        "effect",
        &Array::of2(&setup, &"ctx.plugin()".into()),
    )?;
    browser_fiber::data_field(owner.unchecked_ref(), "dispose", dispose.clone())?;
    if let Some(dispose) = dispose.dyn_ref::<Function>() {
        native.set_browser_disposer(dispose);
    }
    Ok(())
}

pub(crate) fn mark_browser_disposed(owner: &JsValue) -> anyhow::Result<()> {
    if !owner.is_undefined() {
        browser_values::set(owner, &"uid".into(), &JsValue::NULL)
            .map_err(|error| js_anyhow(&error))?;
    }
    Ok(())
}

fn plugin_from_js(
    _descriptor: &JsValue,
    inject: Vec<String>,
    metadata: JsValue,
    root_face: FaceSlot,
    context_face: FaceSlot,
    fiber_face: FaceSlot,
    browser_config: browser_config::BrowserConfig,
) -> Plugin {
    let name = "anonymous";
    Plugin::new(name, inject, move |context, _config| {
        let metadata = metadata.clone();
        let root_face = root_face.clone();
        let context_face = context_face.clone();
        let fiber_face = fiber_face.clone();
        let browser_config = browser_config.clone();
        let fiber = fiber_face.lock().clone().unwrap_or(JsValue::UNDEFINED);
        let receiver = browser_config.activation_face(&fiber);
        Box::pin(async move {
            ensure_context_face(
                context,
                metadata,
                root_face,
                &context_face,
                fiber_face.clone(),
            )?;
            let config = browser_registry::method(
                &receiver,
                "_resolveConfig",
                &Array::of1(
                    &Reflect::get(&receiver, &"_config".into())
                        .map_err(|error| js_anyhow(&error))?,
                ),
            )
            .map_err(|error| js_anyhow(&error))?;
            browser_values::set(&receiver, &"config".into(), &config)
                .map_err(|error| js_anyhow(&error))?;
            let runner = browser_runner::runner(&receiver).map_err(|error| js_anyhow(&error))?;
            let task = browser_registry::method(&receiver, "_execute", &Array::of1(&runner))
                .map_err(|error| js_anyhow(&error))?;
            JsFuture::from(Promise::resolve(&task))
                .await
                .map_err(|error| js_anyhow(&error))?;
            Ok(())
        })
    })
}

fn ensure_context_face(
    context: Context,
    metadata: JsValue,
    root_face: FaceSlot,
    context_face: &FaceSlot,
    fiber_face: FaceSlot,
) -> anyhow::Result<JsValue> {
    if let Some(face) = context_face.lock().clone() {
        return Ok(face);
    }
    let raw = WasmContext::new(
        context,
        Object::create(&Object::from(metadata)).into(),
        root_face,
        fiber_face,
    );
    let face = wrap_context(raw).map_err(|error| js_anyhow(&error))?;
    *context_face.lock() = Some(face.clone());
    Ok(face)
}

fn wrap_detached_context(
    context: Context,
    metadata: JsValue,
    root_face: FaceSlot,
) -> anyhow::Result<JsValue> {
    let owner = context.fiber().browser_context();
    let fiber_face = empty_face_slot();
    if !owner.is_undefined() {
        *fiber_face.lock() =
            Some(Reflect::get(&owner, &"fiber".into()).map_err(|error| js_anyhow(&error))?);
    }
    let parent = if owner.is_undefined() {
        root_face.lock().clone().unwrap_or(metadata)
    } else {
        owner
    };
    wrap_context(WasmContext::new(
        context,
        Object::create(&Object::from(parent)).into(),
        root_face,
        fiber_face,
    ))
    .map_err(|error| js_anyhow(&error))
}

fn wrap_context(context: WasmContext) -> Result<JsValue, JsValue> {
    let binding = context.clone_for_binding();
    let raw: JsValue = context.into();
    CONTEXT_CORES.with(|cores| cores.set(binding.metadata.unchecked_ref::<Object>(), &raw));
    Reflect::set(&raw, &JsValue::from_str("__seekdeepContext"), &raw)?;
    let face = CONTEXT_WRAPPER.with(|configured| {
        configured
            .borrow()
            .as_ref()
            .map_or(Ok(raw.clone()), |wrapper| {
                wrapper.call1(&JsValue::UNDEFINED, &raw)
            })
    })?;
    browser_context::alias_extension(&binding.metadata, &face);
    CONTEXT_CORES.with(|cores| cores.set(face.unchecked_ref::<Object>(), &raw));
    browser_events::remember_context(&face, binding);
    Ok(face)
}

fn bind_context_face(face: &JsValue, binding: WasmContext) {
    let raw: JsValue = binding.clone_for_binding().into();
    CONTEXT_CORES.with(|cores| cores.set(face.unchecked_ref::<Object>(), &raw));
    browser_events::remember_context(face, binding);
}

fn root_fiber_face(context: &JsValue, fiber: &Arc<crate::Fiber>) -> Result<JsValue, JsValue> {
    browser_fiber_api::root_face(
        context,
        fiber,
        Object::new().into(),
        Object::create(&Object::from(JsValue::NULL)).into(),
        &JsValue::NULL,
        None,
        &Function::new_no_args("return [];"),
    )
}

fn context_effect(
    owner: JsValue,
    setup: &JsValue,
    label: JsValue,
    outer: JsValue,
) -> Result<Function, JsValue> {
    browser_registry::method(&owner, "assertActive", &Array::new())?;
    if Reflect::get(&owner, &"state".into())?.as_f64() == Some(5.0) {
        return Err(browser_errors::inactive());
    }
    browser_effects::effect_on_owner(
        owner,
        setup,
        if label.is_undefined() {
            "anonymous".into()
        } else {
            label
        },
        outer,
    )
}

fn event_args_from_js(args: &Array) -> EventArgs {
    EventArgs::from_values(
        args.iter()
            .map(|value| Arc::new(value) as EventValue)
            .collect(),
    )
}

fn event_args_to_js(args: &EventArgs) -> Array {
    let output = Array::new();
    for index in 0..args.len() {
        if let Some(value) = args.get::<JsValue>(index) {
            output.push(&value);
        } else if let Some(fiber) = args.get::<PluginFiber>(index) {
            output.push(&fiber.browser_face());
        } else {
            output.push(&JsValue::UNDEFINED);
        }
    }
    output
}

fn event_reply_from_js(value: JsValue) -> EventReply {
    if value.is_undefined() {
        EventReply::Undefined
    } else if value.is_null() {
        EventReply::Null
    } else if value.as_bool() == Some(false) {
        EventReply::False
    } else {
        EventReply::Value(Arc::new(value))
    }
}

fn event_reply_to_js(reply: EventReply) -> JsValue {
    match reply {
        EventReply::Undefined => JsValue::UNDEFINED,
        EventReply::Null => JsValue::NULL,
        EventReply::False => JsValue::FALSE,
        EventReply::Value(value) => Arc::downcast::<JsValue>(value)
            .map(|value| (*value).clone())
            .unwrap_or(JsValue::UNDEFINED),
    }
}

fn effect_disposer(effect: EffectHandle) -> Function {
    effect
        .browser_disposer()
        .unwrap_or_else(|| native_effect_disposer(effect))
}

fn native_effect_disposer(effect: EffectHandle) -> Function {
    let called = Cell::new(false);
    let closure = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if called.replace(true) {
            return Ok(JsValue::UNDEFINED);
        }
        let effect = effect.clone();
        let mut disposal = Box::pin(async move {
            effect
                .dispose()
                .await
                .map_err(|error| js_cause(&error).unwrap_or_else(|| js_error(error)))?;
            Ok(JsValue::UNDEFINED)
        });
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        match std::future::Future::poll(disposal.as_mut(), &mut context) {
            std::task::Poll::Ready(result) => result,
            std::task::Poll::Pending => Ok(future_to_promise(disposal).into()),
        }
    }) as Box<dyn Fn() -> Result<JsValue, JsValue>>);
    closure.into_js_value().unchecked_into()
}

fn fiber_state_number(state: FiberState) -> u8 {
    match state {
        FiberState::Pending => 0,
        FiberState::Loading => 1,
        FiberState::Active => 2,
        FiberState::Failed => 3,
        FiberState::Disposed => 4,
        FiberState::Unloading => 5,
    }
}

fn empty_face_slot() -> FaceSlot {
    Arc::new(Mutex::new(None))
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        browser_values::define_data(&object, &JsValue::from_str(key), value)?;
    }
    Ok(object)
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(object, &JsValue::from_str(key), value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new(&format!("failed to set Cordis member {key:?}")).into())
    }
}

fn required_function(value: &JsValue, key: &str) -> Result<Function, JsValue> {
    Reflect::get(value, &JsValue::from_str(key))?
        .dyn_into::<Function>()
        .map_err(|_| js_sys::Error::new(&format!("Cordis value omitted function {key:?}")).into())
}

#[derive(Debug)]
struct BrowserException {
    value: JsValue,
    message: String,
}

impl std::fmt::Display for BrowserException {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for BrowserException {}

pub(crate) fn js_cause(error: &anyhow::Error) -> Option<JsValue> {
    error
        .downcast_ref::<BrowserException>()
        .map(|error| error.value.clone())
}

pub(crate) fn js_anyhow(error: &JsValue) -> anyhow::Error {
    let message = Reflect::get(error, &JsValue::from_str("message"))
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_else(|| format!("{error:?}"));
    anyhow::Error::new(BrowserException {
        value: error.clone(),
        message,
    })
}

fn js_error(error: impl std::fmt::Display) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}
