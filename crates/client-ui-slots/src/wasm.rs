//! Browser bindings for the portable Slot registry.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    fmt,
    rc::Rc,
};

use indexmap::IndexMap;
use js_sys::{Array, Function, Object, Promise, Reflect, WeakMap};
use serde::Serialize;
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;

use crate::{
    SlotCore, SlotEntry, SlotEntryId, SlotKind, SlotMicrotaskScheduler, SlotName,
    SlotRegistrationOptions, SlotScope, SlotSpec, SlotStoreDeclaration, StoreHandleId,
};

#[derive(Clone)]
struct JsEntryPayload {
    stored: JsValue,
}

impl fmt::Debug for JsEntryPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JsEntryPayload")
            .finish_non_exhaustive()
    }
}

type BrowserCore = SlotCore<JsEntryPayload, JsValue, JsValue>;
type EntrySnapshot = Rc<Vec<Rc<SlotEntry<JsEntryPayload, JsValue>>>>;

#[derive(Clone, Copy, Debug, Default)]
struct BrowserMicrotasks;

impl SlotMicrotaskScheduler for BrowserMicrotasks {
    fn queue(&self, callback: Box<dyn FnOnce()>) {
        wasm_bindgen_futures::spawn_local(async move {
            let _ = JsFuture::from(Promise::resolve(&JsValue::UNDEFINED)).await;
            callback();
        });
    }
}

/// JavaScript-facing Slot registry backed entirely by Rust/WASM.
#[wasm_bindgen(js_name = SlotCore)]
pub struct WasmSlotCore {
    core: Rc<BrowserCore>,
    entry_ids: WeakMap,
    store_ids: WeakMap,
    next_store_id: Cell<u64>,
    entry_cache: RefCell<HashMap<SlotName, (EntrySnapshot, Array)>>,
}

