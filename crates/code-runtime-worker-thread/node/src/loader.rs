//! Persistent native module identity and reversible Node cache invalidation.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use js_sys::{Array, Promise};
use serde_json::{Value, json};
use wasm_bindgen::{JsCast as _, prelude::*};
use wasm_bindgen_futures::{JsFuture, spawn_local};

use crate::{
    bridge,
    loader_context::{Activation, activate, deactivate, invoke},
};

#[wasm_bindgen(inline_js = r"
const apply = Reflect.apply;
const remove = Reflect.deleteProperty;
const get = Map.prototype.get;
const set = Map.prototype.set;
const del = Map.prototype.delete;
export function loaderMapGet(map, key) { return apply(get, map, [key]); }
export function loaderMapSet(map, key, value) { return apply(set, map, [key, value]); }
export function loaderMapDelete(map, key) { return apply(del, map, [key]); }
export function loaderDelete(object, key) { return remove(object, key); }
")]
extern "C" {
    #[wasm_bindgen(catch, js_name = loaderMapGet)]
    fn map_get(map: &JsValue, key: &JsValue) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch, js_name = loaderMapSet)]
    fn map_set(map: &JsValue, key: &JsValue, value: &JsValue) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch, js_name = loaderMapDelete)]
    fn map_delete(map: &JsValue, key: &JsValue) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch, js_name = loaderDelete)]
    fn delete(object: &JsValue, key: &JsValue) -> Result<bool, JsValue>;
}

pub(crate) struct Realm {
    pub(crate) connection: JsValue,
    input: String,
    modules: BTreeMap<u64, JsValue>,
    paths: BTreeMap<String, u64>,
    next_module: u64,
    pub(crate) activations: BTreeMap<u64, Rc<RefCell<Activation>>>,
    pub(crate) services: BTreeMap<u64, JsValue>,
    pub(crate) next_service: u64,
    pub(crate) pending: BTreeMap<u64, (JsValue, JsValue)>,
    pub(crate) next_call: u64,
    callbacks: Vec<JsValue>,
    backup: Option<CacheBackup>,
    pub(crate) last_reload: JsValue,
    pub(crate) watchers: BTreeMap<u64, crate::loader_watch::Watcher>,
    pub(crate) intrinsics: crate::snapshot::Intrinsics,
}

struct CacheBackup {
    esm: Vec<(String, JsValue)>,
    cjs: Vec<(String, JsValue)>,
    candidate_modules: Vec<u64>,
    paths: BTreeMap<String, u64>,
    reloads: JsValue,
}

thread_local! {
    static REALM: RefCell<Option<Rc<RefCell<Realm>>>> = const { RefCell::new(None) };
}

pub(crate) fn emit(realm: &Rc<RefCell<Realm>>, message: &Value) -> Result<(), JsValue> {
    let text =
        serde_json::to_string(message).map_err(|error| bridge::error(&error.to_string()))? + "\n";
    let connection = realm.borrow().connection.clone();
    bridge::method(&connection, "write", &[JsValue::from_str(&text)])?;
    Ok(())
}

pub(crate) fn wire(value: &JsValue) -> Result<Value, JsValue> {
    if value.is_undefined() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&bridge::stringify(value)?)
        .map_err(|error| bridge::error(&error.to_string()))
}

pub(crate) async fn settled(value: JsValue) -> Result<JsValue, JsValue> {
    JsFuture::from(bridge::resolve(&value).unchecked_into::<Promise>()).await
}

fn path_url(path: &str) -> Result<String, JsValue> {
    let value = bridge::apply(
        &bridge::api("pathToFileURL")?,
        &JsValue::UNDEFINED,
        &[JsValue::from_str(path)],
    )?;
    bridge::get(&value, "href")?
        .as_string()
        .ok_or_else(|| bridge::error("file URL is unavailable"))
}

fn url_path(url: &str) -> Result<String, JsValue> {
    bridge::apply(
        &bridge::api("fileURLToPath")?,
        &JsValue::UNDEFINED,
        &[JsValue::from_str(url)],
    )?
    .as_string()
    .ok_or_else(|| bridge::error("file path is unavailable"))
}

fn cache() -> Result<JsValue, JsValue> {
    bridge::get(&bridge::api("internal")?, "loadCache")
}

