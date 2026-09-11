//! Receiver-owned browser transitions share the Fiber's epoch and effect ledger.

use std::sync::{Arc, Weak};

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use super::browser_registry::method;
use super::{
    FaceSlot, FiberState, PluginFiber, fiber_state_number, js_anyhow, js_cause, js_error,
    required_function,
};
use wasm_bindgen::closure::Closure;

#[derive(Clone)]
pub(super) struct BrowserLifecycle {
    native: Weak<PluginFiber>,
    root: Option<Arc<crate::Fiber>>,
    core: FaceSlot,
    activation: FaceSlot,
}

impl BrowserLifecycle {
    pub(super) fn detached(owner: &JsValue) -> Self {
        let core = super::empty_face_slot();
        *core.lock() = Some(owner.clone());
        Self {
            native: Weak::new(),
            root: None,
            core,
            activation: super::empty_face_slot(),
        }
    }

    pub(super) fn new(native: &Arc<PluginFiber>, core: FaceSlot, activation: FaceSlot) -> Self {
        Self {
            native: Arc::downgrade(native),
            root: None,
            core,
            activation,
        }
    }

    pub(super) fn new_root(native: Arc<crate::Fiber>, core: FaceSlot) -> Self {
        Self {
            native: Weak::new(),
            root: Some(native),
            core,
            activation: super::empty_face_slot(),
        }
    }

    fn native_fiber(&self) -> Option<Arc<crate::Fiber>> {
        self.native
            .upgrade()
            .map(|native| native.fiber().clone())
            .or_else(|| self.root.clone())
    }

    fn core(&self) -> JsValue {
        self.core.lock().clone().unwrap_or(JsValue::UNDEFINED)
    }

    pub(super) fn prepare(&self, _explicit: bool) {
        if let Some(native) = self.native.upgrade() {
            if native.is_disposed() {
                return;
            }
            match super::browser_registry::tracks(&self.core()) {
                Ok(true) => {}
                Ok(false) => return,
                Err(error) => {
                    tracing::error!(?error, "browser registry traversal failed");
                    return;
                }
            }
            if let Err(error) = Self::check_dependencies(&self.core()).and_then(|()| {
                super::browser_registry::method(&self.core(), "_refresh", &Array::new()).map(|_| ())
            }) {
                tracing::error!(?error, "browser dependency transition failed");
            }
        }
    }

    pub(super) fn initial(&self) -> Result<(), JsValue> {
        Self::check_dependencies(&self.core())?;
        super::browser_registry::method(&self.core(), "_refresh", &Array::new()).map(|_| ())
    }

    fn check_dependencies(owner: &JsValue) -> Result<(), JsValue> {
        let inject = Reflect::get(owner, &"inject".into())?;
        for name in Object::keys(inject.unchecked_ref::<Object>()).iter() {
            super::browser_registry::method(owner, "_checkImpl", &Array::of1(&name))?;
        }
        Ok(())
    }

    pub(super) fn check_implementation(
        owner: &JsValue,
        name: &JsValue,
    ) -> Result<JsValue, JsValue> {
        let context = Reflect::get(owner, &"ctx".into())?;
        let reflection = Reflect::get(&context, &"reflect".into())?;
        let implementation = super::browser_registry::method(
            &reflection,
            "_getImpl",
            &Array::of2(name, &JsValue::TRUE),
        )?;
        let mut available = implementation.is_truthy();
        if available {
            let checked = (|| -> Result<bool, JsValue> {
                if !Reflect::get(&implementation, &"check".into())?.is_truthy() {
                    return Ok(true);
                }
                let check = Reflect::get(&implementation, &"check".into())?;
                let value = Reflect::get(&implementation, &"value".into())?;
                let context = Reflect::get(owner, &"ctx".into())?;
                let receiver = super::tracing::Tracer::new(context).trace(&value)?;
                super::browser_registry::method(&check, "call", &Array::of1(&receiver))
                    .map(|value| value.is_truthy())
            })();
            available = match checked {
                Ok(available) => available,
                Err(error) => {
                    let provider = Reflect::get(&implementation, &"fiber".into())?;
                    super::browser_logger::log_error(
                        &Reflect::get(&provider, &"ctx".into())?,
                        &error,
                    )?;
                    false
                }
            };
        }
        let store = Reflect::get(owner, &"_store".into())?;
        if available {
            super::browser_values::set(&store, name, &implementation)?;
        } else {
            if Reflect::delete_property(store.unchecked_ref::<Object>(), name)? {
                return Ok(JsValue::TRUE);
            }
            return Err(js_sys::TypeError::new("Cannot delete property").into());
        }
        Ok(JsValue::UNDEFINED)
    }

