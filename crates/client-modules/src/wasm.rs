//! Browser bindings for the Rust-owned Client module table.

use std::{collections::BTreeMap, sync::Arc};

use futures::{FutureExt, channel::oneshot, future::BoxFuture};
use js_sys::{Array, Function, Map, Object, Promise, Reflect, Set};
use parking_lot::Mutex;
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    BootModuleRow, ClientBundleLoader, ClientFactoryRegistrar, ClientModuleFactory, ClientModuleId,
    ClientModuleRequire, ClientModuleSystem, ClientStyleClaimer, parse_boot_manifest,
};

const MODULE_LOADER_SLOT: &str = "__ModuleLoader__";
const MODULE_SYSTEM_SLOT: &str = "__SEEKDEEP_MODULES__";

struct WasmBundleLoader {
    callback: Option<Function>,
}

impl ClientBundleLoader<JsValue> for WasmBundleLoader {
    fn load(
        &self,
        row: BootModuleRow,
        _registrar: ClientFactoryRegistrar<JsValue>,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        if let Some(callback) = &self.callback {
            let returned = match callback.call1(&JsValue::UNDEFINED, &JsValue::from_str(&row.url)) {
                Ok(returned) => returned,
                Err(error) => return futures::future::ready(Err(js_error(&error))).boxed(),
            };
            let promise = Promise::resolve(&returned);
            return async move {
                promise_result(&promise)
                    .await
                    .map(|_| ())
                    .map_err(|error| js_error(&error))
            }
            .boxed();
        }
        load_script(&row.url)
    }
}

#[derive(Debug, Default)]
struct WasmStyleClaimer;

impl ClientStyleClaimer for WasmStyleClaimer {
    fn claim(&self, id: &ClientModuleId) -> Vec<String> {
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return Vec::new();
        };
        if let Ok(unowned) = document.query_selector_all("style:not([data-plugin])") {
            for index in 0..unowned.length() {
                if let Some(element) = unowned
                    .get(index)
                    .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
                {
                    let _: Result<(), _> = element.set_attribute("data-plugin", id.as_str());
                }
            }
        }
        let selector = format!(
            "style[data-plugin={}]",
            serde_json::to_string(id.as_str()).expect("module id is JSON")
        );
        let Ok(owned) = document.query_selector_all(&selector) else {
            return Vec::new();
        };
        (0..owned.length())
            .filter_map(|index| owned.get(index))
            .filter_map(|node| node.dyn_into::<web_sys::Element>().ok())
            .map(|element| {
                element
                    .get_attribute("data-plugin-css")
                    .unwrap_or_else(|| id.as_str().to_owned())
            })
            .collect()
    }
}

/// JavaScript-facing module table consumed by the Client Cordis Loader.
#[wasm_bindgen]
pub struct WasmClientModuleSystem {
    system: ClientModuleSystem<JsValue>,
    load_cache: Map,
    records: Mutex<BTreeMap<ClientModuleId, JsValue>>,
}

