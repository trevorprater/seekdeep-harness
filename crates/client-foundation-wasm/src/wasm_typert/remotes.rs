//! Atomic, calling-fiber Remote descriptors and change subscriptions.

use super::{State, WasmTypertSchemaRegistry, error, get, lookup, string, validation, values};
use js_sys::{Array, Function, Object};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};

/// Remote-only view over the registry's descriptor ownership implementation.
#[wasm_bindgen]
pub struct WasmTypertRemoteRegistry {
    inner: WasmTypertSchemaRegistry,
}

#[wasm_bindgen]
impl WasmTypertRemoteRegistry {
    /// Validates the complete batch before publishing any package or descriptor.
    ///
    /// # Errors
    /// Rejects malformed declarations, duplicate packages, endpoints, or invocation IDs.
    pub fn register(&self, context: &JsValue, contribution: &JsValue) -> Result<JsValue, JsValue> {
        let package = string(contribution, "package")?;
        validation::segment("Remote package name", &package)?;
        if lookup(&self.inner.state.borrow().packages, &package).is_some() {
            return Err(error(&format!(
                "typert: Remote package \"{package}\" is already registered"
            )));
        }
        let descriptors = values(&get(contribution, "descriptors")?)?;
        self.inner.validate_descriptors(&descriptors)?;
        self.inner.publish(
            context,
            package,
            Object::new().into(),
            Vec::new(),
            descriptors,
        )
    }

    /// Returns the exact live descriptor.
    pub fn get(&self, endpoint: &str) -> JsValue {
        self.inner.local_get(endpoint)
    }

    /// Enumerates live descriptors in insertion order.
    pub fn list(&self) -> Array {
        self.inner.local_list()
    }

    /// Registers an effect-owned change observer.
    ///
    /// # Errors
    /// Propagates observer and owner validation failures.
    pub fn subscribe(&self, context: &JsValue, listener: &JsValue) -> Result<JsValue, JsValue> {
        self.inner.local_subscribe(context, listener)
    }
}

pub(super) fn install(service: &Object, context: &JsValue) -> Result<(), JsValue> {
    let core = WasmTypertRemoteRegistry {
        inner: WasmTypertSchemaRegistry {
            state: Rc::new(RefCell::new(State::default())),
            context: context.clone(),
            kind: "remote",
        },
    };
    Function::new_with_args(
        "service,core",
        r"
Object.defineProperty(service, 'remotes', { configurable: true, get() {
  const ctx = this.ctx;
  return { register: value => core.register(ctx, value), get: key => core.get(key),
    list: () => core.list(), subscribe: listener => core.subscribe(ctx, listener) };
}});
",
    )
    .call2(&JsValue::UNDEFINED, service, &core.into())?;
    Ok(())
}
