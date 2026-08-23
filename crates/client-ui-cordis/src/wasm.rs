//! JavaScript-compatible browser facade compiled into WebAssembly.

use std::sync::Arc;

use js_sys::{Function, Map, Object, Set};
use seekdeep_cordis_client_runner::DynamicCordisLivePackage;
use seekdeep_cordis_dynamic_types::{
    CordisDynamicPackageId, CordisDynamicPluginId, CordisDynamicPluginRunId,
    DynamicCordisInventoryRow,
};
use seekdeep_identity::SessionId;
use serde::Deserialize;
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    CordisInventory, CordisRunCardPointer, CordisRunCardRegistry, CordisRunCardStore,
    InventoryReadTicket, ToolCallBlock, cordis_action_card, cordis_define_card, cordis_run_card,
    cordis_tool_view_key, cordis_visible_status,
};

/// Derives the source-compatible `cordis_define` card model.
///
/// # Errors
///
/// Returns malformed block or JavaScript conversion errors.
#[wasm_bindgen(js_name = cordisDefineCard)]
#[allow(clippy::needless_pass_by_value)]
pub fn cordis_define_card_js(block: JsValue) -> Result<JsValue, JsValue> {
    let block = decode_block(block)?;
    encode(&cordis_define_card(&block))
}

/// Derives the source-compatible `cordis_run` card model.
///
/// # Errors
///
/// Returns malformed block or JavaScript conversion errors.
#[wasm_bindgen(js_name = cordisRunCard)]
#[allow(clippy::needless_pass_by_value)]
pub fn cordis_run_card_js(block: JsValue) -> Result<JsValue, JsValue> {
    let block = decode_block(block)?;
    encode(&cordis_run_card(&block))
}

/// Derives the source-compatible Stop or Remove card model.
///
/// # Errors
///
/// Returns malformed block or JavaScript conversion errors.
#[wasm_bindgen(js_name = cordisActionCard)]
#[allow(clippy::needless_pass_by_value)]
pub fn cordis_action_card_js(block: JsValue) -> Result<JsValue, JsValue> {
    let block = decode_block(block)?;
    encode(&cordis_action_card(&block))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LivePackageWire {
    plugin_id: CordisDynamicPluginId,
    package_id: CordisDynamicPackageId,
    plugin_run_id: CordisDynamicPluginRunId,
    name: String,
    #[serde(default)]
    slots: Vec<String>,
    #[serde(default)]
    style_count: usize,
}

impl From<LivePackageWire> for DynamicCordisLivePackage {
    fn from(wire: LivePackageWire) -> Self {
        Self {
            plugin_id: wire.plugin_id,
            package_id: wire.package_id,
            plugin_run_id: wire.plugin_run_id,
            name: wire.name,
            slots: wire.slots,
            style_count: wire.style_count,
        }
    }
}

/// Derives one Package's visible Host/Client state.
///
/// # Errors
///
/// Returns malformed inventory or live-package inputs.
#[wasm_bindgen(js_name = cordisVisibleStatus)]
#[allow(clippy::needless_pass_by_value)]
pub fn cordis_visible_status_js(
    row: JsValue,
    package_id: String,
    loaded: JsValue,
) -> Result<String, JsValue> {
    let row: DynamicCordisInventoryRow = decode(row)?;
    let loaded = decode::<Vec<LivePackageWire>>(loaded)?
        .into_iter()
        .map(Into::into)
        .collect::<Vec<_>>();
    let status = cordis_visible_status(&row, &CordisDynamicPackageId::new(package_id), &loaded);
    encode(&status)?.as_string().ok_or_else(|| {
        js_sys::Error::new("Cordis visible status did not serialize to a string").into()
    })
}

/// Opaque current-generation read ticket.
#[wasm_bindgen]
#[derive(Clone, Copy)]
pub struct WasmInventoryReadTicket {
    inner: InventoryReadTicket,
}

/// Browser inventory observable with exact generation invalidation.
#[wasm_bindgen]
pub struct WasmCordisInventory {
    inner: Arc<CordisInventory>,
}

#[wasm_bindgen]
impl WasmCordisInventory {
    /// Creates an unread page inventory.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            inner: CordisInventory::new(),
        }
    }

    /// Returns a JavaScript snapshot with a real `Set` for removed Plugin identities.
    ///
    /// # Errors
    ///
    /// Returns JavaScript object or row conversion failures.
    #[wasm_bindgen(js_name = getSnapshot)]
    pub fn snapshot(&self) -> Result<JsValue, JsValue> {
        inventory_snapshot(&self.inner)
    }

    /// Subscribes and returns an idempotent disposer.
    pub fn subscribe(&self, listener: Function) -> Function {
        let subscription = self.inner.subscribe(Arc::new(move || {
            let _ = listener.call0(&JsValue::UNDEFINED);
        }));
        let disposer = Closure::wrap(Box::new(move || subscription.dispose()) as Box<dyn FnMut()>);
        disposer.into_js_value().unchecked_into()
    }

    /// Claims a single-flight refresh slot, or returns `undefined` while occupied.
    #[wasm_bindgen(js_name = beginRefresh)]
    pub fn begin_refresh(&self) -> Option<WasmInventoryReadTicket> {
        self.inner
            .begin_refresh()
            .map(|inner| WasmInventoryReadTicket { inner })
    }

    /// Settles one successful Host read.
    ///
    /// # Errors
    ///
    /// Returns malformed inventory rows.
    #[allow(clippy::needless_pass_by_value)]
    pub fn resolve(&self, ticket: WasmInventoryReadTicket, rows: JsValue) -> Result<bool, JsValue> {
        Ok(self.inner.resolve(
            ticket.inner,
            decode::<Vec<DynamicCordisInventoryRow>>(rows)?,
        ))
    }

    /// Settles one Host read failure. Undefined denotes a non-Error rejection.
    pub fn reject(&self, ticket: WasmInventoryReadTicket, message: Option<String>) -> bool {
        self.inner.reject(ticket.inner, message)
    }

    /// Records an explicit removal immediately.
    pub fn retire(&self, plugin_id: String) {
        self.inner.retire(&CordisDynamicPluginId::new(plugin_id));
    }

    /// Invalidates the prior connection and frees its read slot.
    pub fn reset(&self) {
        self.inner.reset();
    }
}