    pub(super) fn phase(&self, state: FiberState) {
        if let Err(error) = self.set_state(&self.core(), state) {
            tracing::error!(?error, "browser Fiber status observer failed");
        }
    }

    fn set_state(&self, owner: &JsValue, state: FiberState) -> Result<(), JsValue> {
        let state_value = JsValue::from(fiber_state_number(state));
        let update =
            Closure::wrap(Box::new(move || state_value.clone()) as Box<dyn Fn() -> JsValue>)
                .into_js_value();
        self.update_state(owner, &update)
    }

    fn update_state(&self, owner: &JsValue, callback: &JsValue) -> Result<(), JsValue> {
        method(owner, "_updateState", &Array::of1(callback))?;
        self.synchronize_state(owner, &Reflect::get(owner, &"state".into())?);
        Ok(())
    }

    pub(super) fn synchronize_state(&self, owner: &JsValue, state: &JsValue) {
        let core = self.core();
        if *owner == core
            && let Some(native) = self.native_fiber()
        {
            let state = state.as_f64();
            native.set_browser_lookup(state == Some(2.0));
            let state = match state {
                Some(0.0) => Some(FiberState::Pending),
                Some(1.0) => Some(FiberState::Loading),
                Some(2.0) => Some(FiberState::Active),
                Some(3.0) => Some(FiberState::Failed),
                Some(4.0) => Some(FiberState::Disposed),
                Some(5.0) => Some(FiberState::Unloading),
                _ => None,
            };
            if let Some(state) = state {
                native.set_browser_state(state);
            }
        }
    }

    pub(super) fn refresh(owner: &JsValue) -> Result<(), JsValue> {
        let inject = Reflect::get(owner, &"inject".into())?;
        let mut epoch = String::new();
        for key in Object::keys(&Object::from(inject)).iter() {
            let store = Reflect::get(owner, &"_store".into())?;
            let implementation = Reflect::get(&store, &key)?;
            if !implementation.is_truthy() {
                return method(
                    owner,
                    "_setEpoch",
                    &Array::of1(&super::browser_runner::INACTIVE.into()),
                )
                .map(|_| ());
            }
            let fiber = Reflect::get(&implementation, &"fiber".into())?;
            let uid = Reflect::get(&fiber, &"uid".into())?;
            let text = Function::new_with_args("value", "return ':' + value;")
                .call1(&JsValue::UNDEFINED, &uid)?;
            epoch.push_str(
                &text
                    .as_string()
                    .ok_or_else(|| js_sys::TypeError::new("Fiber uid did not convert to text"))?,
            );
        }
        method(owner, "_setEpoch", &Array::of1(&epoch.into())).map(|_| ())
    }

    pub(super) fn set_epoch_value(&self, owner: &JsValue, epoch: &JsValue) -> Result<(), JsValue> {
        let previous = super::browser_runner::epoch(owner)?;
        if previous == *epoch {
            return Ok(());
        }
        super::browser_values::set(
            &super::browser_runner::runner(owner)?,
            &"epoch".into(),
            epoch,
        )?;
        let active = super::browser_runner::is_active(epoch);
        if Reflect::get(owner, &"inertia".into())?.is_truthy() {
            return Ok(());
        }
        let (operation, state) = if active && !super::browser_runner::is_active(&previous) {
            ("_reload", 1)
        } else {
            ("_unload", 5)
        };
        self.update_state(owner, &transition(owner.clone(), operation, state))
    }

