//! JavaScript object bindings over the Rust-owned browser Cordis core.

use std::{cell::RefCell, sync::Arc};

use js_sys::{Array, Function, Object, Promise, Reflect};
use parking_lot::Mutex;
use serde_json::Value;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use crate::{
    BailReply, Context, EventArgs, EventOptions, EventReply, EventValue, FiberState, Plugin,
    PluginFiber, fiber::EffectHandle,
};

thread_local! {
    static CONTEXT_WRAPPER: RefCell<Option<Function>> = const { RefCell::new(None) };
}

type FaceSlot = Arc<Mutex<Option<JsValue>>>;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = Reflect, js_name = get, catch)]
    fn get_with_receiver(
        target: &JsValue,
        key: &JsValue,
        receiver: &JsValue,
    ) -> Result<JsValue, JsValue>;
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

/// Creates one root browser Context backed by the portable Rust core.
///
/// # Errors
///
/// Returns JavaScript wrapper construction failures.
#[wasm_bindgen(js_name = createContext)]
pub fn create_context() -> Result<JsValue, JsValue> {
    let root_face = empty_face_slot();
    let fiber_face = empty_face_slot();
    let context = WasmContext::new(
        Context::new(),
        Object::new().into(),
        root_face.clone(),
        fiber_face.clone(),
    );
    let root_fiber = context.inner.fiber().clone();
    let face = wrap_context(context)?;
    *root_face.lock() = Some(face.clone());
    *fiber_face.lock() = Some(root_fiber_face(&face, root_fiber)?);
    Ok(face)
}

/// Browser Context handle. The ESM package wraps this class in a Proxy whose
/// string-property fallback calls [`WasmContext::get`].
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
}

#[wasm_bindgen]
impl WasmContext {
    /// Reads a service without resolving mixed-in or reflected properties.
    #[wasm_bindgen(js_name = serviceGet)]
    pub fn service_get(&self, name: &str, strict: &JsValue) -> JsValue {
        let value = if strict.is_undefined() || strict.is_truthy() {
            self.inner.get_named::<JsValue>(name)
        } else {
            self.inner.get_named_relaxed::<JsValue>(name)
        };
        value.as_deref().cloned().unwrap_or(JsValue::UNDEFINED)
    }

    /// Reads a reflected service or accessor by runtime name.
    ///
    /// # Errors
    ///
    /// Returns a reflected accessor failure.
    #[allow(clippy::needless_pass_by_value)]
    pub fn get(&self, name: String) -> Result<JsValue, JsValue> {
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
        let effect = self
            .inner
            .provide_named(&name, Arc::new(value))
            .map_err(|error| js_sys::Error::new(&error.to_string()))?;
        Ok(effect_disposer(effect))
    }