async fn linked(url: &str) -> Result<Vec<String>, JsValue> {
    let job = bridge::method(&cache()?, "get", &[JsValue::from_str(url)])?;
    if job.is_undefined() {
        return Ok(Vec::new());
    }
    let children = settled(bridge::get(&job, "linked")?).await?;
    if children.is_undefined() {
        return Ok(Vec::new());
    }
    let children = Array::from(&children);
    let mut output = Vec::new();
    for child in children.iter() {
        if let Some(url) = bridge::get(&child, "url")?.as_string() {
            output.push(url);
        }
    }
    Ok(output)
}

fn excluded(url: &str) -> bool {
    url.starts_with("node:") || url.contains("/node_modules/")
}

async fn dependencies(root: &str, ignored: &BTreeSet<String>) -> Result<BTreeSet<String>, JsValue> {
    let mut found = BTreeSet::new();
    let mut pending = vec![root.to_owned()];
    while let Some(url) = pending.pop() {
        if excluded(&url) || ignored.contains(&url) || !found.insert(url.clone()) {
            continue;
        }
        pending.extend(linked(&url).await?);
    }
    Ok(found)
}

fn unwrap(namespace: JsValue) -> Result<JsValue, JsValue> {
    let default = bridge::get(&namespace, "default")?;
    let plugin = if default.is_null() || default.is_undefined() {
        namespace
    } else {
        default
    };
    if bridge::get(&plugin, "__esModule")?.is_truthy() {
        let default = bridge::get(&plugin, "default")?;
        if !default.is_null() && !default.is_undefined() {
            return Ok(default);
        }
    }
    Ok(plugin)
}

async fn import(realm: &Rc<RefCell<Realm>>, path: &str) -> Result<Value, JsValue> {
    let url = path_url(path)?;
    let namespace = settled(bridge::method(
        &bridge::api("internal")?,
        "import",
        &[
            JsValue::from_str(&url),
            JsValue::UNDEFINED,
            bridge::object(&[])?,
        ],
    )?)
    .await?;
    let plugin = unwrap(namespace)?;
    let apply = if plugin.is_function() {
        plugin.clone()
    } else {
        bridge::get(&plugin, "apply")?
    };
    if !apply.is_function() {
        return Err(bridge::error("plugin must export an apply function"));
    }
    let name = bridge::get(&plugin, "name")?
        .as_string()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.to_owned());
    let inject = bridge::get(&plugin, "inject")?;
    let inject = if bridge::is_array(&inject) {
        wire(&inject)?
    } else if inject.is_object() && !inject.is_null() {
        let required = bridge::get(&inject, "required")?;
        if bridge::is_array(&required) {
            wire(&required)?
        } else {
            Value::Array(Vec::new())
        }
    } else {
        Value::Array(Vec::new())
    };
    let dependencies = dependencies(&url, &BTreeSet::new())
        .await?
        .into_iter()
        .filter_map(|url| url_path(&url).ok())
        .collect::<Vec<_>>();
    let mut realm = realm.borrow_mut();
    let id = realm.next_module;
    realm.next_module += 1;
    realm.modules.insert(id, plugin);
    realm.paths.insert(path.to_owned(), id);
    if let Some(backup) = &mut realm.backup {
        backup.candidate_modules.push(id);
    }
    Ok(json!({"module":id,"name":name,"inject":inject,"dependencies":dependencies,"path":path}))
}

async fn resolve_path(specifier: &str, base: &str) -> Result<Value, JsValue> {
    let internal = bridge::api("internal")?;
    let specifier = JsValue::from_str(specifier);
    let base = JsValue::from_str(base);
    let attributes = bridge::object(&[])?;
    let resolved = if bridge::get(&internal, "resolve")?.is_function() {
        settled(bridge::method(
            &internal,
            "resolve",
            &[specifier, base, attributes],
        )?)
        .await?
    } else {
        let request = bridge::object(&[("specifier", specifier), ("attributes", attributes)])?;
        bridge::method(&internal, "resolveSync", &[base, request])?
    };
    let url = bridge::get(&resolved, "url")?
        .as_string()
        .ok_or_else(|| bridge::error("module resolution returned no URL"))?;
    Ok(json!(url_path(&url)?))
}