    pub(super) fn reload(&self, owner: JsValue) -> Result<Promise, JsValue> {
        let stored = Reflect::get(&owner, &"_store".into())?;
        let store = Object::new();
        super::browser_values::spread_into(&store, &stored)?;
        super::browser_values::set(&owner, &"store".into(), &store)?;
        let epoch = super::browser_runner::epoch(&owner)?;
        let lifecycle = self.clone();
        Ok(future_to_promise(async move {
            let current = super::browser_runner::epoch(&owner)?;
            if current == epoch {
                *lifecycle.activation.lock() = Some(owner.clone());
                let result = if let Some(native) = lifecycle.native.upgrade() {
                    native.run_browser_startup().await
                } else {
                    root_startup(&owner)
                        .await
                        .map_err(|error| js_anyhow(&error))
                };
                match result {
                    Ok(()) => {
                        super::browser_values::set(&owner, &"_error".into(), &JsValue::UNDEFINED)?;
                    }
                    Err(error) => {
                        let error = js_cause(&error).unwrap_or_else(|| js_error(&error));
                        super::browser_logger::log_error(
                            &Reflect::get(&owner, &"ctx".into())?,
                            &error,
                        )?;
                        super::browser_values::set(&owner, &"_error".into(), &error)?;
                        super::browser_values::set(
                            &super::browser_runner::runner(&owner)?,
                            &"epoch".into(),
                            &super::browser_runner::INACTIVE.into(),
                        )?;
                    }
                }
            }
            let updated = owner.clone();
            let update = Closure::wrap(Box::new(move || {
                if super::browser_runner::epoch(&updated)? == epoch {
                    super::browser_values::set(&updated, &"inertia".into(), &JsValue::UNDEFINED)?;
                    Ok(JsValue::UNDEFINED)
                } else {
                    invoke_transition(&updated, "_unload", 5)
                }
            }) as Box<dyn Fn() -> Result<JsValue, JsValue>>)
            .into_js_value();
            lifecycle.update_state(&owner, &update)?;
            Ok(JsValue::UNDEFINED)
        }))
    }

    pub(super) fn unload(&self, owner: JsValue) -> Promise {
        let settled = match super::browser_effects::unload(&owner) {
            Ok(settled) => settled,
            Err(error) => return Promise::reject(&error),
        };
        let lifecycle = self.clone();
        future_to_promise(async move {
            JsFuture::from(settled).await?;
            super::browser_values::set(&owner, &"store".into(), &JsValue::UNDEFINED)?;
            let updated = owner.clone();
            let update = Closure::wrap(Box::new(move || {
                if super::browser_runner::is_active(&super::browser_runner::epoch(&updated)?) {
                    invoke_transition(&updated, "_reload", 1)
                } else {
                    super::browser_values::set(&updated, &"inertia".into(), &JsValue::UNDEFINED)?;
                    Ok(JsValue::UNDEFINED)
                }
            }) as Box<dyn Fn() -> Result<JsValue, JsValue>>)
            .into_js_value();
            lifecycle.update_state(&owner, &update)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub(super) fn wait(owner: JsValue) -> Promise {
        future_to_promise(async move {
            wait_inertia(&owner).await?;
            let error = Reflect::get(&owner, &"_error".into())?;
            if error.is_truthy() {
                return Err(error);
            }
            Ok(owner)
        })
    }

    pub(super) fn request(owner: &JsValue) -> Result<(), JsValue> {
        method(
            owner,
            "_setEpoch",
            &Array::of1(&super::browser_runner::INACTIVE.into()),
        )?;
        method(owner, "_refresh", &Array::new()).map(|_| ())
    }

    pub(super) fn update(
        owner: &JsValue,
        config: &JsValue,
        no_save: &JsValue,
    ) -> Result<JsValue, JsValue> {
        method(owner, "assertActive", &Array::new())?;
        super::browser_values::set(owner, &"_config".into(), config)?;
        if Reflect::get(owner, &"state".into())?.as_f64() != Some(2.0) {
            super::browser_values::set(owner, &"_error".into(), &JsValue::UNDEFINED)?;
            Self::request(owner)?;
            return Ok(JsValue::UNDEFINED);
        }
        let config = method(owner, "_resolveConfig", &Array::of1(config))?;
        let accepted = config.clone();
        let receiver = owner.clone();
        let next =
            wasm_bindgen::closure::Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
                super::browser_values::set(&receiver, &"config".into(), &accepted)?;
                super::browser_values::set(&receiver, &"_error".into(), &JsValue::UNDEFINED)?;
                method(&receiver, "restart", &Array::new())
            })
                as Box<dyn Fn() -> Result<JsValue, JsValue>>)
            .into_js_value();
        let no_save = if no_save.is_undefined() {
            JsValue::FALSE
        } else {
            no_save.clone()
        };
        let args = Array::of4(owner, &"internal/update".into(), &config, &no_save);
        args.push(&next);
        method(&Reflect::get(owner, &"context".into())?, "waterfall", &args)
    }

