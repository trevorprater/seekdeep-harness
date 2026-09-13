//! Browser module import and entry lifecycle bindings.

use std::{cell::Cell, sync::Arc};

use js_sys::{Array, Function, Object, Promise, Reflect};
use parking_lot::Mutex;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use crate::NAME;

#[derive(Clone)]
struct EntryRecord {
    id: String,
    face: JsValue,
}

/// Browser Loader service backed by Rust-owned entry state.
#[wasm_bindgen]
pub struct WasmClientLoader {
    context: JsValue,
    internal: Arc<Mutex<JsValue>>,
    entries: Arc<Mutex<Vec<EntryRecord>>>,
    next_id: Cell<u64>,
}

#[wasm_bindgen]
impl WasmClientLoader {
    /// Creates an empty Loader bound to one compiled Cordis Context.
    #[wasm_bindgen(constructor)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(context: JsValue) -> Self {
        Self {
            context,
            internal: Arc::new(Mutex::new(JsValue::UNDEFINED)),
            entries: Arc::new(Mutex::new(Vec::new())),
            next_id: Cell::new(0),
        }
    }

    /// Module-system adapter used for package imports.
    #[wasm_bindgen(getter)]
    pub fn internal(&self) -> JsValue {
        self.internal.lock().clone()
    }

    /// Replaces the module-system adapter after shell construction.
    #[wasm_bindgen(setter)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn set_internal(&self, internal: JsValue) {
        *self.internal.lock() = internal;
    }

    /// Creates, imports, and starts one entry.
    #[allow(clippy::needless_pass_by_value)]
    pub fn create(&self, options: JsValue) -> Promise {
        let context = self.context.clone();
        let internal = self.internal.clone();
        let entries = self.entries.clone();
        let id = match entry_id(&options, &self.next_id, &entries) {
            Ok(id) => id,
            Err(error) => return Promise::reject(&error),
        };
        future_to_promise(async move {
            let name = required_string(&options, "name")?;
            let face = entry_face(&id, &name, &options)?;
            entries.lock().push(EntryRecord {
                id: id.clone(),
                face: face.clone().into(),
            });
            let internal = internal.lock().clone();
            let result = start_entry(&context, &internal, &face, &options, &name).await;
            match result {
                Ok(()) => Ok(JsValue::from_str(&id)),
                Err(error) => {
                    entries.lock().retain(|entry| entry.id != id);
                    Err(error)
                }
            }
        })
    }

    /// Resolves one exact entry id.
    ///
    /// # Errors
    ///
    /// Returns the source-compatible missing-entry diagnostic.
    #[allow(clippy::needless_pass_by_value)] // wasm-bindgen owns JavaScript strings at this boundary.
    pub fn resolve(&self, id: String) -> Result<JsValue, JsValue> {
        self.entries
            .lock()
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.face.clone())
            .ok_or_else(|| js_sys::Error::new(&format!("cannot resolve entry {id}")).into())
    }

    /// Returns entries in stable id order.
    pub fn entries(&self) -> Array {
        self.entries
            .lock()
            .iter()
            .map(|entry| entry.face.clone())
            .collect()
    }

    /// Waits until every currently present Fiber is stable.
    #[wasm_bindgen(js_name = await)]
    pub fn wait(&self) -> Promise {
        let entries = self.entries.clone();
        future_to_promise(async move {
            loop {
                let snapshot = entries.lock().clone();
                for entry in &snapshot {
                    let fiber = Reflect::get(&entry.face, &JsValue::from_str("fiber"))?;
                    if fiber.is_undefined() {
                        continue;
                    }
                    let wait = call_method(&fiber, "await", &[])?;
                    JsFuture::from(Promise::resolve(&wait)).await?;
                }
                let current = entries
                    .lock()
                    .iter()
                    .map(|entry| entry.id.clone())
                    .collect::<Vec<_>>();
                let observed = snapshot
                    .iter()
                    .map(|entry| entry.id.clone())
                    .collect::<Vec<_>>();
                if current == observed {
                    return Ok(JsValue::UNDEFINED);
                }
            }
        })
    }

    /// Disposes and forgets one entry.
    pub fn remove(&self, id: String) -> Promise {
        let entries = self.entries.clone();
        future_to_promise(async move {
            let entry = {
                let mut entries = entries.lock();
                let Some(index) = entries.iter().position(|entry| entry.id == id) else {
                    return Ok(JsValue::UNDEFINED);
                };
                entries.remove(index)
            };
            let fiber = Reflect::get(&entry.face, &JsValue::from_str("fiber"))?;
            if !fiber.is_undefined() {
                let disposed = call_method(&fiber, "dispose", &[])?;
                JsFuture::from(Promise::resolve(&disposed)).await?;
            }
            Ok(JsValue::UNDEFINED)
        })
    }
}

