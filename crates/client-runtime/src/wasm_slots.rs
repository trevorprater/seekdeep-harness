//! Browser Slot Service facade with caller-owned Cordis effects.

use std::{any::Any, cell::RefCell, collections::HashMap, fmt, rc::Rc};

use indexmap::IndexMap;
use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_ui_slots::{
    SlotEntry, SlotKind, SlotMicrotaskScheduler, SlotName, SlotRegistrationOptions, SlotScope,
    SlotSpec, SlotStoreFactory, SlotStoreInstance,
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;

use crate::{
    ClientRootRenderer, ClientSlotError, ClientSlotRegistry, RuntimeDisposer, RuntimeLocaleFace,
    RuntimeSlotPayload, RuntimeStandardFace, RuntimeStoreDeclaration, SlotEffectBatch,
    SlotInjectionFailureReporter,
};

type BrowserRegistry = ClientSlotRegistry<BrowserEntry, JsValue, JsValue, JsValue, JsValue>;
type BrowserSlotEntry = SlotEntry<RuntimeSlotPayload<BrowserEntry>, JsValue>;
type EntrySnapshot = Rc<Vec<Rc<BrowserSlotEntry>>>;

#[derive(Clone)]
struct BrowserEntry {
    stored: JsValue,
}

impl fmt::Debug for BrowserEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserEntry")
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct JsStandardFace(JsValue);

impl RuntimeStandardFace for JsStandardFace {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn snapshot(&self) -> serde_json::Value {
        serde_wasm_bindgen::from_value(self.0.clone()).unwrap_or(serde_json::Value::Null)
    }
}

#[derive(Clone)]
struct JsLocaleFace(JsValue);

impl RuntimeLocaleFace for JsLocaleFace {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn revision(&self) -> u64 {
        let snapshot = call_method(&self.0, "getSnapshot", &[]).unwrap_or(JsValue::UNDEFINED);
        Reflect::get(&snapshot, &JsValue::from_str("revision"))
            .ok()
            .and_then(|value| value.as_f64())
            .and_then(f64_to_u64)
            .unwrap_or(0)
    }
}

struct JsStoreHandle {
    value: JsValue,
}

impl SlotStoreFactory for JsStoreHandle {
    fn create(&self, scope_key: Option<&str>) -> Rc<dyn SlotStoreInstance> {
        let arguments = scope_key
            .map(|scope| vec![JsValue::from_str(scope)])
            .unwrap_or_default();
        let value = call_method(&self.value, "create", &arguments)
            .unwrap_or_else(|error| wasm_bindgen::throw_val(error));
        Rc::new(JsStoreInstance { value })
    }
}

struct JsStoreInstance {
    value: JsValue,
}

impl SlotStoreInstance for JsStoreInstance {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn snapshot(&self) -> serde_json::Value {
        call_method(&self.value, "getSnapshot", &[])
            .ok()
            .and_then(|value| serde_wasm_bindgen::from_value(value).ok())
            .unwrap_or(serde_json::Value::Null)
    }

    fn subscribe(&self, listener: Rc<dyn Fn()>) -> Box<dyn Fn()> {
        let callback = Closure::wrap(Box::new(move || listener()) as Box<dyn FnMut()>);
        let disposer = call_method(&self.value, "subscribe", &[callback.into_js_value()])
            .ok()
            .and_then(|value| value.dyn_into::<Function>().ok());
        Box::new(move || {
            if let Some(disposer) = &disposer {
                let _ = disposer.call0(&JsValue::UNDEFINED);
            }
        })
    }

    fn clear_persisted(&self) {
        let _ = call_method(&self.value, "clearPersisted", &[]);
    }
}

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

struct BrowserReporter;

impl SlotInjectionFailureReporter for BrowserReporter {
    fn report_later(&self, error: ClientSlotError) {
        if error.to_string().starts_with("INACTIVE_EFFECT:") {
            return;
        }
        wasm_bindgen_futures::spawn_local(async move {
            let _ = JsFuture::from(Promise::resolve(&JsValue::UNDEFINED)).await;
            wasm_bindgen::throw_val(js_sys::Error::new(&error.to_string()).into());
        });
    }
}

struct BrowserRenderer {
    renderer: JsValue,
    state: std::rc::Weak<BrowserState>,
}

impl ClientRootRenderer<BrowserEntry, JsValue, JsValue, JsValue, JsValue> for BrowserRenderer {
    fn render_root(&self, _host: &BrowserRegistry, owner: JsValue) -> JsValue {
        let state = self
            .state
            .upgrade()
            .unwrap_or_else(|| wasm_bindgen::throw_str("Client Slot Service was disposed"));
        let host = host_face(&state).unwrap_or_else(|error| wasm_bindgen::throw_val(error));
        call_method(&self.renderer, "renderRoot", &[host, owner])
            .unwrap_or_else(|error| wasm_bindgen::throw_val(error))
    }
}

struct BrowserState {
    registry: Rc<BrowserRegistry>,
    store_handles: RefCell<Vec<(JsValue, Rc<JsStoreHandle>)>>,
    entry_cache: RefCell<HashMap<SlotName, (EntrySnapshot, Array)>>,
}

/// Browser Client Slot Service backed by the portable Rust ledger.
#[wasm_bindgen(js_name = ClientSlotRegistry)]
pub struct WasmClientSlotRegistry {
    state: Rc<BrowserState>,
}

#[wasm_bindgen(js_class = ClientSlotRegistry)]
impl WasmClientSlotRegistry {
    /// Creates the Service and an optional synchronous `slots/changed` sink.
    #[wasm_bindgen(constructor)]
    pub fn new(on_changed: Option<Function>) -> Self {
        let on_changed = Rc::new(move |key: &SlotName| {
            if let Some(listener) = &on_changed {
                call_or_throw(
                    listener.call1(&JsValue::UNDEFINED, &JsValue::from_str(key.as_str())),
                );
            }
        });
        let registry = ClientSlotRegistry::new(Rc::new(BrowserMicrotasks), on_changed);
        Self {
            state: Rc::new(BrowserState {
                registry,
                store_handles: RefCell::new(Vec::new()),
                entry_cache: RefCell::new(HashMap::new()),
            }),
        }
    }

    /// Returns a caller-bound Service face whose mutations enter `caller.effect`.
    ///
    /// # Errors
    ///
    /// Returns JavaScript object-construction failures.
    #[wasm_bindgen(js_name = faceFor)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn face_for(&self, caller: JsValue) -> Result<JsValue, JsValue> {
        service_face(&self.state, &caller)
    }

    /// Installs the Session Host face.
    #[wasm_bindgen(js_name = installSessions)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn install_sessions(&self, face: JsValue) -> Function {
        let disposer = self
            .state
            .registry
            .install_sessions(Rc::new(JsStandardFace(face)));
        runtime_disposer(disposer)
    }

    /// Installs the Workspace Host face.
    #[wasm_bindgen(js_name = installWorkspaces)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn install_workspaces(&self, face: JsValue) -> Function {
        let disposer = self
            .state
            .registry
            .install_workspaces(Rc::new(JsStandardFace(face)));
        runtime_disposer(disposer)
    }

    /// Stable raw entry snapshot until mutation.
    pub fn entries(&self, key: String) -> Array {
        self.state.entries(&SlotName::new(key))
    }

    /// Active winner per shadowing cell.
    #[wasm_bindgen(js_name = entriesOfSlot)]
    pub fn entries_of_slot(&self, key: String) -> Array {
        self.state.entries_of_slot(&SlotName::new(key))
    }

    /// Runtime spec lookup.
    ///
    /// # Errors
    ///
    /// Returns JavaScript object-construction failures.
    pub fn spec(&self, key: String) -> Result<JsValue, JsValue> {
        self.state
            .registry
            .core()
            .spec(&SlotName::new(key))
            .map(|spec| spec_to_js(&spec))
            .transpose()
            .map(|value| value.unwrap_or(JsValue::UNDEFINED))
    }

    /// JSON-safe live topology.
    ///
    /// # Errors
    ///
    /// Returns JavaScript serialization failures.
    #[allow(clippy::needless_pass_by_value)]
    pub fn snapshot(&self, root: Option<String>) -> Result<JsValue, JsValue> {
        serde_wasm_bindgen::to_value(
            &self
                .state
                .registry
                .core()
                .snapshot(root.as_deref().map(SlotName::new).as_ref()),
        )
        .map_err(|error| js_sys::Error::new(&error.to_string()).into())
    }

    /// Clears persisted Store state for one dead Session.
    #[wasm_bindgen(js_name = pruneStoreScope)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn prune_store_scope(&self, session_id: String) {
        self.state.registry.prune_store_scope(&session_id);
    }
}