impl Default for WasmCordisInventory {
    fn default() -> Self {
        Self::new()
    }
}

/// Page-lifetime registry for session-local latest-card Stores.
#[wasm_bindgen]
#[derive(Default)]
pub struct WasmCordisRunCardRegistry {
    inner: CordisRunCardRegistry,
}

#[wasm_bindgen]
impl WasmCordisRunCardRegistry {
    /// Creates an empty page registry.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the persistent Store for one Session.
    #[wasm_bindgen(js_name = forSession)]
    pub fn for_session(&self, session_id: String) -> WasmCordisRunCardStore {
        WasmCordisRunCardStore {
            inner: self.inner.for_session(SessionId::new(session_id)),
        }
    }
}

/// JavaScript observable facade over one session's latest-card Store.
#[wasm_bindgen]
pub struct WasmCordisRunCardStore {
    inner: Arc<CordisRunCardStore>,
}

#[wasm_bindgen]
impl WasmCordisRunCardStore {
    /// Returns a JavaScript `Map` keyed by Plugin and Package.
    ///
    /// # Errors
    ///
    /// Returns JavaScript object construction failures.
    #[wasm_bindgen(js_name = getSnapshot)]
    pub fn snapshot(&self) -> Result<Map, JsValue> {
        let output = Map::new();
        for (key, pointer) in self.inner.snapshot().iter() {
            output.set(&JsValue::from_str(key.as_str()), &pointer_value(pointer)?);
        }
        Ok(output)
    }

    /// Subscribes and returns an idempotent disposer.
    pub fn subscribe(&self, listener: Function) -> Function {
        let subscription = self.inner.subscribe(Arc::new(move || {
            let _ = listener.call0(&JsValue::UNDEFINED);
        }));
        let disposer = Closure::wrap(Box::new(move || subscription.dispose()) as Box<dyn FnMut()>);
        disposer.into_js_value().unchecked_into()
    }

    /// Publishes a successful Run result when its sequence is newer.
    #[wasm_bindgen(js_name = observe)]
    pub fn observe(
        &self,
        plugin_id: String,
        package_id: String,
        call_id: String,
        seq: u64,
        plugin_run_id: String,
    ) -> bool {
        let plugin_id = CordisDynamicPluginId::new(plugin_id);
        let package_id = CordisDynamicPackageId::new(package_id);
        self.inner.observe(CordisRunCardPointer {
            key: cordis_tool_view_key(&plugin_id, &package_id),
            call_id,
            seq,
            plugin_run_id: CordisDynamicPluginRunId::new(plugin_run_id),
        })
    }
}

fn decode_block(value: JsValue) -> Result<ToolCallBlock, JsValue> {
    decode(value)
}

fn decode<T: serde::de::DeserializeOwned>(value: JsValue) -> Result<T, JsValue> {
    serde_wasm_bindgen::from_value(value)
        .map_err(|error| js_sys::Error::new(&error.to_string()).into())
}

fn encode<T: serde::Serialize + ?Sized>(value: &T) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(value)
        .map_err(|error| js_sys::Error::new(&error.to_string()).into())
}

fn inventory_snapshot(inventory: &CordisInventory) -> Result<JsValue, JsValue> {
    let snapshot = inventory.snapshot();
    let value = Object::new();
    set(&value, "rows", &encode(&snapshot.rows)?)?;
    let removed = Set::new(&JsValue::UNDEFINED);
    for plugin_id in &snapshot.removed {
        removed.add(&JsValue::from_str(plugin_id.as_str()));
    }
    set(&value, "removed", &removed.into())?;
    set(&value, "read", &JsValue::from_bool(snapshot.read))?;
    if let Some(error) = &snapshot.error {
        set(&value, "error", &JsValue::from_str(error))?;
    }
    Ok(value.into())
}

fn pointer_value(pointer: &CordisRunCardPointer) -> Result<JsValue, JsValue> {
    let value = Object::new();
    set(&value, "key", &JsValue::from_str(pointer.key.as_str()))?;
    set(&value, "callId", &JsValue::from_str(&pointer.call_id))?;
    let seq = pointer
        .seq
        .to_string()
        .parse::<f64>()
        .expect("u64 decimal text is a finite JavaScript number");
    set(&value, "seq", &JsValue::from_f64(seq))?;
    set(
        &value,
        "pluginRunId",
        &JsValue::from_str(pointer.plugin_run_id.as_str()),
    )?;
    Ok(value.into())
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if js_sys::Reflect::set(object, &JsValue::from_str(key), value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new(&format!("failed to set {key:?}")).into())
    }
}
