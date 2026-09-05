//! JavaScript-compatible immutable Store engine compiled into WebAssembly.

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use js_sys::{Array, Function, JSON, Object, Promise, Reflect};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;

thread_local! {
    static PRODUCE: RefCell<Option<Function>> = const { RefCell::new(None) };
}

/// Installs the page's existing Immer `produce` function.
#[wasm_bindgen(js_name = installStoreProduce)]
#[allow(clippy::needless_pass_by_value)]
pub fn install_store_produce(produce: Function) {
    PRODUCE.with(|current| *current.borrow_mut() = Some(produce));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BrowserFlushMode {
    Sync,
    Frame,
}

struct BrowserSnapshotState {
    value: JsValue,
    listeners: BTreeMap<u64, Function>,
    next_listener: u64,
    scheduled: bool,
}

struct BrowserSnapshotStore {
    state: RefCell<BrowserSnapshotState>,
    mode: BrowserFlushMode,
    produce: Function,
    persist_name: Option<String>,
}

impl BrowserSnapshotStore {
    fn new(initial: JsValue, options: &JsValue) -> Result<Rc<Self>, JsValue> {
        let mode = match optional_string(options, "flush")?.as_deref() {
            Some("raf") => BrowserFlushMode::Frame,
            Some("sync") | None => BrowserFlushMode::Sync,
            Some(value) => {
                return Err(js_sys::Error::new(&format!(
                    "snapshot Store flush must be 'sync' or 'raf', received {value:?}"
                ))
                .into());
            }
        };
        let persist_name = optional(options, "persist")?
            .as_ref()
            .map(|persist| required_string(persist, "name"))
            .transpose()?;
        let produce = PRODUCE
            .with(|produce| produce.borrow().clone())
            .ok_or_else(|| {
                js_sys::Error::new(
                    "snapshot Store requires installStoreProduce(immer.produce) at boot",
                )
            })?;
        let value = match persist_name.as_deref() {
            Some(name) => rehydrate(name).unwrap_or(initial),
            None => initial,
        };
        Ok(Rc::new(Self {
            state: RefCell::new(BrowserSnapshotState {
                value,
                listeners: BTreeMap::new(),
                next_listener: 0,
                scheduled: false,
            }),
            mode,
            produce,
            persist_name,
        }))
    }

    fn snapshot(&self) -> JsValue {
        self.state.borrow().value.clone()
    }

    fn subscribe(self: &Rc<Self>, listener: Function) -> Function {
        let id = {
            let mut state = self.state.borrow_mut();
            state.next_listener = state.next_listener.wrapping_add(1);
            let id = state.next_listener;
            state.listeners.insert(id, listener);
            id
        };
        let weak = Rc::downgrade(self);
        Closure::wrap(Box::new(move || {
            if let Some(store) = weak.upgrade() {
                store.state.borrow_mut().listeners.remove(&id);
            }
        }) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into()
    }

    fn update(self: &Rc<Self>, mutator: &Function) -> Result<(), JsValue> {
        let next = self
            .produce
            .call2(&JsValue::UNDEFINED, &self.snapshot(), mutator)?;
        if !is_production() {
            deep_freeze(&next)?;
        }
        self.commit(next);
        Ok(())
    }

    fn set(self: &Rc<Self>, next: JsValue) -> Result<(), JsValue> {
        if !is_production() {
            deep_freeze(&next)?;
        }
        self.commit(next);
        Ok(())
    }

    fn commit(self: &Rc<Self>, next: JsValue) {
        self.state.borrow_mut().value = next;
        if let Some(name) = &self.persist_name {
            persist(name, &self.snapshot());
        }
        match self.mode {
            BrowserFlushMode::Sync => self.notify(),
            BrowserFlushMode::Frame => self.schedule(),
        }
    }

    fn schedule(self: &Rc<Self>) {
        {
            let mut state = self.state.borrow_mut();
            if state.scheduled {
                return;
            }
            state.scheduled = true;
        }
        let weak = Rc::downgrade(self);
        let callback = move || {
            if let Some(store) = weak.upgrade() {
                store.state.borrow_mut().scheduled = false;
                store.notify();
            }
        };
        let global = js_sys::global();
        if let Ok(frame) = Reflect::get(&global, &JsValue::from_str("requestAnimationFrame"))
            && let Ok(frame) = frame.dyn_into::<Function>()
        {
            let callback = Closure::once_into_js(callback);
            let _ = frame.call1(&global, &callback);
            return;
        }
        wasm_bindgen_futures::spawn_local(async move {
            let _ = JsFuture::from(Promise::resolve(&JsValue::UNDEFINED)).await;
            callback();
        });
    }

    fn notify(&self) {
        let listeners = self
            .state
            .borrow()
            .listeners
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for listener in listeners {
            if let Err(error) = listener.call0(&JsValue::UNDEFINED) {
                wasm_bindgen::throw_val(error);
            }
        }
    }

    fn clear_persisted(&self) {
        if let Some(name) = &self.persist_name {
            let _ = storage_call("removeItem", &[JsValue::from_str(name)]);
        }
    }
}

/// Creates a source-compatible bare snapshot Store object.
///
/// # Errors
///
/// Returns missing Immer, malformed options, persistence parse, or JavaScript mutation failures.
#[wasm_bindgen(js_name = createSnapshotStore)]
#[allow(clippy::needless_pass_by_value)]
pub fn create_snapshot_store(initial: JsValue, options: JsValue) -> Result<JsValue, JsValue> {
    let options = if options.is_undefined() || options.is_null() {
        Object::new().into()
    } else {
        options
    };
    snapshot_store_face(BrowserSnapshotStore::new(initial, &options)?)
}

/// Shallow equality using JavaScript reference identity for one-level members.
#[wasm_bindgen(js_name = shallowEqual)]
#[allow(clippy::needless_pass_by_value)]
pub fn shallow_equal(left: JsValue, right: JsValue) -> bool {
    if Object::is(&left, &right) {
        return true;
    }
    if Array::is_array(&left) != Array::is_array(&right)
        || !left.is_object()
        || left.is_null()
        || !right.is_object()
        || right.is_null()
    {
        return false;
    }
    let left = Object::from(left);
    let right = Object::from(right);
    let left_keys = Object::keys(&left);
    let right_keys = Object::keys(&right);
    if left_keys.length() != right_keys.length() {
        return false;
    }
    left_keys.iter().all(|key| {
        Reflect::has(&right, &key).unwrap_or(false)
            && Reflect::get(&left, &key)
                .ok()
                .zip(Reflect::get(&right, &key).ok())
                .is_some_and(|(left, right)| Object::is(&left, &right))
    })
}

/// Bakes `init`, persistence, and draft actions into a reusable Store handle.
///
/// # Errors
///
/// Returns malformed declaration or JavaScript object-construction failures.
#[wasm_bindgen(js_name = defineStore)]
#[allow(clippy::needless_pass_by_value)]
pub fn define_store(declaration: JsValue) -> Result<JsValue, JsValue> {
    let init = required(&declaration, "init")?.dyn_into::<Function>()?;
    let actions = required(&declaration, "actions")?;
    if !actions.is_object() || actions.is_null() {
        return Err(js_sys::Error::new("Store declaration actions must be an object").into());
    }
    let persist = optional_string(&declaration, "persist")?;
    let handle = Object::new();
    set(&handle, "spec", &declaration)?;
    let create = Closure::wrap(
        Box::new(move |scope_key: JsValue| -> Result<JsValue, JsValue> {
            let resolved = persist.as_ref().map(|base| {
                scope_key
                    .as_string()
                    .map_or_else(|| base.clone(), |scope| format!("{base}.{scope}"))
            });
            let options = Object::new();
            if let Some(name) = &resolved {
                let persist = Object::new();
                set(&persist, "name", &JsValue::from_str(name))?;
                set(&options, "persist", &persist)?;
            }
            let store = BrowserSnapshotStore::new(init.call0(&JsValue::UNDEFINED)?, &options)?;
            engine_instance_face(store, &actions)
        }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    );
    set(&handle, "create", &create.into_js_value())?;
    Ok(handle.into())
}

fn snapshot_store_face(store: Rc<BrowserSnapshotStore>) -> Result<JsValue, JsValue> {
    let output = Object::new();
    let getter = store.clone();
    let get_snapshot =
        Closure::wrap(Box::new(move || getter.snapshot()) as Box<dyn FnMut() -> JsValue>);
    set(&output, "getSnapshot", &get_snapshot.into_js_value())?;
    let subscriber = store.clone();
    let subscribe = Closure::wrap(
        Box::new(move |listener: Function| subscriber.subscribe(listener))
            as Box<dyn FnMut(Function) -> Function>,
    );
    set(&output, "subscribe", &subscribe.into_js_value())?;
    let updater = store.clone();
    let update = Closure::wrap(Box::new(move |mutator: Function| updater.update(&mutator))
        as Box<dyn FnMut(Function) -> Result<(), JsValue>>);
    set(&output, "update", &update.into_js_value())?;
    let setter = store;
    let set_state = Closure::wrap(Box::new(move |next: JsValue| setter.set(next))
        as Box<dyn Fn(JsValue) -> Result<(), JsValue>>);
    set(&output, "set", &set_state.into_js_value())?;
    Ok(output.into())
}

fn engine_instance_face(
    store: Rc<BrowserSnapshotStore>,
    action_declarations: &JsValue,
) -> Result<JsValue, JsValue> {
    let store_face = snapshot_store_face(store.clone())?;
    let actions = Object::new();
    let invoke_store = store.clone();
    let declarations = action_declarations.clone();
    let invoke = Closure::wrap(
        Box::new(move |key: String, args: Array| -> Result<(), JsValue> {
            let action =
                Reflect::get(&declarations, &JsValue::from_str(&key))?.dyn_into::<Function>()?;
            let recipe = Closure::wrap(Box::new(move |draft: JsValue| -> Result<(), JsValue> {
                let values = Array::new();
                values.push(&draft);
                for value in args.iter() {
                    values.push(&value);
                }
                action.apply(&JsValue::UNDEFINED, &values)?;
                Ok(())
            })
                as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
            invoke_store.update(&recipe.into_js_value().unchecked_into())
        }) as Box<dyn FnMut(String, Array) -> Result<(), JsValue>>,
    );
    let invoke: JsValue = invoke.into_js_value();
    for key in Object::keys(&Object::from(action_declarations.clone()))
        .iter()
        .filter_map(|key| key.as_string())
    {
        let factory = Function::new_with_args(
            "invoke, key",
            "return function(...args) { return invoke(key, args) }",
        );
        let action = factory.call2(&JsValue::UNDEFINED, &invoke, &JsValue::from_str(&key))?;
        set(&actions, &key, &action)?;
    }
    let output = Object::new();
    set(&output, "actions", &actions)?;
    set(&output, "store", &store_face)?;
    set(
        &output,
        "getSnapshot",
        &required(&store_face, "getSnapshot")?,
    )?;
    set(&output, "subscribe", &required(&store_face, "subscribe")?)?;
    let clear_store = store;
    let clear = Closure::wrap(Box::new(move || clear_store.clear_persisted()) as Box<dyn FnMut()>);
    set(&output, "clearPersisted", &clear.into_js_value())?;
    Ok(output.into())
}

fn rehydrate(name: &str) -> Option<JsValue> {
    let raw = match storage_call_result("getItem", &[JsValue::from_str(name)])? {
        Ok(raw) => raw,
        Err(error) => {
            console_error(
                &format!("snapshot store '{name}' rehydration failed:"),
                &error,
            );
            return None;
        }
    };
    if raw.is_null() || raw.is_undefined() {
        return None;
    }
    let raw = raw.as_string()?;
    match JSON::parse(&raw) {
        Ok(value) => Some(value),
        Err(error) => {
            console_error(
                &format!("snapshot store '{name}' rehydration failed:"),
                &error,
            );
            None
        }
    }
}

fn persist(name: &str, value: &JsValue) {
    match JSON::stringify(value) {
        Ok(value) => {
            if let Some(Err(error)) =
                storage_call_result("setItem", &[JsValue::from_str(name), value.into()])
            {
                console_error(
                    &format!("snapshot store '{name}' persistence failed:"),
                    &error,
                );
            }
        }
        Err(error) => console_error(
            &format!("snapshot store '{name}' persistence failed:"),
            &error,
        ),
    }
}

fn storage_call(method: &str, arguments: &[JsValue]) -> Option<JsValue> {
    storage_call_result(method, arguments)?.ok()
}

fn storage_call_result(method: &str, arguments: &[JsValue]) -> Option<Result<JsValue, JsValue>> {
    let storage = Reflect::get(&js_sys::global(), &JsValue::from_str("localStorage")).ok()?;
    if storage.is_undefined() || storage.is_null() {
        return None;
    }
    Some(call_method(&storage, method, arguments))
}

fn deep_freeze(value: &JsValue) -> Result<(), JsValue> {
    if !value.is_object() || value.is_null() || Object::is_frozen(&Object::from(value.clone())) {
        return Ok(());
    }
    let object = Object::from(value.clone());
    Object::freeze(&object);
    let keys = Reflect::own_keys(&object)?;
    for index in 0..keys.length() {
        deep_freeze(&Reflect::get(&object, &keys.get(index))?)?;
    }
    Ok(())
}

fn is_production() -> bool {
    !cfg!(debug_assertions)
}

fn console_error(message: &str, error: &JsValue) {
    if let Ok(console) = Reflect::get(&js_sys::global(), &JsValue::from_str("console")) {
        let _ = call_method(
            &console,
            "error",
            &[JsValue::from_str(message), error.clone()],
        );
    }
}

fn required(value: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    let value = Reflect::get(value, &JsValue::from_str(key))?;
    if value.is_undefined() || value.is_null() {
        Err(js_sys::Error::new(&format!("snapshot Store requires {key:?}")).into())
    } else {
        Ok(value)
    }
}

fn optional(value: &JsValue, key: &str) -> Result<Option<JsValue>, JsValue> {
    let value = Reflect::get(value, &JsValue::from_str(key))?;
    Ok((!value.is_undefined()).then_some(value))
}

fn optional_string(value: &JsValue, key: &str) -> Result<Option<String>, JsValue> {
    let Some(value) = optional(value, key)? else {
        return Ok(None);
    };
    value.as_string().map(Some).ok_or_else(|| {
        js_sys::Error::new(&format!("snapshot Store {key:?} must be a string")).into()
    })
}

fn required_string(value: &JsValue, key: &str) -> Result<String, JsValue> {
    optional_string(value, key)?
        .ok_or_else(|| js_sys::Error::new(&format!("snapshot Store {key:?} is required")).into())
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(object, &JsValue::from_str(key), value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new(&format!("failed to set {key:?}")).into())
    }
}

fn call_method(value: &JsValue, method: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = required(value, method)?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}