#[wasm_bindgen(js_class = SlotCore)]
impl WasmSlotCore {
    /// Creates the a-priori root declaration and browser microtask scheduler.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            core: SlotCore::new(Rc::new(BrowserMicrotasks)),
            entry_ids: WeakMap::new(),
            store_ids: WeakMap::new(),
            next_store_id: Cell::new(0),
            entry_cache: RefCell::new(HashMap::new()),
        }
    }

    /// Registers one entry and returns its idempotent disposer.
    ///
    /// # Errors
    ///
    /// Returns the source-compatible load-time validation diagnostic.
    #[allow(clippy::needless_pass_by_value)]
    pub fn register(&self, options: JsValue, component: JsValue) -> Result<Function, JsValue> {
        let (options, stored) = self.parse_registration(&options, &component)?;
        let stored_value: JsValue = stored.clone().into();
        let registration = self
            .core
            .register(
                options,
                JsEntryPayload {
                    stored: stored_value,
                },
            )
            .map_err(|error| js_sys::Error::new(&error.to_string()))?;
        self.entry_ids.set(
            &stored,
            &JsValue::from_f64(u64_to_f64(registration.entry_id().get())),
        );
        let disposer = Closure::wrap(Box::new(move || registration.dispose()) as Box<dyn FnMut()>);
        Ok(disposer.into_js_value().unchecked_into())
    }

    /// Whether a previously returned stored entry remains in the ledger.
    #[wasm_bindgen(js_name = isLive)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn is_live(&self, entry: JsValue) -> bool {
        self.entry_id(&entry)
            .and_then(|id| self.core.entry_by_id(id))
            .is_some_and(|entry| self.core.is_live(&entry))
    }

    /// Stable raw entry-array reference until mutation.
    pub fn entries(&self, key: String) -> Array {
        let key = SlotName::new(key);
        let snapshot = self.core.entries(&key);
        let mut cache = self.entry_cache.borrow_mut();
        if let Some((current, array)) = cache.get(&key)
            && Rc::ptr_eq(current, &snapshot)
        {
            return array.clone();
        }
        let array = Array::new();
        for entry in snapshot.iter() {
            array.push(&entry.payload.stored);
        }
        cache.insert(key, (snapshot, array.clone()));
        array
    }

    /// Fresh projection of active shadowing winners, or every chain entry.
    #[wasm_bindgen(js_name = entriesOfSlot)]
    pub fn entries_of_slot(&self, key: String) -> Array {
        let output = Array::new();
        for entry in self.core.entries_of_slot(&SlotName::new(key)) {
            output.push(&entry.payload.stored);
        }
        output
    }

    /// Wide runtime spec lookup.
    ///
    /// # Errors
    ///
    /// Returns JavaScript object-construction failures.
    #[wasm_bindgen(js_name = specDynamic)]
    pub fn spec_dynamic(&self, key: String) -> Result<JsValue, JsValue> {
        self.core
            .spec(&SlotName::new(key))
            .map(|spec| spec_to_js(&spec))
            .transpose()
            .map(|value| value.unwrap_or(JsValue::UNDEFINED))
    }

    /// Alias retained for statically typed source callers.
    ///
    /// # Errors
    ///
    /// Returns JavaScript object-construction failures.
    pub fn spec(&self, key: String) -> Result<JsValue, JsValue> {
        self.spec_dynamic(key)
    }

    /// Monotonic declaration lifetime.
    #[wasm_bindgen(js_name = declarationEpoch)]
    pub fn declaration_epoch(&self, key: String) -> f64 {
        u64_to_f64(self.core.declaration_epoch(&SlotName::new(key)))
    }

    /// Monotonic per-key mutation version.
    #[wasm_bindgen(js_name = getVersion)]
    pub fn version(&self, key: String) -> f64 {
        u64_to_f64(self.core.version(&SlotName::new(key)))
    }

    /// Subscribes to microtask-batched mutations of one key.
    pub fn subscribe(&self, key: String, listener: Function) -> Function {
        let subscription = self.core.subscribe(
            SlotName::new(key),
            Rc::new(move || {
                call_or_throw(listener.call0(&JsValue::UNDEFINED));
            }),
        );
        disposer(move || subscription.dispose())
    }

    /// Subscribes synchronously to declaration lifetime boundaries.
    #[wasm_bindgen(js_name = subscribeDeclaration)]
    pub fn subscribe_declaration(&self, key: String, listener: Function) -> Function {
        let subscription = self.core.subscribe_declaration(
            SlotName::new(key),
            Rc::new(move || {
                call_or_throw(listener.call0(&JsValue::UNDEFINED));
            }),
        );
        disposer(move || subscription.dispose())
    }

    /// Observes every mutation synchronously.
    #[wasm_bindgen(js_name = onMutate)]
    pub fn on_mutate(&self, listener: Function) -> Function {
        let subscription = self.core.on_mutate(Rc::new(move |key| {
            call_or_throw(listener.call1(&JsValue::UNDEFINED, &JsValue::from_str(key.as_str())));
        }));
        disposer(move || subscription.dispose())
    }

    /// Observes every contained entry failure synchronously.
    #[wasm_bindgen(js_name = onEntryError)]
    pub fn on_entry_error(&self, listener: Function) -> Function {
        let subscription =
            self.core
                .on_entry_error(Rc::new(move |key, entry, error, abdicated| {
                    let info = Object::new();
                    let _ = Reflect::set(
                        &info,
                        &JsValue::from_str("abdicated"),
                        &JsValue::from_bool(abdicated),
                    );
                    call_or_throw(listener.call4(
                        &JsValue::UNDEFINED,
                        &JsValue::from_str(key.as_str()),
                        &entry.payload.stored,
                        error,
                        &info,
                    ));
                }));
        disposer(move || subscription.dispose())
    }

    /// Reports one boundary crash and optionally retires the entry from its cell.
    ///
    /// # Errors
    ///
    /// Rejects an entry object not created by this registry.
    #[wasm_bindgen(js_name = reportEntryError)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn report_entry_error(
        &self,
        key: String,
        entry: JsValue,
        error: JsValue,
        info: JsValue,
    ) -> Result<(), JsValue> {
        let id = self
            .entry_id(&entry)
            .ok_or_else(|| js_sys::Error::new("Slot entry is not owned by this registry"))?;
        let entry = self
            .core
            .entry_by_id(id)
            .ok_or_else(|| js_sys::Error::new("Slot entry is no longer registered"))?;
        let abdicate = Reflect::get(&info, &JsValue::from_str("abdicate"))?
            .as_bool()
            .unwrap_or(false);
        self.core
            .report_entry_error(&SlotName::new(key), &entry, &error, abdicate);
        Ok(())
    }

    /// Exports the live declaration tree without executable values.
    ///
    /// # Errors
    ///
    /// Returns JavaScript serialization failures.
    #[allow(clippy::needless_pass_by_value)]
    pub fn snapshot(&self, root: Option<String>) -> Result<JsValue, JsValue> {
        to_js_json(
            &self
                .core
                .snapshot(root.as_deref().map(SlotName::new).as_ref()),
        )
    }
}