async fn begin(realm: &Rc<RefCell<Realm>>, request: &Value) -> Result<Value, JsValue> {
    if realm.borrow().backup.is_some() {
        return Err(bridge::error("Host HMR cache transaction already active"));
    }
    let paths = strings(request, "paths");
    let roots = strings(request, "roots");
    let mut accepted = paths
        .iter()
        .map(|path| path_url(path))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let mut declined = strings(request, "externals")
        .iter()
        .map(|path| path_url(path))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let mut pending = Vec::new();
    for url in accepted.clone() {
        for child in linked(&url).await? {
            if !accepted.contains(&child) && !declined.contains(&child) && !excluded(&child) {
                pending.push(child);
            }
        }
    }
    while !pending.is_empty() {
        let mut index = 0;
        let mut updated = false;
        while index < pending.len() {
            let url = pending[index].clone();
            let mut is_declined = true;
            let mut is_accepted = false;
            for child in linked(&url).await? {
                if declined.contains(&child) || excluded(&child) {
                    continue;
                }
                if accepted.contains(&child) {
                    is_accepted = true;
                    break;
                }
                is_declined = false;
                if !pending.contains(&child) {
                    updated = true;
                    pending.push(child);
                }
            }
            if is_accepted || is_declined {
                updated = true;
                pending.remove(index);
                if is_accepted {
                    accepted.insert(url);
                } else {
                    declined.insert(url);
                }
            } else {
                index += 1;
            }
        }
        if !updated {
            break;
        }
    }
    declined.extend(pending);
    let mut plugins = Vec::new();
    for path in &roots {
        let url = path_url(path)?;
        if !declined.contains(&url) {
            plugins.push((path.clone(), url.clone()));
            declined.insert(url);
        }
    }
    let mut reloads = Vec::new();
    for (path, url) in plugins {
        declined.remove(&url);
        let dependencies = dependencies(&url, &declined).await?;
        declined.insert(url);
        if dependencies.iter().any(|url| accepted.contains(url)) {
            accepted.extend(dependencies);
            reloads.push(path);
        }
    }
    backup_caches(realm, request, &reloads, accepted)?;
    let mut candidates = Vec::new();
    for path in reloads {
        match import(realm, &path).await {
            Ok(module) => candidates.push(module),
            Err(error) => {
                rollback(realm)?;
                return Err(error);
            }
        }
    }
    Ok(Value::Array(candidates))
}

fn backup_caches(
    realm: &Rc<RefCell<Realm>>,
    request: &Value,
    reloads: &[String],
    accepted: BTreeSet<String>,
) -> Result<(), JsValue> {
    let esm = cache()?;
    let cjs = bridge::get(&bridge::api("require")?, "cache")?;
    let reload_map = bridge::construct(
        &bridge::get(&bridge::api("global")?, "Map")?,
        &bridge::array(&[])?,
    )?;
    for path in reloads {
        let old = realm.borrow().paths.get(path).copied();
        if let Some(id) = old
            && let Some(plugin) = realm.borrow().modules.get(&id).cloned()
        {
            let fibers = if let Some(snapshots) = request["fibers"].as_array() {
                snapshots
                    .iter()
                    .filter(|snapshot| snapshot["path"].as_str() == Some(path.as_str()))
                    .map(|snapshot| snapshot_fiber(realm, snapshot))
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                realm
                    .borrow()
                    .activations
                    .values()
                    .filter(|activation| activation.borrow().module == id)
                    .map(|activation| activation.borrow().fiber.clone())
                    .collect::<Vec<_>>()
            };
            let runtime = if fibers.is_empty() {
                JsValue::UNDEFINED
            } else {
                bridge::object(&[("fibers", bridge::array(&fibers)?)])?
            };
            let value = bridge::object(&[
                ("filename", JsValue::from_str(&path_url(path)?)),
                ("runtime", runtime),
            ])?;
            bridge::method(&reload_map, "set", &[plugin, value])?;
        }
    }
    let mut backup = CacheBackup {
        esm: Vec::new(),
        cjs: Vec::new(),
        candidate_modules: Vec::new(),
        paths: realm.borrow().paths.clone(),
        reloads: reload_map,
    };
    for url in accepted {
        let key = JsValue::from_str(&url);
        backup.esm.push((url.clone(), map_get(&esm, &key)?));
        map_delete(&esm, &key)?;
        if let Ok(path) = url_path(&url) {
            let previous = bridge::get(&cjs, &path)?;
            if !previous.is_undefined() {
                backup.cjs.push((path.clone(), previous));
                delete(&cjs, &JsValue::from_str(&path))?;
            }
        }
    }
    realm.borrow_mut().backup = Some(backup);
    Ok(())
}