impl Default for WasmClientSlotRegistry {
    fn default() -> Self {
        Self::new(None)
    }
}

impl BrowserState {
    fn register(
        self: &Rc<Self>,
        source: &JsValue,
        component: &JsValue,
        caller: &JsValue,
    ) -> Result<RuntimeDisposer, ClientSlotError> {
        let source = clone_object(source).map_err(js_client_error)?;
        if optional(&source, "registrant")
            .map_err(js_client_error)?
            .is_none()
            && let Some(name) = Reflect::get(caller, &JsValue::from_str("fiber"))
                .ok()
                .and_then(|fiber| Reflect::get(&fiber, &JsValue::from_str("name")).ok())
                .and_then(|name| name.as_string())
        {
            set(&source, "registrant", &JsValue::from_str(&name)).map_err(js_client_error)?;
        }
        let store = optional(&source, "store").map_err(js_client_error)?;
        let store = store
            .map(|store| {
                let handle = if store.is_function() {
                    store
                        .dyn_into::<Function>()
                        .and_then(|factory| factory.call0(&JsValue::UNDEFINED))
                        .map_err(js_client_error)?
                } else {
                    store
                };
                set(&source, "store", &handle).map_err(js_client_error)?;
                Ok(RuntimeStoreDeclaration::Shared(self.store_handle(handle)?))
            })
            .transpose()?;
        let options = parse_options(&source).map_err(js_client_error)?;
        let stored = stored_entry(&source, component).map_err(js_client_error)?;
        self.registry
            .register(options, BrowserEntry { stored }, store)
    }

