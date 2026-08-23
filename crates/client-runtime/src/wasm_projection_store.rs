//! Browser facade for Host-computed Session projection values.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use indexmap::IndexMap;
use js_sys::{Function, Object, Reflect};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    ProjectionFace, ProjectionValueStore, ProjectionsBaseline, RuntimeDisposer,
    wasm_notifier::browser_notifier_scheduler,
};

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

type ProjectionSnapshot = Rc<IndexMap<String, Rc<JsValue>>>;

/// Browser `ProjectionValueStore` backed by the portable Rust core.
#[wasm_bindgen(js_name = ProjectionValueStore)]
pub struct WasmProjectionValueStore {
    store: ProjectionValueStore<JsValue>,
    faces: RefCell<HashMap<String, JsValue>>,
    values_cache: RefCell<Option<(ProjectionSnapshot, JsValue)>>,
}

#[wasm_bindgen(js_class = ProjectionValueStore)]
impl WasmProjectionValueStore {
    /// Creates one empty per-Session store.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            store: ProjectionValueStore::new(browser_notifier_scheduler()),
            faces: RefCell::new(HashMap::new()),
            values_cache: RefCell::new(None),
        }
    }

    /// Returns the identity-stable bare observable face for one key.
    ///
    /// # Errors
    ///
    /// Returns when JavaScript face construction fails.
    #[wasm_bindgen(js_name = faceOf)]
    pub fn face_of(&self, key: String) -> Result<JsValue, JsValue> {
        if let Some(face) = self.faces.borrow().get(&key) {
            return Ok(face.clone());
        }
        let value = projection_face(self.store.face_of(key.clone()))?;
        self.faces.borrow_mut().insert(key, value.clone());
        Ok(value)
    }

    /// Returns one current whole value or JavaScript `undefined`.
    pub fn get(&self, key: &str) -> JsValue {
        self.store
            .get(key)
            .map_or(JsValue::UNDEFINED, |value| value.as_ref().clone())
    }

    /// Returns the same frozen aggregate object until a row changes.
    ///
    /// # Errors
    ///
    /// Returns when a projection property cannot be defined.
    pub fn values(&self) -> Result<JsValue, JsValue> {
        let snapshot = self.store.values();
        if let Some((current, value)) = &*self.values_cache.borrow()
            && Rc::ptr_eq(current, &snapshot)
        {
            return Ok(value.clone());
        }
        let value = Object::new();
        for (key, projection) in snapshot.iter() {
            set(&value, key, projection)?;
        }
        Object::freeze(&value);
        let value: JsValue = value.into();
        *self.values_cache.borrow_mut() = Some((snapshot, value.clone()));
        Ok(value)
    }

    /// Subscribes to any-key changes.
    #[wasm_bindgen(js_name = subscribeAny)]
    pub fn subscribe_any(&self, listener: Function) -> Function {
        disposer(self.store.subscribe_any(Rc::new(move || {
            call_or_throw(listener.call0(&JsValue::UNDEFINED));
        })))
    }

    /// Applies one strictly newer finished value.
    ///
    /// # Errors
    ///
    /// Returns when `seq` is not a safe integer.
    #[allow(clippy::needless_pass_by_value)]
    pub fn apply(&self, key: String, value: JsValue, seq: f64) -> Result<(), JsValue> {
        self.store.apply(key, Rc::new(value), sequence(seq)?);
        Ok(())
    }

    /// Seeds one history-tail projections block.
    ///
    /// # Errors
    ///
    /// Returns when the baseline shape or sequence is invalid.
    #[allow(clippy::needless_pass_by_value)]
    pub fn seed(&self, baseline: JsValue) -> Result<(), JsValue> {
        let as_of_seq = required_number(&baseline, "asOfSeq")?;
        let values = required(&baseline, "values")?;
        if !values.is_object() || values.is_null() {
            return Err(js_sys::Error::new("projection baseline values must be an object").into());
        }
        let values = Object::from(values);
        let mut baseline_values = IndexMap::new();
        for key in Object::keys(&values).iter() {
            let key = key
                .as_string()
                .ok_or_else(|| js_sys::Error::new("projection key must be a string"))?;
            baseline_values.insert(
                key.clone(),
                Rc::new(Reflect::get(&values, &JsValue::from_str(&key))?),
            );
        }
        self.store.seed(&ProjectionsBaseline {
            as_of_seq,
            values: baseline_values,
        });
        Ok(())
    }

    /// Drops rows newer than a replacement generation's durable baseline.
    ///
    /// # Errors
    ///
    /// Returns when `lastSeq` is not a safe integer.
    pub fn truncate(&self, last_seq: f64) -> Result<(), JsValue> {
        self.store.truncate(sequence(last_seq)?);
        Ok(())
    }
}

impl Default for WasmProjectionValueStore {
    fn default() -> Self {
        Self::new()
    }
}

fn projection_face(face: Rc<ProjectionFace<JsValue>>) -> Result<JsValue, JsValue> {
    let value = Object::new();
    let snapshot_face = face.clone();
    let snapshot = Closure::wrap(Box::new(move || {
        snapshot_face
            .snapshot()
            .map_or(JsValue::UNDEFINED, |value| value.as_ref().clone())
    }) as Box<dyn FnMut() -> JsValue>);
    set(&value, "getSnapshot", &snapshot.into_js_value())?;
    let subscribe = Closure::wrap(Box::new(move |listener: Function| {
        disposer(face.subscribe(Rc::new(move || {
            call_or_throw(listener.call0(&JsValue::UNDEFINED));
        })))
    }) as Box<dyn FnMut(Function) -> Function>);
    set(&value, "subscribe", &subscribe.into_js_value())?;
    Ok(value.into())
}

fn sequence(value: f64) -> Result<i64, JsValue> {
    if !value.is_finite()
        || value.fract() != 0.0
        || !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&value)
    {
        return Err(js_sys::Error::new("projection seq must be a safe integer").into());
    }
    #[allow(clippy::cast_possible_truncation)]
    Ok(value as i64)
}

fn required_number(value: &JsValue, key: &str) -> Result<i64, JsValue> {
    let number = required(value, key)?.as_f64().ok_or_else(|| {
        js_sys::Error::new(&format!("projection baseline {key} must be a number"))
    })?;
    sequence(number)
}

fn required(value: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    let member = Reflect::get(value, &JsValue::from_str(key))?;
    if member.is_undefined() || member.is_null() {
        Err(js_sys::Error::new(&format!("projection baseline requires {key:?}")).into())
    } else {
        Ok(member)
    }
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(object, &JsValue::from_str(key), value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new(&format!("failed to set projection property {key:?}")).into())
    }
}

fn disposer(disposer: RuntimeDisposer) -> Function {
    Closure::wrap(Box::new(move || disposer.dispose()) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into()
}

fn call_or_throw(result: Result<JsValue, JsValue>) {
    if let Err(error) = result {
        wasm_bindgen::throw_val(error);
    }
}