    /// Writes a reflected accessor or provider-owned service.
    ///
    /// # Errors
    ///
    /// Returns missing-provider, ownership, or setter failures.
    #[wasm_bindgen(js_name = setProperty)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn set_property(&self, name: String, value: JsValue) -> Result<bool, JsValue> {
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
        let metadata = parent.unwrap_or_else(|| self.metadata.clone());
        let context_face = empty_face_slot();
        let fiber_face = empty_face_slot();
        let plugin = plugin_from_js(
            descriptor,
            metadata.clone(),
            self.root_face.clone(),
            context_face.clone(),
            fiber_face.clone(),
        )?;
        let config = config_from_js(config)?;
        let mounted = self
            .inner
            .plugin(plugin, config)
            .map_err(|error| js_sys::Error::new(&error.to_string()))?;
        let child = ensure_context_face(
            mounted.context().clone(),
            metadata,
            self.root_face.clone(),
            &context_face,
            fiber_face.clone(),
        )
        .map_err(js_error)?;
        let fiber: JsValue = WasmFiber::new(mounted, child).into();
        *fiber_face.lock() = Some(fiber.clone());
        Ok(fiber)
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
        name: String,
        listener: JsValue,
        options: JsValue,
    ) -> Result<Function, JsValue> {
        let listener = listener
            .dyn_into::<Function>()
            .map_err(|_| js_sys::Error::new("ctx.on listener must be a function"))?;
        let registration = event_options(&options)?;
        let once = bool_property(&options, "once")?;
        let root_face = self.root_face.clone();
        let metadata = self.metadata.clone();
        let callback = move |context: Context, args: EventArgs| {
            let listener = listener.clone();
            let root_face = root_face.clone();
            let metadata = metadata.clone();
            Box::pin(async move {
                let this = wrap_detached_context(context, metadata, root_face)?;
                let returned = listener
                    .apply(&this, &event_args_to_js(&args))
                    .map_err(|error| js_anyhow(&error))?;
                let settled = JsFuture::from(Promise::resolve(&returned))
                    .await
                    .map_err(|error| js_anyhow(&error))?;
                Ok(event_reply_from_js(settled))
            }) as crate::events::ListenerFuture
        };
        let effect = if once {
            self.inner
                .events()
                .once(&self.inner, name, callback, registration)
        } else {
            self.inner
                .events()
                .on(&self.inner, name, callback, registration)
        }
        .map_err(|error| js_sys::Error::new(&error.to_string()))?;
        Ok(effect_disposer(effect))
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

    /// Registers an arbitrary setup result in the current Fiber ledger.
    ///
    /// # Errors
    ///
    /// Returns setup or inactive-Fiber failures.
    #[allow(clippy::needless_pass_by_value)]
    pub fn effect(&self, setup: JsValue, label: Option<String>) -> Result<Function, JsValue> {
        if matches!(
            self.inner.fiber().state(),
            FiberState::Unloading | FiberState::Disposed
        ) {
            return Err(js_sys::Error::new(&crate::CordisError::InactiveEffect.to_string()).into());
        }
        let setup = setup
            .dyn_into::<Function>()
            .map_err(|_| js_sys::Error::new("ctx.effect setup must be a function"))?;
        let result = setup.call0(&JsValue::UNDEFINED)?;
        let effect = js_disposal_effect(label.unwrap_or_else(|| "ctx.effect()".to_owned()), result);
        self.inner
            .own(effect.clone())
            .map_err(|error| js_sys::Error::new(&error.to_string()))?;
        Ok(effect_disposer(effect))
    }

    /// Creates a metadata-bearing child context with the same lifecycle owner.
    ///
    /// # Errors
    ///
    /// Returns malformed metadata or wrapper failures.
    #[allow(clippy::needless_pass_by_value)]
    pub fn extend(&self, extension: JsValue, parent: Option<JsValue>) -> Result<JsValue, JsValue> {
        let extension = if extension.is_undefined() {
            Object::new().into()
        } else {
            extension
        };
        if !extension.is_object() || extension.is_null() {
            return Err(js_sys::Error::new("ctx.extend metadata must be an object").into());
        }
        let metadata = Object::create(&Object::from(
            parent.unwrap_or_else(|| self.metadata.clone()),
        ));
        for key in Reflect::own_keys(&extension)? {
            let descriptor =
                Reflect::get_own_property_descriptor(&Object::from(extension.clone()), &key)?;
            Reflect::define_property(&metadata, &key, &Object::from(descriptor))?;
        }
        wrap_context(self.child(self.inner.clone(), metadata.into(), self.fiber_face.clone()))
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
        label: Option<String>,
        parent: Option<JsValue>,
    ) -> Result<JsValue, JsValue> {
        let inner = label.map_or_else(
            || self.inner.isolate_named(&name),
            |label| self.inner.isolate_named_as(&name, &label),
        );
        let metadata = Object::create(&Object::from(
            parent.unwrap_or_else(|| self.metadata.clone()),
        ));
        wrap_context(self.child(inner, metadata.into(), self.fiber_face.clone()))
    }

    /// Creates a child carrying one service intercept.
    ///
    /// # Errors
    ///
    /// Returns non-JSON config or wrapper failures.
    #[allow(clippy::needless_pass_by_value)]
    pub fn intercept(
        &self,
        name: String,
        config: JsValue,
        parent: Option<JsValue>,
    ) -> Result<JsValue, JsValue> {
        let config = serde_wasm_bindgen::from_value(config)
            .map_err(|error| js_sys::Error::new(&error.to_string()))?;
        wrap_context(
            self.child(
                self.inner.intercept(&name, config),
                Object::create(&Object::from(
                    parent.unwrap_or_else(|| self.metadata.clone()),
                ))
                .into(),
                self.fiber_face.clone(),
            ),
        )
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
        get_with_receiver(
            &self.metadata,
            &key,
            &receiver.unwrap_or_else(|| self.metadata.clone()),
        )
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
    #[wasm_bindgen(js_name = propertyDefined)]
    pub fn property_defined(&self, name: &str) -> bool {
        self.inner.has_property(name)
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
        let context = self.inner.clone();
        let provide = Closure::wrap(Box::new(
            move |name: String, value: JsValue, _check: JsValue| -> Result<Function, JsValue> {
                let effect = context
                    .provide_named(&name, Arc::new(value))
                    .map_err(|error| js_sys::Error::new(&error.to_string()))?;
                Ok(effect_disposer(effect))
            },
        )
            as Box<dyn FnMut(String, JsValue, JsValue) -> Result<Function, JsValue>>);
        object(&[("provide", provide.into_js_value())]).map(Into::into)
    }

    /// Registry compatibility face. Lifecycle still runs in the Rust registry.
    ///
    /// # Errors
    ///
    /// Returns JavaScript object-construction failures.
    #[wasm_bindgen(getter)]
    pub fn registry(&self) -> Result<JsValue, JsValue> {
        let context = self.clone_for_binding();
        let plugin = Closure::wrap(Box::new(move |descriptor: JsValue, config: JsValue| {
            context.plugin(descriptor, config, None)
        })
            as Box<dyn FnMut(JsValue, JsValue) -> Result<JsValue, JsValue>>);
        object(&[("plugin", plugin.into_js_value())]).map(Into::into)
    }

    /// Event compatibility face; event methods are already mixed onto Context.
    #[wasm_bindgen(getter)]
    pub fn events(&self) -> JsValue {
        self.root()
    }
}

impl WasmContext {
    fn clone_for_binding(&self) -> Self {
        Self::new(
            self.inner.clone(),
            self.metadata.clone(),
            self.root_face.clone(),
            self.fiber_face.clone(),
        )
    }
}

/// JavaScript Fiber face backed by one exact Rust plugin generation.
#[wasm_bindgen]
pub struct WasmFiber {
    inner: Arc<PluginFiber>,
    context: JsValue,
    entry: FaceSlot,
}

impl WasmFiber {
    fn new(inner: Arc<PluginFiber>, context: JsValue) -> Self {
        Self {
            inner,
            context,
            entry: empty_face_slot(),
        }
    }
}

#[wasm_bindgen]
impl WasmFiber {
    /// Plugin-scoped Context.
    #[wasm_bindgen(getter, js_name = ctx)]
    pub fn context(&self) -> JsValue {
        self.context.clone()
    }

    /// Source-compatible numeric lifecycle state.
    #[wasm_bindgen(getter)]
    pub fn state(&self) -> u8 {
        fiber_state_number(self.inner.fiber().state())
    }

    /// Monotonic runtime identity; null after disposal.
    #[wasm_bindgen(getter)]
    #[allow(clippy::cast_precision_loss)]
    pub fn uid(&self) -> JsValue {
        self.inner
            .uid()
            .map_or(JsValue::NULL, |uid| JsValue::from_f64(uid as f64))
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
    pub fn wait(&self) -> Promise {
        let fiber = self.inner.clone();
        future_to_promise(async move {
            fiber.await_settled().await.map_err(js_error)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `PromiseLike` bridge used by `await ctx.plugin(...)`.
    ///
    /// # Errors
    ///
    /// Returns JavaScript invocation failures.
    #[allow(clippy::needless_pass_by_value)]
    pub fn then(&self, fulfilled: Function, rejected: Function) -> Result<Promise, JsValue> {
        let promise = self.wait();
        required_function(promise.as_ref(), "then")?
            .call2(&promise, &fulfilled, &rejected)?
            .dyn_into::<Promise>()
    }

    /// Permanently disposes this plugin generation.
    pub fn dispose(&self) -> Promise {
        let fiber = self.inner.clone();
        future_to_promise(async move {
            fiber.dispose().await.map_err(js_error)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Transactionally replaces raw plugin configuration.
    #[allow(clippy::needless_pass_by_value)]
    pub fn update(&self, config: JsValue) -> Promise {
        let fiber = self.inner.clone();
        future_to_promise(async move {
            let config = config_from_js(config)?;
            fiber.update(config).await.map_err(js_error)?;
            Ok(JsValue::UNDEFINED)
        })
    }
}

fn plugin_from_js(
    descriptor: JsValue,
    metadata: JsValue,
    root_face: FaceSlot,
    context_face: FaceSlot,
    fiber_face: FaceSlot,
) -> Result<Plugin, JsValue> {
    let name = string_property(&descriptor, "name")?.unwrap_or_else(|| "anonymous".to_owned());
    let diagnostic_name = name.clone();
    let inject = inject_names(&Reflect::get(&descriptor, &JsValue::from_str("inject"))?)?;
    let callback_descriptor = descriptor;
    Ok(Plugin::new(name, inject, move |context, config| {
        let descriptor = callback_descriptor.clone();
        let metadata = metadata.clone();
        let root_face = root_face.clone();
        let context_face = context_face.clone();
        let fiber_face = fiber_face.clone();
        let diagnostic_name = diagnostic_name.clone();
        Box::pin(async move {
            let owner = context.clone();
            let face =
                ensure_context_face(context, metadata, root_face, &context_face, fiber_face)?;
            let config = serde_wasm_bindgen::to_value(&config)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            let returned = if let Some(function) = descriptor.dyn_ref::<Function>() {
                function
                    .call2(&JsValue::UNDEFINED, &face, &config)
                    .map_err(|error| {
                        anyhow::anyhow!(
                            "browser plugin {diagnostic_name:?} apply failed: {}",
                            js_anyhow(&error)
                        )
                    })?
            } else {
                let apply = required_function(&descriptor, "apply").map_err(|error| {
                    anyhow::anyhow!(
                        "browser plugin {diagnostic_name:?} apply resolution failed: {}",
                        js_anyhow(&error)
                    )
                })?;
                apply.call2(&descriptor, &face, &config).map_err(|error| {
                    anyhow::anyhow!(
                        "browser plugin {diagnostic_name:?} apply failed: {}",
                        js_anyhow(&error)
                    )
                })?
            };
            let returned = JsFuture::from(Promise::resolve(&returned))
                .await
                .map_err(|error| {
                    anyhow::anyhow!(
                        "browser plugin {diagnostic_name:?} async apply failed: {}",
                        js_anyhow(&error)
                    )
                })?;
            if returned.is_function() {
                let effect = js_disposal_effect("plugin return disposer".to_owned(), returned);
                owner
                    .own(effect)
                    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            } else if !returned.is_undefined() && !returned.is_null() {
                return Err(anyhow::anyhow!(
                    "browser plugin {diagnostic_name:?} returned an unsupported effect value"
                ));
            }
            Ok(())
        })
    }))
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
    wrap_context(WasmContext::new(
        context,
        Object::create(&Object::from(metadata)).into(),
        root_face,
        empty_face_slot(),
    ))
    .map_err(|error| js_anyhow(&error))
}

fn wrap_context(context: WasmContext) -> Result<JsValue, JsValue> {
    let raw: JsValue = context.into();
    Reflect::set(&raw, &JsValue::from_str("__seekdeepContext"), &raw)?;
    CONTEXT_WRAPPER.with(|configured| {
        configured
            .borrow()
            .as_ref()
            .map_or(Ok(raw.clone()), |wrapper| {
                wrapper.call1(&JsValue::UNDEFINED, &raw)
            })
    })
}

fn root_fiber_face(context: &JsValue, fiber: Arc<crate::Fiber>) -> Result<JsValue, JsValue> {
    let face = Object::new();
    set(&face, "uid", &JsValue::from_f64(0.0))?;
    set(&face, "state", &JsValue::from_f64(2.0))?;
    set(&face, "ctx", context)?;
    let dispose = Closure::wrap(Box::new(move || -> Promise {
        let fiber = fiber.clone();
        future_to_promise(async move {
            fiber
                .dispose()
                .await
                .map(|()| JsValue::UNDEFINED)
                .map_err(|error| js_sys::Error::new(&error.to_string()).into())
        })
    }) as Box<dyn FnMut() -> Promise>);
    set(&face, "dispose", &dispose.into_js_value())?;
    Ok(face.into())
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
        output.push(
            args.get::<JsValue>(index)
                .as_deref()
                .unwrap_or(&JsValue::UNDEFINED),
        );
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

fn js_disposal_effect(label: String, result: JsValue) -> EffectHandle {
    EffectHandle::new(label, move || {
        Box::pin(async move {
            let result = if result.is_function() || result.is_undefined() || result.is_null() {
                result
            } else {
                JsFuture::from(Promise::resolve(&result))
                    .await
                    .map_err(|error| js_anyhow(&error))?
            };
            if result.is_undefined() || result.is_null() {
                return Ok(());
            }
            let disposer = result
                .dyn_into::<Function>()
                .map_err(|_| anyhow::anyhow!("effect result must resolve to a disposer"))?;
            let returned = disposer
                .call0(&JsValue::UNDEFINED)
                .map_err(|error| js_anyhow(&error))?;
            if returned.is_object() || returned.is_function() {
                JsFuture::from(Promise::resolve(&returned))
                    .await
                    .map_err(|error| js_anyhow(&error))?;
            }
            Ok(())
        })
    })
}

fn effect_disposer(effect: EffectHandle) -> Function {
    let closure = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let effect = effect.clone();
        let mut disposal = Box::pin(async move {
            effect.dispose().await.map_err(js_error)?;
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

fn config_from_js(value: JsValue) -> Result<Value, JsValue> {
    if value.is_undefined() {
        return Ok(Value::Null);
    }
    serde_wasm_bindgen::from_value(value)
        .map_err(|error| js_sys::Error::new(&format!("plugin config is not JSON: {error}")).into())
}

fn inject_names(value: &JsValue) -> Result<Vec<String>, JsValue> {
    if value.is_undefined() || value.is_null() {
        return Ok(Vec::new());
    }
    if Array::is_array(value) {
        return Array::from(value)
            .iter()
            .map(|value| {
                value.as_string().ok_or_else(|| {
                    js_sys::Error::new("plugin inject values must be strings").into()
                })
            })
            .collect();
    }
    if value.is_object() {
        return Ok(Object::keys(&Object::from(value.clone()))
            .iter()
            .filter_map(|value| value.as_string())
            .collect());
    }
    Err(js_sys::Error::new("plugin inject must be an array or object").into())
}

fn event_options(value: &JsValue) -> Result<EventOptions, JsValue> {
    Ok(EventOptions {
        prepend: bool_property(value, "prepend")?,
        global: bool_property(value, "global")?,
    })
}

fn bool_property(value: &JsValue, key: &str) -> Result<bool, JsValue> {
    if !value.is_object() || value.is_null() {
        return Ok(false);
    }
    Ok(Reflect::get(value, &JsValue::from_str(key))?
        .as_bool()
        .unwrap_or(false))
}

fn string_property(value: &JsValue, key: &str) -> Result<Option<String>, JsValue> {
    let value = Reflect::get(value, &JsValue::from_str(key))?;
    Ok((!value.is_undefined() && !value.is_null())
        .then(|| value.as_string())
        .flatten())
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
        set(&object, key, value)?;
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

fn js_anyhow(error: &JsValue) -> anyhow::Error {
    let message = Reflect::get(error, &JsValue::from_str("message"))
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_else(|| format!("{error:?}"));
    anyhow::anyhow!(message)
}

fn js_error(error: impl std::fmt::Display) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}