#[wasm_bindgen]
impl WasmClientModuleSystem {
    /// Builds the table and installs `window.__ModuleLoader__` exactly once.
    ///
    /// # Errors
    ///
    /// Returns malformed options, duplicate graph rows, or double-boot errors.
    #[wasm_bindgen(constructor)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(
        modules: JsValue,
        static_modules: JsValue,
        load_bundle: Option<Function>,
    ) -> Result<Self, JsValue> {
        let modules: Vec<BootModuleRow> = serde_wasm_bindgen::from_value(modules)
            .map_err(|error| js_sys::Error::new(&error.to_string()))?;
        let seed = object_entries(static_modules)?;
        let system = ClientModuleSystem::new(
            modules,
            seed,
            Arc::new(WasmBundleLoader {
                callback: load_bundle,
            }),
            Arc::new(WasmStyleClaimer),
        )
        .map_err(|error| js_sys::Error::new(&error.to_string()))?;
        install_registration_sink(system.registrar())?;
        Ok(Self {
            system,
            load_cache: Map::new(),
            records: Mutex::new(BTreeMap::new()),
        })
    }

    /// Client loader discriminant.
    #[wasm_bindgen(getter)]
    pub fn version(&self) -> String {
        "client".to_owned()
    }

    /// Stable JavaScript `Map` of materialized records.
    #[wasm_bindgen(getter, js_name = loadCache)]
    pub fn load_cache(&self) -> Map {
        self.load_cache.clone()
    }

    /// Imports one module through the exact lazy branch order.
    ///
    /// # Errors
    ///
    /// Returns resolution, transport, registration, factory, or cycle failures.
    #[wasm_bindgen(js_name = import)]
    pub async fn import_module(
        &self,
        specifier: String,
        _parent_url: String,
        _attributes: JsValue,
    ) -> Result<JsValue, JsValue> {
        let exports = self
            .system
            .import(&specifier)
            .await
            .map_err(|error| js_sys::Error::new(&error.to_string()))?;
        self.sync_cache()?;
        Ok(exports)
    }

    /// Registers one shell-owned module.
    ///
    /// # Errors
    ///
    /// Rejects duplicate static identities.
    #[wasm_bindgen(js_name = registerStatic)]
    pub fn register_static(&self, id: String, module: JsValue) -> Result<(), JsValue> {
        self.system
            .register_static(id, module)
            .map_err(|error| js_sys::Error::new(&error.to_string()).into())
    }

    /// Prefetches a bundle without materializing it.
    ///
    /// # Errors
    ///
    /// Returns unknown-row, transport, and missing-registration failures.
    pub async fn prefetch(&self, id: String) -> Result<(), JsValue> {
        self.system
            .prefetch(&ClientModuleId::new(id))
            .await
            .map_err(|error| js_sys::Error::new(&error.to_string()).into())
    }

    /// Drops one factory and materialized record for HMR.
    pub fn invalidate(&self, id: String) {
        self.system.invalidate(&ClientModuleId::new(id));
        let _ = self.sync_cache();
    }
}

impl WasmClientModuleSystem {
    fn sync_cache(&self) -> Result<(), JsValue> {
        let snapshot = self.system.cache_snapshot();
        let mut records = self.records.lock();
        let stale = records
            .keys()
            .filter(|id| !snapshot.contains_key(*id))
            .cloned()
            .collect::<Vec<_>>();
        for id in stale {
            records.remove(&id);
            self.load_cache.delete(&JsValue::from_str(id.as_str()));
        }
        for (id, record) in snapshot {
            if records.contains_key(&id) {
                continue;
            }
            let value = Object::new();
            set(&value, "id", &JsValue::from_str(id.as_str()))?;
            set(&value, "exports", &record.exports)?;
            let styles = Array::new();
            for style in record.styles {
                styles.push(&JsValue::from_str(&style));
            }
            set(&value, "styles", &styles.into())?;
            let edges = Set::new(&JsValue::UNDEFINED);
            for edge in record.edges {
                edges.add(&JsValue::from_str(&edge));
            }
            set(&value, "edges", &edges.into())?;
            let value: JsValue = value.into();
            self.load_cache.set(&JsValue::from_str(id.as_str()), &value);
            records.insert(id, value);
        }
        Ok(())
    }
}

/// Parses a raw boot graph into its module and plugin views.
///
/// # Errors
///
/// Returns the first field-specific boot-manifest diagnostic.
#[wasm_bindgen(js_name = parseBootManifest)]
#[allow(clippy::needless_pass_by_value)]
pub fn parse_boot_manifest_js(wire: JsValue) -> Result<JsValue, JsValue> {
    let wire: serde_json::Value = serde_wasm_bindgen::from_value(wire).map_err(|_| {
        js_sys::Error::new("client-modules: window.__SEEKDEEP_BOOT__ is missing or not an object")
    })?;
    let manifest =
        parse_boot_manifest(&wire).map_err(|error| js_sys::Error::new(&error.to_string()))?;
    to_js(&manifest)
}