    pub(super) fn restart(owner: &JsValue) -> Result<Promise, JsValue> {
        method(owner, "assertActive", &Array::new())?;
        Self::request(owner)?;
        let waiting = Promise::resolve(&method(owner, "await", &Array::new())?);
        let done = wasm_bindgen::closure::Closure::wrap(
            Box::new(|_: JsValue| JsValue::UNDEFINED) as Box<dyn Fn(JsValue) -> JsValue>
        )
        .into_js_value();
        required_function(&waiting, "then")?
            .call1(&waiting, &done)
            .map(wasm_bindgen::JsCast::unchecked_into)
    }

    pub(super) fn dispose(&self) -> crate::fiber::DisposeFuture {
        let result = (|| -> Result<JsValue, JsValue> {
            let core = self.core();
            super::browser_registry::withdraw(&core)?;
            method(
                &core,
                "_setEpoch",
                &Array::of1(&super::browser_runner::INACTIVE.into()),
            )?;
            if !Reflect::get(&core, &"inertia".into())?.is_truthy() {
                self.update_state(&core, &transition(core.clone(), "_unload", 5))?;
            }
            Ok(core)
        })();
        Box::pin(async move {
            wait_inertia(&result.map_err(|error| js_anyhow(&error))?)
                .await
                .map_err(|error| js_anyhow(&error))
        })
    }

    pub(super) fn settled(&self) -> crate::fiber::DisposeFuture {
        let core = self.core();
        Box::pin(async move {
            JsFuture::from(Self::wait(core))
                .await
                .map(|_| ())
                .map_err(|error| js_anyhow(&error))
        })
    }
}

pub(super) fn assert_active(owner: &JsValue) -> Result<(), JsValue> {
    if Reflect::get(owner, &"uid".into())?.is_null() {
        Err(super::browser_errors::inactive())
    } else {
        Ok(())
    }
}

async fn root_startup(owner: &JsValue) -> Result<(), JsValue> {
    let config = method(
        owner,
        "_resolveConfig",
        &Array::of1(&Reflect::get(owner, &"_config".into())?),
    )?;
    super::browser_values::set(owner, &"config".into(), &config)?;
    let result = super::browser_registry::method(
        owner,
        "_execute",
        &Array::of1(&super::browser_runner::runner(owner)?),
    )?;
    JsFuture::from(Promise::resolve(&result)).await?;
    Ok(())
}

fn invoke_transition(owner: &JsValue, operation: &str, state: u8) -> Result<JsValue, JsValue> {
    let inertia = method(owner, operation, &Array::new())?;
    super::browser_values::set(owner, &"inertia".into(), &inertia)?;
    Ok(state.into())
}

fn transition(owner: JsValue, operation: &'static str, state: u8) -> JsValue {
    Closure::wrap(
        Box::new(move || invoke_transition(&owner, operation, state))
            as Box<dyn Fn() -> Result<JsValue, JsValue>>,
    )
    .into_js_value()
}

async fn wait_inertia(owner: &JsValue) -> Result<(), JsValue> {
    loop {
        let inertia = Reflect::get(owner, &"inertia".into())?;
        if !inertia.is_truthy() {
            return Ok(());
        }
        JsFuture::from(Promise::resolve(&inertia)).await?;
    }
}

pub(super) fn data_field(target: &Object, name: &str, value: JsValue) -> Result<(), JsValue> {
    Reflect::define_property(
        target,
        &name.into(),
        &super::object(&[
            ("value", value),
            ("writable", JsValue::TRUE),
            ("enumerable", JsValue::TRUE),
            ("configurable", JsValue::TRUE),
        ])?,
    )?;
    Ok(())
}