fn snapshot_fiber(realm: &Rc<RefCell<Realm>>, snapshot: &Value) -> Result<JsValue, JsValue> {
    let key = snapshot["key"].as_str().unwrap_or_default();
    let active = realm
        .borrow()
        .activations
        .values()
        .find(|activation| activation.borrow().native_key == key)
        .cloned();
    let fiber = active.map_or_else(
        || bridge::object(&[]),
        |activation| Ok(activation.borrow().fiber.clone()),
    )?;
    for field in ["uid", "state"] {
        bridge::set_key(
            &fiber,
            &JsValue::from_str(field),
            &bridge::from_json(&snapshot[field])?,
        )?;
    }
    if bridge::get(&fiber, "_config")?.is_undefined() {
        let config = bridge::from_json(&snapshot["config"])?;
        bridge::set_key(&fiber, &JsValue::from_str("_config"), &config)?;
        bridge::set_key(&fiber, &JsValue::from_str("config"), &config)?;
    }
    if bridge::get(&fiber, "entry")?.is_undefined() {
        bridge::set_key(
            &fiber,
            &JsValue::from_str("entry"),
            &bridge::from_json(&snapshot["entry"])?,
        )?;
    }
    Ok(fiber)
}

fn rollback(realm: &Rc<RefCell<Realm>>) -> Result<(), JsValue> {
    let Some(backup) = realm.borrow_mut().backup.take() else {
        return Ok(());
    };
    let esm = cache()?;
    for (url, job) in backup.esm {
        map_set(&esm, &JsValue::from_str(&url), &job)?;
    }
    let cjs = bridge::get(&bridge::api("require")?, "cache")?;
    for (path, module) in backup.cjs {
        bridge::set_key(&cjs, &JsValue::from_str(&path), &module)?;
    }
    for id in backup.candidate_modules {
        realm.borrow_mut().modules.remove(&id);
    }
    realm.borrow_mut().paths = backup.paths;
    Ok(())
}