/// Builds the enrollment plugin for the kernel-created module system.
///
/// # Errors
///
/// Returns JavaScript construction failures.
#[wasm_bindgen(js_name = clientModulesPlugin)]
pub fn client_modules_plugin() -> Result<JsValue, JsValue> {
    let apply = Closure::wrap(Box::new(move |ctx: JsValue| -> Result<(), JsValue> {
        let modules = Reflect::get(&js_sys::global(), &JsValue::from_str(MODULE_SYSTEM_SLOT))?;
        if modules.is_undefined() {
            return Err(js_sys::Error::new(
                "client-modules: window.__SEEKDEEP_MODULES__ missing — the shell kernel must construct the module system before plugin boot",
            )
            .into());
        }
        let reflect = Reflect::get(&ctx, &JsValue::from_str("reflect"))?;
        call_method(
            &reflect,
            "provide",
            &[JsValue::from_str("modules"), modules],
        )?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let plugin = Object::new();
    set(&plugin, "name", &JsValue::from_str("client-modules"))?;
    set(&plugin, "apply", &apply.into_js_value())?;
    Ok(plugin.into())
}

fn install_registration_sink(registrar: ClientFactoryRegistrar<JsValue>) -> Result<(), JsValue> {
    let global = js_sys::global();
    if !Reflect::get(&global, &JsValue::from_str(MODULE_LOADER_SLOT))?.is_undefined() {
        return Err(js_sys::Error::new(
            "client-modules: window.__ModuleLoader__ already installed (double boot?)",
        )
        .into());
    }
    let load = Closure::wrap(Box::new(move |handoff: JsValue| -> Result<(), JsValue> {
        let id = Reflect::get(&handoff, &JsValue::from_str("id"))?
            .as_string()
            .ok_or_else(|| {
                js_sys::Error::new("client-modules: factory handoff id must be a string")
            })?;
        let factory =
            Reflect::get(&handoff, &JsValue::from_str("factory"))?.dyn_into::<Function>()?;
        let registered: ClientModuleFactory<JsValue> =
            Arc::new(move |require| materialize_js_factory(&factory, require));
        registrar
            .register(ClientModuleId::new(id), registered)
            .map_err(|error| js_sys::Error::new(&error.to_string()).into())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let sink = Object::new();
    set(&sink, "load", &load.into_js_value())?;
    Reflect::set(&global, &JsValue::from_str(MODULE_LOADER_SLOT), &sink)?;
    Ok(())
}

fn materialize_js_factory(
    factory: &Function,
    require: ClientModuleRequire<JsValue>,
) -> anyhow::Result<JsValue> {
    let callback = Closure::wrap(
        Box::new(move |specifier: String| -> Result<JsValue, JsValue> {
            require
                .require(&specifier)
                .map_err(|error| js_sys::Error::new(&error.to_string()).into())
        }) as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>,
    );
    factory
        .call1(&JsValue::UNDEFINED, &callback.into_js_value())
        .map_err(|error| js_error(&error))
}

fn object_entries(value: JsValue) -> Result<Vec<(String, JsValue)>, JsValue> {
    if !value.is_object() || value.is_null() {
        return Err(js_sys::Error::new("client-modules: staticModules must be an object").into());
    }
    Object::entries(&Object::from(value))
        .iter()
        .map(|entry| {
            let entry = entry.dyn_into::<Array>()?;
            let key = entry
                .get(0)
                .as_string()
                .ok_or_else(|| js_sys::Error::new("static module key is not a string"))?;
            Ok((key, entry.get(1)))
        })
        .collect()
}

fn load_script(url: &str) -> BoxFuture<'static, anyhow::Result<()>> {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return futures::future::ready(Err(anyhow::anyhow!(
            "client-modules: bundle loading requires browser Document"
        )))
        .boxed();
    };
    let script: web_sys::HtmlScriptElement =
        match document.create_element("script").and_then(|element| {
            element
                .dyn_into::<web_sys::HtmlScriptElement>()
                .map_err(Into::into)
        }) {
            Ok(script) => script,
            Err(error) => return futures::future::ready(Err(js_error(&error))).boxed(),
        };
    script.set_async(true);
    script.set_src(url);
    let (sender, receiver) = oneshot::channel::<anyhow::Result<()>>();
    let sender = Arc::new(Mutex::new(Some(sender)));
    let loaded = {
        let sender = sender.clone();
        let script = script.clone();
        Closure::once_into_js(move || {
            script.remove();
            if let Some(sender) = sender.lock().take() {
                let _ = sender.send(Ok(()));
            }
        })
    };
    let failed = {
        let sender = sender.clone();
        let script = script.clone();
        let url = url.to_owned();
        Closure::once_into_js(move || {
            script.remove();
            if let Some(sender) = sender.lock().take() {
                let _ = sender.send(Err(anyhow::anyhow!(
                    "client-modules: bundle script {url} failed to load"
                )));
            }
        })
    };
    if let Err(error) = Reflect::set(&script, &JsValue::from_str("onload"), &loaded)
        .and_then(|_| Reflect::set(&script, &JsValue::from_str("onerror"), &failed))
        .and_then(|_| {
            document
                .head()
                .ok_or_else(|| {
                    JsValue::from(js_sys::Error::new(
                        "client-modules: document.head is missing",
                    ))
                })?
                .append_child(&script)
                .map(|_| true)
        })
        && let Some(sender) = sender.lock().take()
    {
        let _ = sender.send(Err(js_error(&error)));
    }
    async move {
        receiver.await.unwrap_or_else(|_| {
            Err(anyhow::anyhow!(
                "client-modules: bundle load settlement channel closed"
            ))
        })
    }
    .boxed()
}

fn promise_result(promise: &Promise) -> BoxFuture<'static, Result<JsValue, JsValue>> {
    let (sender, receiver) = oneshot::channel();
    let sender = Arc::new(Mutex::new(Some(sender)));
    let resolve = {
        let sender = sender.clone();
        Closure::once_into_js(move |value: JsValue| {
            if let Some(sender) = sender.lock().take() {
                let _ = sender.send(Ok(value));
            }
        })
    };
    let reject = {
        let sender = sender.clone();
        Closure::once_into_js(move |error: JsValue| {
            if let Some(sender) = sender.lock().take() {
                let _ = sender.send(Err(error));
            }
        })
    };
    let registration = Reflect::get(promise, &JsValue::from_str("then"))
        .and_then(wasm_bindgen::JsCast::dyn_into::<Function>)
        .and_then(|then| then.call2(promise, &resolve, &reject));
    if let Err(error) = registration
        && let Some(sender) = sender.lock().take()
    {
        let _ = sender.send(Err(error));
    }
    async move {
        receiver.await.unwrap_or_else(|_| {
            Err(js_sys::Error::new("JavaScript Promise settlement channel closed").into())
        })
    }
    .boxed()
}

fn call_method(target: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(target, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    Reflect::apply(&method, target, &args)
}

fn set(target: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(target, &JsValue::from_str(key), value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new(&format!("could not assign {key:?}")).into())
    }
}

fn to_js(value: &impl serde::Serialize) -> Result<JsValue, JsValue> {
    value
        .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
        .map_err(|error| js_sys::Error::new(&error.to_string()).into())
}

fn js_error(error: &JsValue) -> anyhow::Error {
    let message = Reflect::get(error, &JsValue::from_str("message"))
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_else(|| js_sys::JsString::from(error.clone()).into());
    anyhow::anyhow!(message)
}