impl Default for WasmSlotCore {
    fn default() -> Self {
        Self::new()
    }
}

impl WasmSlotCore {
    fn parse_registration(
        &self,
        source: &JsValue,
        component: &JsValue,
    ) -> Result<(SlotRegistrationOptions<JsValue>, Object), JsValue> {
        let name = required_string(source, "name")?;
        let mut options = SlotRegistrationOptions::new(name);
        options.key = optional_string(source, "key")?;
        options.id = optional_string(source, "id")?;
        options.order = optional_number(source, "order")?;
        options.priority = optional_number(source, "priority")?;
        options.locale = optional_string(source, "locale")?;
        options.registrant = optional_string(source, "registrant")?;
        let select = optional(source, "select")?;
        options.has_selector = select.is_some();
        let children = optional(source, "children")?;
        options.children = children
            .as_ref()
            .map(parse_children)
            .transpose()?
            .unwrap_or_default();
        let store = optional(source, "store")?;
        options.store = store
            .as_ref()
            .map(|store| self.store_declaration(store))
            .transpose()?;

        let stored = Object::new();
        set(&stored, "component", component)?;
        let stored_options = Object::new();
        copy_defined(source, &stored_options, "key")?;
        copy_defined(source, &stored_options, "id")?;
        copy_defined(source, &stored_options, "order")?;
        copy_defined(source, &stored_options, "label")?;
        copy_defined(source, &stored_options, "priority")?;
        set(&stored, "options", &stored_options)?;
        for key in [
            "select",
            "inject",
            "children",
            "store",
            "locale",
            "registrant",
        ] {
            copy_defined(source, &stored, key)?;
        }
        Ok((options, stored))
    }

    fn store_declaration(&self, value: &JsValue) -> Result<SlotStoreDeclaration, JsValue> {
        if value.is_function() {
            return Ok(SlotStoreDeclaration::Factory);
        }
        if !value.is_object() || value.is_null() {
            return Err(js_sys::Error::new("Slot store must be a handle object or factory").into());
        }
        let object = Object::from(value.clone());
        let current = self.store_ids.get(&object);
        if let Some(id) = f64_to_u64(&current) {
            return Ok(SlotStoreDeclaration::Shared(StoreHandleId::new(id)));
        }
        let id = self.next_store_id.get().wrapping_add(1);
        self.next_store_id.set(id);
        self.store_ids
            .set(&object, &JsValue::from_f64(u64_to_f64(id)));
        Ok(SlotStoreDeclaration::Shared(StoreHandleId::new(id)))
    }

    fn entry_id(&self, value: &JsValue) -> Option<SlotEntryId> {
        if !value.is_object() || value.is_null() {
            return None;
        }
        let object = Object::from(value.clone());
        f64_to_u64(&self.entry_ids.get(&object)).map(SlotEntryId::new)
    }
}

/// Resolves a string or late-bound label thunk.
///
/// # Errors
///
/// Returns a thrown label callback error.
#[wasm_bindgen(js_name = resolveSlotLabel)]
#[allow(clippy::needless_pass_by_value)]
pub fn resolve_slot_label(label: JsValue) -> Result<JsValue, JsValue> {
    match label.dyn_ref::<Function>() {
        Some(label) => label.call0(&JsValue::UNDEFINED),
        None => Ok(label),
    }
}

/// Creates the stale-authorization JavaScript error used by render bindings.
#[wasm_bindgen(js_name = staleAuthorizationError)]
pub fn stale_authorization_error() -> js_sys::Error {
    named_error(
        "StaleAuthorizationError",
        "slot render authorization is stale",
    )
}

/// Creates the undeclared-child JavaScript error used by render bindings.
#[wasm_bindgen(js_name = slotOwnershipError)]
pub fn slot_ownership_error() -> js_sys::Error {
    named_error(
        "SlotOwnershipError",
        "slot is outside the declaring entry's children authorization",
    )
}