    fn store_handle(&self, value: JsValue) -> Result<Rc<dyn SlotStoreFactory>, ClientSlotError> {
        if !value.is_object() || value.is_null() || !has_function(&value, "create") {
            return Err(ClientSlotError::new(
                "Slot store must be a handle object or factory",
            ));
        }
        if let Some((_, handle)) = self
            .store_handles
            .borrow()
            .iter()
            .find(|(candidate, _)| Object::is(candidate, &value))
        {
            return Ok(handle.clone());
        }
        let handle = Rc::new(JsStoreHandle {
            value: value.clone(),
        });
        self.store_handles
            .borrow_mut()
            .push((value, handle.clone()));
        Ok(handle)
    }

    fn entries(&self, key: &SlotName) -> Array {
        let snapshot = self.registry.core().entries(key);
        let mut cache = self.entry_cache.borrow_mut();
        if let Some((current, array)) = cache.get(key)
            && Rc::ptr_eq(current, &snapshot)
        {
            return array.clone();
        }
        let array = Array::new();
        for entry in snapshot.iter() {
            array.push(&entry.payload.component.stored);
        }
        cache.insert(key.clone(), (snapshot, array.clone()));
        array
    }

    fn entries_of_slot(&self, key: &SlotName) -> Array {
        let output = Array::new();
        for entry in self.registry.core().entries_of_slot(key) {
            output.push(&entry.payload.component.stored);
        }
        output
    }

