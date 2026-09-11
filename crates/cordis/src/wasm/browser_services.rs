//! Shared JavaScript implementation records for reflection and Fiber activation stores.

use std::{collections::HashMap, sync::Arc};

use js_sys::{Function, Map, Object, Reflect};
use parking_lot::Mutex;
use wasm_bindgen::{JsCast as _, JsValue};

use crate::{Context, fiber::EffectHandle, service::ServiceSlot};

pub(crate) struct BrowserServices {
    symbols: Mutex<HashMap<ServiceSlot, JsValue>>,
    isolation_labels: Map,
    isolation_realms: Mutex<Vec<Arc<crate::context::IsolationRealm>>>,
    pub(crate) store: Object,
    pub(crate) props: Object,
}

pub(super) fn initialize_root_key(root: &JsValue, name: &str) -> Result<(), JsValue> {
    let scopes = Reflect::get(root, &super::browser_symbols::get("isolate")?)?;
    let existing = Reflect::get(&scopes, &name.into())?;
    if existing.is_null() || existing.is_undefined() {
        let native = Reflect::get(root, &"__seekdeepContext".into())?;
        let key = super::browser_registry::method(
            &native,
            "serviceScope",
            &js_sys::Array::of1(&name.into()),
        )?;
        Reflect::set(&scopes, &name.into(), &key)?;
    }
    Ok(())
}

impl Default for BrowserServices {
    fn default() -> Self {
        Self {
            symbols: Mutex::default(),
            isolation_labels: Map::new(),
            isolation_realms: Mutex::default(),
            store: Object::create(&Object::from(JsValue::NULL)),
            props: Object::create(&Object::from(JsValue::NULL)),
        }
    }
}

impl BrowserServices {
    pub(crate) fn key(&self, slot: &ServiceSlot) -> Result<JsValue, JsValue> {
        if let Some(key) = self.symbols.lock().get(slot).cloned() {
            return Ok(key);
        }
        let symbol = Reflect::get(&js_sys::global(), &"Symbol".into())?.dyn_into::<Function>()?;
        let key = symbol.call1(&JsValue::UNDEFINED, &slot.name.clone().into())?;
        self.symbols.lock().insert(slot.clone(), key.clone());
        Ok(key)
    }

    pub(super) fn isolate(
        &self,
        context: &Context,
        name: &str,
        label: &JsValue,
    ) -> Result<Context, JsValue> {
        let isolated = if let Some(label) = label.as_string() {
            context.isolate_named_as(name, &label)
        } else {
            let cached = self.isolation_labels.get(label);
            let realm = if cached.is_undefined() {
                let realm = context.new_isolation();
                let id = {
                    let mut realms = self.isolation_realms.lock();
                    let id = u32::try_from(realms.len())
                        .map_err(|_| js_sys::RangeError::new("too many isolation labels"))?;
                    realms.push(realm.clone());
                    id
                };
                self.isolation_labels.set(label, &id.into());
                realm
            } else {
                let id: u32 = serde_wasm_bindgen::from_value(cached)
                    .map_err(|error| js_sys::Error::new(&error.to_string()))?;
                self.isolation_realms.lock()[id as usize].clone()
            };
            context.with_isolation(name, realm)
        };
        self.symbols
            .lock()
            .insert(isolated.slot(name), label.clone());
        Ok(isolated)
    }

    pub(crate) fn get_impl(
        &self,
        context: &Context,
        name: &str,
        strict: bool,
    ) -> Result<JsValue, JsValue> {
        let key = self.key(&context.slot(name))?;
        let implementation = Reflect::get(&self.store, &key)?;
        if !implementation.is_truthy() {
            return Ok(JsValue::UNDEFINED);
        }
        if strict {
            let fiber = Reflect::get(&implementation, &"fiber".into())?;
            if Reflect::get(&fiber, &"state".into())?.as_f64() != Some(2.0) {
                return Ok(JsValue::UNDEFINED);
            }
        }
        Ok(implementation)
    }

    pub(crate) fn remove(
        &self,
        context: &Context,
        name: &str,
        implementation: &JsValue,
    ) -> Result<(), JsValue> {
        let key = self.key(&context.slot(name))?;
        if Reflect::get(&self.store, &key)? == *implementation {
            Reflect::delete_property(&self.store, &key)?;
        }
        Ok(())
    }

    pub(crate) fn dependency(&self, context: &Context, name: &str) -> Option<bool> {
        let implementation = self.get_impl(context, name, false).ok()?;
        if !implementation.is_truthy() {
            update_dependency(context, name, None);
            return None;
        }
        let ready = (|| -> Result<bool, JsValue> {
            let fiber = Reflect::get(&implementation, &"fiber".into())?;
            if Reflect::get(&fiber, &"state".into())?.as_f64() != Some(2.0) {
                return Ok(false);
            }
            let check = Reflect::get(&implementation, &"check".into())?;
            if !check.is_truthy() {
                return Ok(true);
            }
            let value = Reflect::get(&implementation, &"value".into())?;
            let owner = context.fiber().browser_context();
            let receiver = if owner.is_undefined() {
                value
            } else {
                super::tracing::Tracer::new(owner).trace(&value)?
            };
            check
                .dyn_into::<Function>()?
                .call0(&receiver)
                .map(|value| value.is_truthy())
        })()
        .unwrap_or_else(|error| {
            tracing::error!(?error, "browser service availability check failed");
            false
        });
        update_dependency(context, name, ready.then_some(&implementation));
        Some(ready)
    }

    pub(crate) fn retain_failed(
        self: &Arc<Self>,
        context: &Context,
        name: &str,
        implementation: &JsValue,
    ) {
        let (store, context, name, implementation) = (
            self.clone(),
            context.clone(),
            name.to_owned(),
            implementation.clone(),
        );
        let root = context.root_fiber().clone();
        let effect = EffectHandle::synchronous("browser failed-publication view", move || {
            store
                .remove(&context, &name, &implementation)
                .map_err(|error| super::js_anyhow(&error))
        });
        let _ = root.own(effect);
    }
}

fn update_dependency(context: &Context, name: &str, implementation: Option<&JsValue>) {
    let owner = context.fiber().browser_context();
    if owner.is_undefined() {
        return;
    }
    let result = (|| -> Result<(), JsValue> {
        let fiber = Reflect::get(&owner, &"fiber".into())?;
        let store = Reflect::get(&fiber, &"_store".into())?;
        if let Some(implementation) = implementation {
            Reflect::set(&store, &name.into(), implementation)?;
        } else {
            Reflect::delete_property(store.unchecked_ref::<Object>(), &name.into())?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        tracing::error!(?error, "browser dependency store rejected update");
    }
}