/// Builds the Loader plugin descriptor consumed by the compiled Cordis face.
///
/// # Errors
///
/// Returns JavaScript object-construction failures.
#[wasm_bindgen(js_name = clientLoaderPlugin)]
pub fn client_loader_plugin() -> Result<JsValue, JsValue> {
    let apply = Closure::wrap(Box::new(move |context: JsValue| -> Result<(), JsValue> {
        let loader: JsValue = WasmClientLoader::new(context.clone()).into();
        call_method(&context, "provide", &[JsValue::from_str(NAME), loader])?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let plugin = Object::new();
    set(&plugin, "name", &JsValue::from_str(NAME))?;
    set(&plugin, "apply", &apply.into_js_value())?;
    Ok(plugin.into())
}

async fn start_entry(
    context: &JsValue,
    internal: &JsValue,
    face: &Object,
    options: &JsValue,
    name: &str,
) -> Result<(), JsValue> {
    let group = Reflect::get(options, &JsValue::from_str("group"))?;
    let disabled = Reflect::get(options, &JsValue::from_str("disabled"))?;
    if !group.is_truthy() && disabled.is_truthy() {
        return Ok(());
    }
    if internal.is_undefined() || internal.is_null() {
        return Err(js_sys::Error::new("loader: internal module system is unavailable").into());
    }
    let imported = call_method(
        internal,
        "import",
        &[
            JsValue::from_str(name),
            JsValue::from_str(""),
            Object::new().into(),
        ],
    )?;
    let imported = JsFuture::from(Promise::resolve(&imported)).await?;
    let module = unwrap_exports(imported)?;
    let descriptor = merge_entry_inject(&module, options)?;
    let config = Reflect::get(options, &JsValue::from_str("config"))?;
    let fiber = call_method(context, "plugin", &[descriptor, config])?;
    Reflect::set(&fiber, &JsValue::from_str("entry"), face)?;
    set(face, "fiber", &fiber)?;
    let _ = call_method(
        context,
        "emit",
        &[JsValue::from_str("internal/status"), fiber.clone()],
    );
    let wait = call_method(&fiber, "await", &[])?;
    JsFuture::from(Promise::resolve(&wait)).await?;
    let _ = call_method(
        context,
        "emit",
        &[JsValue::from_str("internal/status"), fiber],
    );
    Ok(())
}

fn merge_entry_inject(module: &JsValue, options: &JsValue) -> Result<JsValue, JsValue> {
    let descriptor = Object::new();
    let apply = if let Some(function) = module.dyn_ref::<Function>() {
        function.clone().into()
    } else {
        Reflect::get(module, &JsValue::from_str("apply"))?
    };
    if !apply.is_function() {
        return Err(
            js_sys::Error::new("loader module does not export a plugin apply function").into(),
        );
    }
    set(&descriptor, "apply", &apply)?;
    let name = required_string(module, "name").or_else(|_| required_string(options, "name"))?;
    set(&descriptor, "name", &JsValue::from_str(&name))?;
    let merged = Array::new();
    for source in [
        Reflect::get(module, &JsValue::from_str("inject"))?,
        Reflect::get(options, &JsValue::from_str("inject"))?,
    ] {
        for name in inject_names(&source)? {
            if !merged.includes(&JsValue::from_str(&name), 0) {
                merged.push(&JsValue::from_str(&name));
            }
        }
    }
    if merged.length() > 0 {
        set(&descriptor, "inject", &merged.into())?;
    }
    Ok(descriptor.into())
}

fn unwrap_exports(mut value: JsValue) -> Result<JsValue, JsValue> {
    if value.is_undefined() || value.is_null() {
        return Ok(value);
    }
    let default = Reflect::get(&value, &JsValue::from_str("default"))?;
    if !default.is_undefined() && !default.is_null() {
        value = default;
    }
    let es_module = Reflect::get(&value, &JsValue::from_str("__esModule"))?
        .as_bool()
        .unwrap_or(false);
    if es_module {
        let default = Reflect::get(&value, &JsValue::from_str("default"))?;
        if !default.is_undefined() && !default.is_null() {
            value = default;
        }
    }
    Ok(value)
}

fn entry_id(
    options: &JsValue,
    next_id: &Cell<u64>,
    entries: &Mutex<Vec<EntryRecord>>,
) -> Result<String, JsValue> {
    let configured = Reflect::get(options, &JsValue::from_str("id"))?;
    let id = if let Some(id) = configured.as_string() {
        id
    } else {
        let next = next_id.get().checked_add(1).ok_or_else(|| {
            JsValue::from(js_sys::Error::new("loader entry id counter exhausted"))
        })?;
        next_id.set(next);
        format!("{next:08x}")
    };
    if entries.lock().iter().any(|entry| entry.id == id) {
        return Err(js_sys::Error::new(&format!("duplicate loader entry id: {id}")).into());
    }
    Ok(id)
}

fn entry_face(id: &str, name: &str, source: &JsValue) -> Result<Object, JsValue> {
    let options = Object::new();
    set(&options, "id", &JsValue::from_str(id))?;
    set(&options, "name", &JsValue::from_str(name))?;
    for key in ["config", "group", "disabled", "inject"] {
        let value = Reflect::get(source, &JsValue::from_str(key))?;
        if !value.is_undefined() {
            set(&options, key, &value)?;
        }
    }
    let face = Object::new();
    set(&face, "options", &options.into())?;
    set(&face, "fiber", &JsValue::UNDEFINED)?;
    Ok(face)
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
                    js_sys::Error::new("loader inject values must be strings").into()
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
    Err(js_sys::Error::new("loader inject must be an array or object").into())
}

fn required_string(value: &JsValue, key: &str) -> Result<String, JsValue> {
    Reflect::get(value, &JsValue::from_str(key))?
        .as_string()
        .ok_or_else(|| js_sys::Error::new(&format!("loader field {key:?} must be a string")).into())
}

fn call_method(value: &JsValue, name: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let function = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let args: Array = args.iter().cloned().collect();
    function.apply(value, &args)
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(object, &JsValue::from_str(key), value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new(&format!("failed to set Loader member {key:?}")).into())
    }
}