    fn entry_for(&self, value: &JsValue) -> Option<Rc<BrowserSlotEntry>> {
        find_entry_recursive(self.registry.core(), value)
    }
}

#[allow(clippy::too_many_lines)]
fn service_face(state: &Rc<BrowserState>, caller: &JsValue) -> Result<JsValue, JsValue> {
    let face = Object::new();
    let register_state = state.clone();
    let register_caller = caller.clone();
    let register = Closure::wrap(Box::new(
        move |options: JsValue, component: JsValue| -> Result<JsValue, JsValue> {
            let state = register_state.clone();
            let caller = register_caller.clone();
            let installer = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
                let disposer = state
                    .register(&options, &component, &caller)
                    .map_err(|error| js_sys::Error::new(&error.to_string()))?;
                Ok(runtime_disposer(disposer).into())
            })
                as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
            call_method(
                &register_caller,
                "effect",
                &[
                    installer.into_js_value(),
                    JsValue::from_str("slots.register()"),
                ],
            )
        },
    )
        as Box<dyn FnMut(JsValue, JsValue) -> Result<JsValue, JsValue>>);
    set(&face, "register", &register.into_js_value())?;

    let inject_state = state.clone();
    let inject_caller = caller.clone();
    let inject = Closure::wrap(Box::new(
        move |key: String, callback: Function| -> Result<JsValue, JsValue> {
            let state = inject_state.clone();
            let caller = inject_caller.clone();
            let controller_label = key.clone();
            let installer = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
                let callback = callback.clone();
                let declaration_caller = caller.clone();
                let declaration_key = key.clone();
                let setup = Rc::new(move |batch: &mut SlotEffectBatch| {
                    let returned = call_method(
                        &declaration_caller,
                        "effect",
                        &[
                            callback.clone().into(),
                            JsValue::from_str(&format!(
                                "slots.inject({declaration_key:?}): declaration"
                            )),
                        ],
                    )
                    .map_err(|error| {
                        let message = js_error_text(&error);
                        if Reflect::get(&error, &JsValue::from_str("code"))
                            .ok()
                            .and_then(|value| value.as_string())
                            .as_deref()
                            == Some("INACTIVE_EFFECT")
                        {
                            ClientSlotError::new(format!("INACTIVE_EFFECT: {message}"))
                        } else {
                            ClientSlotError::new(message)
                        }
                    })?;
                    let disposer = returned.dyn_into::<Function>().map_err(|_| {
                        ClientSlotError::new("slots.inject declaration effect returned no disposer")
                    })?;
                    batch.push(RuntimeDisposer::new(move || {
                        let _ = disposer.call0(&JsValue::UNDEFINED);
                    }));
                    Ok(())
                });
                let injection = state
                    .registry
                    .inject(SlotName::new(&key), setup, Rc::new(BrowserReporter))
                    .map_err(|error| js_sys::Error::new(&error.to_string()))?;
                let disposer =
                    Closure::wrap(Box::new(move || injection.dispose()) as Box<dyn FnMut()>);
                Ok(disposer.into_js_value())
            })
                as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
            call_method(
                &inject_caller,
                "effect",
                &[
                    installer.into_js_value(),
                    JsValue::from_str(&format!("slots.inject({controller_label:?})")),
                ],
            )
        },
    )
        as Box<dyn FnMut(String, Function) -> Result<JsValue, JsValue>>);
    set(&face, "inject", &inject.into_js_value())?;

    let install_state = state.clone();
    let install_caller = caller.clone();
    let install = Closure::wrap(Box::new(move |renderer: JsValue| -> Result<(), JsValue> {
        let renderer: Rc<dyn ClientRootRenderer<BrowserEntry, JsValue, JsValue, JsValue, JsValue>> =
            Rc::new(BrowserRenderer {
                renderer,
                state: Rc::downgrade(&install_state),
            });
        let disposer = install_state
            .registry
            .install_renderer(renderer)
            .map_err(|error| js_sys::Error::new(&error.to_string()))?;
        own_runtime_disposer(&install_caller, "slots.install()", disposer)
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    set(&face, "install", &install.into_js_value())?;

    let locale_state = state.clone();
    let locale_caller = caller.clone();
    let install_locale = Closure::wrap(Box::new(move |locale: JsValue| -> Result<(), JsValue> {
        let disposer = locale_state
            .registry
            .install_locale(Rc::new(JsLocaleFace(locale)))
            .map_err(|error| js_sys::Error::new(&error.to_string()))?;
        own_runtime_disposer(&locale_caller, "slots.installLocale()", disposer)
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    set(&face, "installLocale", &install_locale.into_js_value())?;

    let render_state = state.clone();
    let render_slot = Closure::wrap(Box::new(move |key: String, owner: JsValue| {
        render_state
            .registry
            .render_slot(&SlotName::new(key), owner)
            .map_err(|error| js_sys::Error::new(&error.to_string()).into())
    })
        as Box<dyn FnMut(String, JsValue) -> Result<JsValue, JsValue>>);
    set(&face, "renderSlot", &render_slot.into_js_value())?;

    let prune_state = state.clone();
    let prune = Closure::wrap(Box::new(move |session_id: String| {
        prune_state.registry.prune_store_scope(&session_id);
    }) as Box<dyn FnMut(String)>);
    set(&face, "pruneStoreScope", &prune.into_js_value())?;
    let error_core = state.registry.core().clone();
    let on_entry_error = Closure::wrap(Box::new(move |listener: Function| -> Function {
        let subscription =
            error_core.on_entry_error(Rc::new(move |key, entry, error, abdicated| {
                let info = Object::new();
                let _ = set(&info, "abdicated", &JsValue::from_bool(abdicated));
                call_or_throw(listener.call4(
                    &JsValue::UNDEFINED,
                    &JsValue::from_str(key.as_str()),
                    &entry.payload.component.stored,
                    error,
                    &info,
                ));
            }));
        Closure::wrap(Box::new(move || subscription.dispose()) as Box<dyn FnMut()>)
            .into_js_value()
            .unchecked_into()
    }) as Box<dyn FnMut(Function) -> Function>);
    set(&face, "onEntryError", &on_entry_error.into_js_value())?;
    add_ledger_methods(&face, state)?;
    Ok(face.into())
}

fn add_ledger_methods(face: &Object, state: &Rc<BrowserState>) -> Result<(), JsValue> {
    let version_core = state.registry.core().clone();
    let version = Closure::wrap(Box::new(move |key: String| {
        u64_to_f64(version_core.version(&SlotName::new(key)))
    }) as Box<dyn FnMut(String) -> f64>);
    set(face, "getVersion", &version.into_js_value())?;
    let entries_state = state.clone();
    let entries =
        Closure::wrap(
            Box::new(move |key: String| entries_state.entries(&SlotName::new(key)))
                as Box<dyn FnMut(String) -> Array>,
        );
    set(face, "entries", &entries.into_js_value())?;
    let winners_state = state.clone();
    let winners = Closure::wrap(Box::new(move |key: String| {
        winners_state.entries_of_slot(&SlotName::new(key))
    }) as Box<dyn FnMut(String) -> Array>);
    set(face, "entriesOfSlot", &winners.into_js_value())?;
    let spec_state = state.clone();
    let spec = Closure::wrap(Box::new(move |key: String| -> Result<JsValue, JsValue> {
        spec_state
            .registry
            .core()
            .spec(&SlotName::new(key))
            .map(|spec| spec_to_js(&spec))
            .transpose()
            .map(|value| value.unwrap_or(JsValue::UNDEFINED))
    }) as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>);
    set(face, "spec", &spec.into_js_value())?;
    let snapshot_state = state.clone();
    let snapshot = Closure::wrap(Box::new(move |root: JsValue| -> Result<JsValue, JsValue> {
        serde_wasm_bindgen::to_value(
            &snapshot_state
                .registry
                .core()
                .snapshot(root.as_string().as_deref().map(SlotName::new).as_ref()),
        )
        .map_err(|error| js_sys::Error::new(&error.to_string()).into())
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    set(face, "snapshot", &snapshot.into_js_value())?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn host_face(state: &Rc<BrowserState>) -> Result<JsValue, JsValue> {
    let host = Object::new();
    let subscribe_core = state.registry.core().clone();
    let subscribe = Closure::wrap(
        Box::new(move |key: String, listener: Function| -> Function {
            let subscription = subscribe_core.subscribe(
                SlotName::new(key),
                Rc::new(move || call_or_throw(listener.call0(&JsValue::UNDEFINED))),
            );
            Closure::wrap(Box::new(move || subscription.dispose()) as Box<dyn FnMut()>)
                .into_js_value()
                .unchecked_into()
        }) as Box<dyn FnMut(String, Function) -> Function>,
    );
    set(&host, "subscribe", &subscribe.into_js_value())?;
    let version_core = state.registry.core().clone();
    let version = Closure::wrap(Box::new(move |key: String| {
        u64_to_f64(version_core.version(&SlotName::new(key)))
    }) as Box<dyn FnMut(String) -> f64>);
    set(&host, "getVersion", &version.into_js_value())?;
    let entries_state = state.clone();
    let entries =
        Closure::wrap(
            Box::new(move |key: String| entries_state.entries(&SlotName::new(key)))
                as Box<dyn FnMut(String) -> Array>,
        );
    set(&host, "entriesOf", &entries.into_js_value())?;
    let winners_state = state.clone();
    let winners = Closure::wrap(Box::new(move |key: String| {
        winners_state.entries_of_slot(&SlotName::new(key))
    }) as Box<dyn FnMut(String) -> Array>);
    set(&host, "entriesOfSlot", &winners.into_js_value())?;
    let live_state = state.clone();
    let is_live = Closure::wrap(Box::new(move |entry: JsValue| {
        live_state
            .entry_for(&entry)
            .is_some_and(|entry| live_state.registry.core().is_live(&entry))
    }) as Box<dyn FnMut(JsValue) -> bool>);
    set(&host, "isLive", &is_live.into_js_value())?;
    let spec_core = state.registry.core().clone();
    let spec = Closure::wrap(Box::new(move |key: String| -> Result<JsValue, JsValue> {
        spec_core
            .spec(&SlotName::new(key))
            .map(|spec| spec_to_js(&spec))
            .transpose()
            .map(|value| value.unwrap_or(JsValue::UNDEFINED))
    }) as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>);
    set(&host, "specOf", &spec.into_js_value())?;
    let error_state = state.clone();
    let report = Closure::wrap(Box::new(
        move |key: String, entry: JsValue, error: JsValue, info: JsValue| -> Result<(), JsValue> {
            let entry = error_state
                .entry_for(&entry)
                .ok_or_else(|| js_sys::Error::new("Slot entry is not owned by this registry"))?;
            let abdicate = Reflect::get(&info, &JsValue::from_str("abdicate"))?
                .as_bool()
                .unwrap_or(false);
            error_state.registry.core().report_entry_error(
                &SlotName::new(key),
                &entry,
                &error,
                abdicate,
            );
            Ok(())
        },
    )
        as Box<dyn FnMut(String, JsValue, JsValue, JsValue) -> Result<(), JsValue>>);
    set(&host, "reportEntryError", &report.into_js_value())?;
    let store_state = state.clone();
    let store_of = Closure::wrap(Box::new(
        move |entry: JsValue, scope_key: JsValue| -> Result<JsValue, JsValue> {
            let entry = store_state
                .entry_for(&entry)
                .ok_or_else(|| js_sys::Error::new("Slot entry is not owned by this registry"))?;
            let instance = store_state
                .registry
                .store_of(&entry, scope_key.as_string().as_deref())
                .map_err(|error| js_sys::Error::new(&error.to_string()))?;
            let Some(instance) = instance else {
                return Ok(JsValue::UNDEFINED);
            };
            instance
                .as_any()
                .downcast_ref::<JsStoreInstance>()
                .map(|instance| instance.value.clone())
                .ok_or_else(|| {
                    js_sys::Error::new("Slot Store instance is not browser-backed").into()
                })
        },
    )
        as Box<dyn FnMut(JsValue, JsValue) -> Result<JsValue, JsValue>>);
    set(&host, "storeOf", &store_of.into_js_value())?;
    let sessions = standard_value(state.registry.sessions())?;
    let workspaces = standard_value(state.registry.workspaces())?;
    set(&host, "sessions", &sessions)?;
    set(&host, "workspaces", &workspaces)?;
    let locale_state = state.clone();
    let locale_getter = Closure::wrap(Box::new(move || -> JsValue {
        locale_state
            .registry
            .locale()
            .and_then(|locale| {
                locale
                    .as_any()
                    .downcast_ref::<JsLocaleFace>()
                    .map(|face| face.0.clone())
            })
            .unwrap_or(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> JsValue>);
    let descriptor = Object::new();
    set(&descriptor, "get", &locale_getter.into_js_value())?;
    Object::define_property(&host, &JsValue::from_str("locale"), &descriptor);
    Ok(host.into())
}

fn parse_options(value: &JsValue) -> Result<SlotRegistrationOptions<JsValue>, JsValue> {
    let mut options = SlotRegistrationOptions::new(required_string(value, "name")?);
    options.key = optional_string(value, "key")?;
    options.id = optional_string(value, "id")?;
    options.order = optional_number(value, "order")?;
    options.priority = optional_number(value, "priority")?;
    options.locale = optional_string(value, "locale")?;
    options.registrant = optional_string(value, "registrant")?;
    options.has_selector = optional(value, "select")?.is_some();
    options.children = optional(value, "children")?
        .as_ref()
        .map(parse_children)
        .transpose()?
        .unwrap_or_default();
    Ok(options)
}

fn parse_children(value: &JsValue) -> Result<IndexMap<SlotName, SlotSpec<JsValue>>, JsValue> {
    let object = Object::from(value.clone());
    let mut output = IndexMap::new();
    for key in Object::keys(&object)
        .iter()
        .filter_map(|key| key.as_string())
    {
        output.insert(
            SlotName::new(&key),
            parse_spec(&Reflect::get(&object, &JsValue::from_str(&key))?)?,
        );
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

fn stored_entry(source: &JsValue, component: &JsValue) -> Result<JsValue, JsValue> {
    let stored = Object::new();
    set(&stored, "component", component)?;
    let options = Object::new();
    for key in ["key", "id", "order", "label", "priority"] {
        copy_defined(source, &options, key)?;
    }
    set(&stored, "options", &options)?;
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
    Ok(stored.into())
}

fn spec_to_js(spec: &SlotSpec<JsValue>) -> Result<JsValue, JsValue> {
    let output = Object::new();
    set(
        &output,
        "kind",
        &JsValue::from_str(match spec.kind {
            SlotKind::Single => "single",
            SlotKind::List => "list",
            SlotKind::Keyed => "keyed",
            SlotKind::Chain => "chain",
        }),
    )?;
    set(
        &output,
        "scope",
        &JsValue::from_str(match spec.scope {
            SlotScope::Root => "root",
            SlotScope::SessionMaybe => "session-maybe",
            SlotScope::Session => "session",
        }),
    )?;
    if let Some(inject) = &spec.inject {
        set(&output, "inject", inject)?;
    }
    Ok(output.into())
}

fn find_entry_recursive(
    core: &Rc<
        seekdeep_client_ui_slots::SlotCore<RuntimeSlotPayload<BrowserEntry>, JsValue, JsValue>,
    >,
    value: &JsValue,
) -> Option<Rc<BrowserSlotEntry>> {
    fn visit(
        core: &Rc<
            seekdeep_client_ui_slots::SlotCore<RuntimeSlotPayload<BrowserEntry>, JsValue, JsValue>,
        >,
        node: &seekdeep_client_ui_slots::LiveSlotNode,
        value: &JsValue,
    ) -> Option<Rc<BrowserSlotEntry>> {
        if let Some(entry) = core
            .entries(&node.name)
            .iter()
            .find(|entry| Object::is(&entry.payload.component.stored, value))
        {
            return Some(entry.clone());
        }
        node.children
            .iter()
            .find_map(|child| visit(core, child, value))
    }
    core.snapshot(None)
        .iter()
        .find_map(|node| visit(core, node, value))
}

fn standard_value(face: Option<Rc<dyn RuntimeStandardFace>>) -> Result<JsValue, JsValue> {
    face.and_then(|face| {
        face.as_any()
            .downcast_ref::<JsStandardFace>()
            .map(|face| face.0.clone())
    })
    .ok_or_else(|| js_sys::Error::new("Slot renderer Host standard face is absent").into())
}

fn own_runtime_disposer(
    caller: &JsValue,
    label: &str,
    disposer: RuntimeDisposer,
) -> Result<(), JsValue> {
    let installer =
        Closure::wrap(
            Box::new(move || -> JsValue { runtime_disposer(disposer.clone()).into() })
                as Box<dyn FnMut() -> JsValue>,
        );
    call_method(
        caller,
        "effect",
        &[installer.into_js_value(), JsValue::from_str(label)],
    )?;
    Ok(())
}

fn runtime_disposer(disposer: RuntimeDisposer) -> Function {
    Closure::wrap(Box::new(move || disposer.dispose()) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into()
}

fn clone_object(value: &JsValue) -> Result<Object, JsValue> {
    if !value.is_object() || value.is_null() {
        return Err(js_sys::Error::new("Slot registration options must be an object").into());
    }
    Ok(Object::assign(&Object::new(), &Object::from(value.clone())))
}

fn required(value: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    let value = Reflect::get(value, &JsValue::from_str(key))?;
    if value.is_undefined() || value.is_null() {
        Err(js_sys::Error::new(&format!("Client Slot Service requires {key:?}")).into())
    } else {
        Ok(value)
    }
}

fn optional(value: &JsValue, key: &str) -> Result<Option<JsValue>, JsValue> {
    let value = Reflect::get(value, &JsValue::from_str(key))?;
    Ok((!value.is_undefined()).then_some(value))
}

fn required_string(value: &JsValue, key: &str) -> Result<String, JsValue> {
    optional_string(value, key)?
        .ok_or_else(|| js_sys::Error::new(&format!("Slot option {key:?} is required")).into())
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

fn has_function(value: &JsValue, key: &str) -> bool {
    Reflect::get(value, &JsValue::from_str(key)).is_ok_and(|value| value.is_function())
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

fn call_method(value: &JsValue, method: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = required(value, method)?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}

fn call_or_throw(result: Result<JsValue, JsValue>) {
    if let Err(error) = result {
        wasm_bindgen::throw_val(error);
    }
}

#[allow(clippy::needless_pass_by_value)]
fn js_client_error(error: JsValue) -> ClientSlotError {
    ClientSlotError::new(js_error_text(&error))
}

fn js_error_text(error: &JsValue) -> String {
    Reflect::get(error, &JsValue::from_str("message"))
        .ok()
        .and_then(|value| value.as_string())
        .or_else(|| js_sys::JsString::from(error.clone()).as_string())
        .unwrap_or_else(|| format!("{error:?}"))
}

fn f64_to_u64(value: f64) -> Option<u64> {
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
        return None;
    }
    value.to_string().parse().ok()
}

fn u64_to_f64(value: u64) -> f64 {
    value
        .to_string()
        .parse()
        .expect("u64 decimal text is a finite JavaScript number")
}