fn strings(request: &Value, key: &str) -> Vec<String> {
    request[key]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

async fn dispatch(realm: &Rc<RefCell<Realm>>, request: &Value) -> Result<Value, JsValue> {
    match request["action"].as_str().unwrap_or_default() {
        "import" => import(realm, request["path"].as_str().unwrap_or_default()).await,
        "resolve" => {
            resolve_path(
                request["specifier"].as_str().unwrap_or_default(),
                request["base"].as_str().unwrap_or_default(),
            )
            .await
        }
        "watch" => crate::loader_watch::open(realm, request),
        "closeWatch" => {
            crate::loader_watch::close(realm, request["watch"].as_u64().unwrap_or_default()).await
        }
        "contains" => {
            let path = request["path"].as_str().unwrap_or_default();
            let url = path_url(path)?;
            let esm = bridge::method(&cache()?, "has", &[JsValue::from_str(&url)])?.is_truthy();
            Ok(json!(esm))
        }
        "begin" => begin(realm, request).await,
        "commit" => {
            let backup = realm.borrow_mut().backup.take();
            if let Some(backup) = backup {
                let values = bridge::method(&backup.reloads, "values", &[])?;
                for reload in Array::from(&values).iter() {
                    let runtime = bridge::get(&reload, "runtime")?;
                    if runtime.is_undefined() {
                        continue;
                    }
                    for fiber in Array::from(&bridge::get(&runtime, "fibers")?).iter() {
                        bridge::set_key(&fiber, &JsValue::from_str("uid"), &JsValue::NULL)?;
                        bridge::set_key(
                            &fiber,
                            &JsValue::from_str("state"),
                            &JsValue::from_f64(5.0),
                        )?;
                    }
                }
                realm.borrow_mut().last_reload = backup.reloads;
            }
            Ok(Value::Null)
        }
        "rollback" => {
            rollback(realm)?;
            Ok(Value::Null)
        }
        "activate" => {
            let module = request["module"]
                .as_u64()
                .ok_or_else(|| bridge::error("missing module identity"))?;
            let plugin = realm
                .borrow()
                .modules
                .get(&module)
                .cloned()
                .ok_or_else(|| bridge::error("module generation is no longer available"))?;
            activate(realm, &plugin, request).await
        }
        "deactivate" => deactivate(realm, request["activation"].as_u64().unwrap_or_default()).await,
        "invoke" => invoke(realm, request).await,
        "toolInvoke" => Ok(crate::loader_tools::invoke(realm, request).await),
        "release" => {
            if let Some(module) = request["module"].as_u64() {
                realm.borrow_mut().modules.remove(&module);
            }
            Ok(Value::Null)
        }
        "shutdown" => {
            let watchers = realm.borrow().watchers.keys().copied().collect::<Vec<_>>();
            for id in watchers {
                let _ = crate::loader_watch::close(realm, id).await;
            }
            let ids = realm
                .borrow()
                .activations
                .keys()
                .copied()
                .collect::<Vec<_>>();
            for id in ids {
                let _ = deactivate(realm, id).await;
            }
            realm.borrow_mut().modules.clear();
            realm.borrow_mut().services.clear();
            Ok(Value::Null)
        }
        action => Err(bridge::error(&format!(
            "unknown plugin realm command {action}"
        ))),
    }
}

pub(crate) fn error_wire(error: &JsValue) -> Value {
    if !error.is_object() || error.is_null() || bridge::is_array(error) {
        return wire(error).unwrap_or_else(|_| json!(bridge::message(error)));
    }
    let mut result = wire(error).unwrap_or_else(|_| json!({}));
    if !result.is_object() {
        result = json!({"thrown":result});
    }
    for key in [
        "name", "message", "stack", "errors", "warnings", "code", "cause",
    ] {
        if let Ok(value) = bridge::get(error, key)
            && !value.is_undefined()
            && let Ok(value) = wire(&value)
        {
            result[key] = value;
        }
    }
    if result["message"].is_null() {
        result["message"] = json!(bridge::message(error));
    }
    result
}

fn receive(realm: &Rc<RefCell<Realm>>, message: Value) {
    if let Some(call) = message["call"].as_u64() {
        let pending = realm.borrow_mut().pending.remove(&call);
        if let Some((resolve, reject)) = pending {
            if let Some(error) = message.get("error") {
                let _ = bridge::apply(
                    &reject,
                    &JsValue::UNDEFINED,
                    &[bridge::error(
                        error.as_str().unwrap_or("Host callback failed"),
                    )],
                );
            } else if let Ok(value) = bridge::from_json(&message["result"]) {
                let _ = bridge::apply(&resolve, &JsValue::UNDEFINED, &[value]);
            }
        }
        return;
    }
    let realm = realm.clone();
    spawn_local(async move {
        let result = dispatch(&realm, &message).await;
        let response = match result {
            Ok(value) => json!({"id":message["id"],"result":value}),
            Err(error) => json!({"id":message["id"],"error":error_wire(&error)}),
        };
        let _ = emit(&realm, &response);
    });
}

pub(crate) fn start(apis: &JsValue) -> Result<(), JsValue> {
    let process = bridge::get(apis, "process")?;
    let env = bridge::get(&process, "env")?;
    let address = bridge::get(&env, "SEEKDEEP_PLUGIN_NODE_SOCKET")?;
    let address = bridge::parse(&bridge::string(&address)?)?;
    let connection = bridge::apply(
        &bridge::get(apis, "createConnection")?,
        &JsValue::UNDEFINED,
        &[address],
    )?;
    bridge::method(&connection, "setEncoding", &[JsValue::from_str("utf8")])?;
    let realm = Rc::new(RefCell::new(Realm {
        connection: connection.clone(),
        input: String::new(),
        modules: BTreeMap::new(),
        paths: BTreeMap::new(),
        next_module: 1,
        activations: BTreeMap::new(),
        services: BTreeMap::new(),
        next_service: 1,
        pending: BTreeMap::new(),
        next_call: 1,
        callbacks: Vec::new(),
        backup: None,
        last_reload: JsValue::UNDEFINED,
        watchers: BTreeMap::new(),
        intrinsics: crate::snapshot::Intrinsics::capture()?,
    }));
    let owner = realm.clone();
    let callback = Closure::<dyn FnMut(JsValue)>::new(move |chunk: JsValue| {
        let Some(chunk) = chunk.as_string() else {
            return;
        };
        owner.borrow_mut().input.push_str(&chunk);
        loop {
            let line = {
                let mut owner = owner.borrow_mut();
                let Some(end) = owner.input.find('\n') else {
                    break;
                };
                owner.input.drain(..=end).collect::<String>()
            };
            if let Ok(message) = serde_json::from_str(&line) {
                receive(&owner, message);
            }
        }
    })
    .into_js_value();
    bridge::method(
        &connection,
        "on",
        &[JsValue::from_str("data"), callback.clone()],
    )?;
    realm.borrow_mut().callbacks.push(callback);
    let callback = Closure::<dyn FnMut()>::new(move || {
        let _ = bridge::method(&process, "exit", &[JsValue::from_f64(0.0)]);
    })
    .into_js_value();
    bridge::method(
        &connection,
        "on",
        &[JsValue::from_str("close"), callback.clone()],
    )?;
    realm.borrow_mut().callbacks.push(callback);
    emit(&realm, &json!({"ready":true}))?;
    REALM.with(|slot| *slot.borrow_mut() = Some(realm));
    Ok(())
}
