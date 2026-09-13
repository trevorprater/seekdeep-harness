//! Compiled browser composer-block registry and snapshot-store faces.

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use js_sys::{Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

type BrowserStores = Rc<RefCell<BTreeMap<String, JsValue>>>;

struct BrowserBlockStore {
    value: RefCell<JsValue>,
    listeners: RefCell<BTreeMap<u64, Function>>,
    next_listener: RefCell<u64>,
}

/// Browser-compatible constructible composer-block registry.
#[wasm_bindgen(js_name = ComposerBlockRegistry)]
pub struct BrowserComposerBlockRegistry {
    stores: BrowserStores,
}

#[wasm_bindgen(js_class = ComposerBlockRegistry)]
#[allow(clippy::needless_pass_by_value)] // wasm-bindgen class methods own JavaScript arguments.
impl BrowserComposerBlockRegistry {
    /// Creates an empty registry.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            stores: Rc::new(RefCell::new(BTreeMap::new())),
        }
    }

    /// Raises or clears one session block.
    ///
    /// # Errors
    ///
    /// Returns for invalid session IDs, malformed blocks, or store callback failures.
    #[wasm_bindgen(js_name = set)]
    pub fn set_browser(&self, session_id: JsValue, block: JsValue) -> Result<(), JsValue> {
        set_browser(&self.stores, &session_id, &block)
    }

    /// Returns the identity-stable browser snapshot store for one session.
    ///
    /// # Errors
    ///
    /// Returns for invalid session IDs or store-face construction failures.
    #[wasm_bindgen(js_name = storeFor)]
    pub fn store_for_browser_method(&self, session_id: JsValue) -> Result<JsValue, JsValue> {
        store_for_browser(&self.stores, &session_id)
    }

    /// Drops one session's registry-owned store handle.
    ///
    /// # Errors
    ///
    /// Returns for invalid session IDs.
    #[wasm_bindgen(js_name = forget)]
    pub fn forget_browser(&self, session_id: JsValue) -> Result<(), JsValue> {
        let key = session_key(&session_id)?;
        self.stores.borrow_mut().remove(&key);
        Ok(())
    }
}

impl Default for BrowserComposerBlockRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserBlockStore {
    fn new() -> Rc<Self> {
        Rc::new(Self {
            value: RefCell::new(JsValue::UNDEFINED),
            listeners: RefCell::new(BTreeMap::new()),
            next_listener: RefCell::new(0),
        })
    }

    fn face(self: &Rc<Self>) -> Result<JsValue, JsValue> {
        let snapshot_store = Rc::clone(self);
        let get_snapshot =
            Closure::wrap(Box::new(move || snapshot_store.value.borrow().clone())
                as Box<dyn FnMut() -> JsValue>)
            .into_js_value();
        let set_store = Rc::clone(self);
        let set = Closure::wrap(Box::new(move |value: JsValue| -> Result<(), JsValue> {
            set_store.replace(value)
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value();
        let update_store = Rc::clone(self);
        let update = Closure::wrap(Box::new(move |mutator: JsValue| -> Result<(), JsValue> {
            let mutator = mutator.dyn_into::<Function>()?;
            let current = update_store.value.borrow().clone();
            let draft = if current.is_object() && !current.is_null() {
                Object::assign(&Object::new(), &Object::from(current)).into()
            } else {
                current
            };
            let returned = mutator.call1(&JsValue::UNDEFINED, &draft)?;
            update_store.replace(if returned.is_undefined() {
                draft
            } else {
                returned
            })
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value();
        let subscribe_store = Rc::clone(self);
        let subscribe = Closure::wrap(Box::new(
            move |listener: JsValue| -> Result<JsValue, JsValue> {
                let listener = listener.dyn_into::<Function>()?;
                let id = {
                    let mut next = subscribe_store.next_listener.borrow_mut();
                    *next = next.wrapping_add(1);
                    *next
                };
                subscribe_store.listeners.borrow_mut().insert(id, listener);
                let unsubscribe_store = Rc::clone(&subscribe_store);
                Ok(Closure::wrap(Box::new(move || {
                    unsubscribe_store.listeners.borrow_mut().remove(&id);
                }) as Box<dyn FnMut()>)
                .into_js_value())
            },
        )
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
        .into_js_value();
        Ok(object(&[
            ("getSnapshot", get_snapshot),
            ("update", update),
            ("set", set),
            ("subscribe", subscribe),
        ])?
        .into())
    }

    fn replace(&self, value: JsValue) -> Result<(), JsValue> {
        *self.value.borrow_mut() = value;
        let listeners = self
            .listeners
            .borrow()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for listener in listeners {
            listener.call0(&JsValue::UNDEFINED)?;
        }
        Ok(())
    }
}

/// Creates one browser-compatible `ComposerBlockRegistry` face.
///
/// # Errors
///
/// Returns if JavaScript store-face construction fails.
#[wasm_bindgen(js_name = composerBlockRegistry)]
pub fn composer_block_registry_browser() -> Result<JsValue, JsValue> {
    let stores = BrowserComposerBlockRegistry::new().stores;
    let store_for_stores = Rc::clone(&stores);
    let store_for = Closure::wrap(Box::new(move |session_id: JsValue| {
        store_for_browser(&store_for_stores, &session_id)
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value();
    let set_stores = Rc::clone(&stores);
    let set = Closure::wrap(Box::new(
        move |session_id: JsValue, block: JsValue| -> Result<(), JsValue> {
            set_browser(&set_stores, &session_id, &block)
        },
    )
        as Box<dyn FnMut(JsValue, JsValue) -> Result<(), JsValue>>)
    .into_js_value();
    let forget_stores = stores;
    let forget = Closure::wrap(Box::new(move |session_id: JsValue| -> Result<(), JsValue> {
        let key = session_key(&session_id)?;
        forget_stores.borrow_mut().remove(&key);
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
    .into_js_value();
    Ok(object(&[("set", set), ("storeFor", store_for), ("forget", forget)])?.into())
}

fn set_browser(
    stores: &BrowserStores,
    session_id: &JsValue,
    block: &JsValue,
) -> Result<(), JsValue> {
    let store = store_for_browser(stores, session_id)?;
    let current =
        required_function(&store, "getSnapshot", "composer block store")?.call0(&store)?;
    if Object::is(&optional_reason(&current)?, &optional_reason(block)?) {
        return Ok(());
    }
    required_function(&store, "set", "composer block store")?.call1(&store, block)?;
    Ok(())
}

fn store_for_browser(stores: &BrowserStores, session_id: &JsValue) -> Result<JsValue, JsValue> {
    let key = session_key(session_id)?;
    if let Some(existing) = stores.borrow().get(&key) {
        return Ok(existing.clone());
    }
    let created = BrowserBlockStore::new().face()?;
    stores.borrow_mut().insert(key, created.clone());
    Ok(created)
}

fn session_key(value: &JsValue) -> Result<String, JsValue> {
    value
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("composer block session id must be a string").into())
}

fn optional_reason(value: &JsValue) -> Result<JsValue, JsValue> {
    if value.is_null() || value.is_undefined() {
        Ok(JsValue::UNDEFINED)
    } else {
        Reflect::get(value, &JsValue::from_str("reason"))
    }
}

fn required_function(value: &JsValue, key: &str, owner: &str) -> Result<Function, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        return Err(js_sys::Error::new(&format!("{owner} omitted {key}")).into());
    }
    property.dyn_into()
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        Reflect::set(&object, &JsValue::from_str(key), value)?;
    }
    Ok(object)
}