fn parse_children(value: &JsValue) -> Result<IndexMap<SlotName, SlotSpec<JsValue>>, JsValue> {
    let object = Object::from(value.clone());
    let mut output = IndexMap::new();
    for key in Object::keys(&object)
        .iter()
        .filter_map(|key| key.as_string())
    {
        let value = Reflect::get(&object, &JsValue::from_str(&key))?;
        output.insert(SlotName::new(key), parse_spec(&value)?);
    }
    Ok(output)
}

fn parse_spec(value: &JsValue) -> Result<SlotSpec<JsValue>, JsValue> {
    let kind = match required_string(value, "kind")?.as_str() {
        "single" => SlotKind::Single,
        "list" => SlotKind::List,
        "keyed" => SlotKind::Keyed,
        "chain" => SlotKind::Chain,
        kind => return Err(js_sys::Error::new(&format!("unknown Slot kind {kind:?}")).into()),
    };
    let scope = match required_string(value, "scope")?.as_str() {
        "root" => SlotScope::Root,
        "session-maybe" => SlotScope::SessionMaybe,
        "session" => SlotScope::Session,
        scope => return Err(js_sys::Error::new(&format!("unknown Slot scope {scope:?}")).into()),
    };
    Ok(SlotSpec {
        kind,
        scope,
        inject: optional(value, "inject")?,
    })
}

fn spec_to_js(spec: &SlotSpec<JsValue>) -> Result<JsValue, JsValue> {
    let value = Object::new();
    set(
        &value,
        "kind",
        &JsValue::from_str(match spec.kind {
            SlotKind::Single => "single",
            SlotKind::List => "list",
            SlotKind::Keyed => "keyed",
            SlotKind::Chain => "chain",
        }),
    )?;
    set(
        &value,
        "scope",
        &JsValue::from_str(match spec.scope {
            SlotScope::Root => "root",
            SlotScope::SessionMaybe => "session-maybe",
            SlotScope::Session => "session",
        }),
    )?;
    if let Some(inject) = &spec.inject {
        set(&value, "inject", inject)?;
    }
    Ok(value.into())
}

fn optional(value: &JsValue, key: &str) -> Result<Option<JsValue>, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    Ok((!property.is_undefined()).then_some(property))
}

fn optional_string(value: &JsValue, key: &str) -> Result<Option<String>, JsValue> {
    let Some(value) = optional(value, key)? else {
        return Ok(None);
    };
    value
        .as_string()
        .map(Some)
        .ok_or_else(|| js_sys::Error::new(&format!("Slot option {key:?} must be a string")).into())
}

fn optional_number(value: &JsValue, key: &str) -> Result<Option<f64>, JsValue> {
    let Some(value) = optional(value, key)? else {
        return Ok(None);
    };
    value
        .as_f64()
        .map(Some)
        .ok_or_else(|| js_sys::Error::new(&format!("Slot option {key:?} must be a number")).into())
}

fn required_string(value: &JsValue, key: &str) -> Result<String, JsValue> {
    optional_string(value, key)?
        .ok_or_else(|| js_sys::Error::new(&format!("Slot option {key:?} is required")).into())
}

fn copy_defined(source: &JsValue, target: &Object, key: &str) -> Result<(), JsValue> {
    if let Some(value) = optional(source, key)? {
        set(target, key, &value)?;
    }
    Ok(())
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(object, &JsValue::from_str(key), value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new(&format!("failed to set {key:?}")).into())
    }
}

fn disposer(callback: impl FnMut() + 'static) -> Function {
    Closure::wrap(Box::new(callback) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into()
}

fn named_error(name: &str, message: &str) -> js_sys::Error {
    let error = js_sys::Error::new(message);
    error.set_name(name);
    error
}

fn u64_to_f64(value: u64) -> f64 {
    value
        .to_string()
        .parse()
        .expect("u64 decimal text is a finite JavaScript number")
}

fn f64_to_u64(value: &JsValue) -> Option<u64> {
    let value = value.as_f64()?;
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
        return None;
    }
    value.to_string().parse().ok()
}

fn to_js_json(value: &impl Serialize) -> Result<JsValue, JsValue> {
    value
        .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
        .map_err(|error| js_sys::Error::new(&error.to_string()).into())
}

fn call_or_throw(result: Result<JsValue, JsValue>) {
    if let Err(error) = result {
        wasm_bindgen::throw_val(error);
    }
}
