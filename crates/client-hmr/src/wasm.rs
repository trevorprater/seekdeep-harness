//! Browser Client HMR bindings.

use std::sync::Arc;

use futures::{FutureExt, channel::oneshot, future::BoxFuture};
use js_sys::{Array, Function, Object, Promise, Reflect};
use parking_lot::Mutex;
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    ClientHmrLogger, ClientHmrPlatform, ClientHmrRuntime, ClientHmrSpawner, EVENTS_ENDPOINT,
    parse_plugins_event_frame,
};

/// `spawn_local` executor for serialized reload tails.
#[derive(Clone, Copy, Debug, Default)]
pub struct WasmClientHmrSpawner;

impl ClientHmrSpawner for WasmClientHmrSpawner {
    fn spawn(&self, future: BoxFuture<'static, ()>) {
        wasm_bindgen_futures::spawn_local(future);
    }
}

/// Actual Loader/module-table/DOM swap adapter.
#[derive(Clone)]
pub struct WasmClientHmrPlatform {
    loader: JsValue,
    modules: JsValue,
}

impl std::fmt::Debug for WasmClientHmrPlatform {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WasmClientHmrPlatform")
            .finish_non_exhaustive()
    }
}

impl WasmClientHmrPlatform {
    /// Wraps the real Client Loader and Rust/WASM module table services.
    #[must_use]
    pub fn new(loader: JsValue, modules: JsValue) -> Self {
        Self { loader, modules }
    }

    async fn reload_inner(&self, id: &str) -> anyhow::Result<()> {
        let Some(entry) = find_entry(&self.loader, id)? else {
            web_sys::console::warn_1(&JsValue::from_str(&format!(
                "client-hmr: rebuilt frame for unknown entry {id:?} (not in the loader tree)"
            )));
            return Ok(());
        };
        call_method(&self.modules, "invalidate", &[JsValue::from_str(id)])
            .map_err(|error| js_error(&error))?;
        let prefetch = call_method(&self.modules, "prefetch", &[JsValue::from_str(id)])
            .map_err(|error| js_error(&error))?;
        let prefetch = Promise::resolve(&prefetch);
        promise_result(&prefetch)
            .await
            .map_err(|error| js_error(&error))?;

        let old_fiber =
            Reflect::get(&entry, &JsValue::from_str("fiber")).map_err(|error| js_error(&error))?;
        if !old_fiber.is_undefined() {
            let runtime = Reflect::get(&old_fiber, &JsValue::from_str("runtime"))
                .map_err(|error| js_error(&error))?;
            if !runtime.is_null() {
                let callback = Reflect::get(&runtime, &JsValue::from_str("callback"))
                    .map_err(|error| js_error(&error))?;
                let ctx = Reflect::get(&entry, &JsValue::from_str("ctx"))
                    .map_err(|error| js_error(&error))?;
                let registry = Reflect::get(&ctx, &JsValue::from_str("registry"))
                    .map_err(|error| js_error(&error))?;
                call_method(&registry, "delete", &[callback]).map_err(|error| js_error(&error))?;
            }
            loop {
                let inertia = Reflect::get(&old_fiber, &JsValue::from_str("inertia"))
                    .map_err(|error| js_error(&error))?;
                if inertia.is_undefined() {
                    break;
                }
                let inertia = Promise::resolve(&inertia);
                promise_result(&inertia)
                    .await
                    .map_err(|error| js_error(&error))?;
            }
            Reflect::delete_property(
                &entry.clone().dyn_into::<Object>().map_err(|value| {
                    anyhow::anyhow!("client-hmr entry is not an object: {value:?}")
                })?,
                &JsValue::from_str("fiber"),
            )
            .map_err(|error| js_error(&error))?;
        }
        remove_owned_styles(id);
        let refreshed = call_method(&entry, "refresh", &[]).map_err(|error| js_error(&error))?;
        let refreshed = Promise::resolve(&refreshed);
        promise_result(&refreshed)
            .await
            .map_err(|error| js_error(&error))?;
        let fiber =
            Reflect::get(&entry, &JsValue::from_str("fiber")).map_err(|error| js_error(&error))?;
        if !fiber.is_undefined() {
            let settled = call_method(&fiber, "await", &[]).map_err(|error| js_error(&error))?;
            let settled = Promise::resolve(&settled);
            promise_result(&settled)
                .await
                .map_err(|error| js_error(&error))?;
        }
        Ok(())
    }
}

impl ClientHmrPlatform for WasmClientHmrPlatform {
    fn reload(&self, id: String) -> BoxFuture<'static, anyhow::Result<()>> {
        let platform = self.clone();
        async move { platform.reload_inner(&id).await }.boxed()
    }
}

/// Builds the browser Client HMR plugin descriptor.
///
/// # Errors
///
/// Returns JavaScript construction failures.
#[wasm_bindgen(js_name = clientHmrPlugin)]
pub fn client_hmr_plugin() -> Result<JsValue, JsValue> {
    let apply =
        Closure::wrap(
            Box::new(move |ctx: JsValue| -> Result<(), JsValue> { apply_client_hmr(ctx) })
                as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>,
        );
    let plugin = Object::new();
    set(&plugin, "name", &JsValue::from_str("client-hmr"))?;
    let inject = Array::new();
    inject.push(&JsValue::from_str("loader"));
    inject.push(&JsValue::from_str("modules"));
    set(&plugin, "inject", &inject.into())?;
    set(&plugin, "apply", &apply.into_js_value())?;
    Ok(plugin.into())
}

/// Mounts the `EventSource` receiver into one Client Cordis Context.
///
/// # Errors
///
/// Returns missing services, `EventSource` construction, or effect registration
/// failures.
#[wasm_bindgen(js_name = applyClientHmr)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_client_hmr(ctx: JsValue) -> Result<(), JsValue> {
    let loader = required_service(&ctx, "loader")?;
    let modules = required_service(&ctx, "modules")?;
    let runtime = ClientHmrRuntime::new(
        Arc::new(WasmClientHmrPlatform::new(loader, modules)),
        Arc::new(WasmClientHmrSpawner),
        wasm_logger(),
    );
    let source = web_sys::EventSource::new(EVENTS_ENDPOINT)?;
    let listener_runtime = runtime;
    let listener = Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
        let Some(data) = event.data().as_string() else {
            web_sys::console::warn_1(&JsValue::from_str(
                "client-hmr: unparseable event frame: [non-string]",
            ));
            return;
        };
        let frame = serde_json::from_str(&data)
            .map_err(anyhow::Error::from)
            .and_then(|value| parse_plugins_event_frame(&value));
        match frame {
            Ok(frame) => listener_runtime.handle(frame),
            Err(_) => web_sys::console::warn_1(&JsValue::from_str(&format!(
                "client-hmr: unparseable event frame: {data}"
            ))),
        }
    }) as Box<dyn FnMut(web_sys::MessageEvent)>);
    source.set_onmessage(Some(listener.as_ref().unchecked_ref()));
    let installer = Closure::once_into_js(move || -> JsValue {
        let source = source;
        let listener = listener;
        let disposer = Closure::wrap(Box::new(move || {
            source.close();
            let _ = &listener;
        }) as Box<dyn FnMut()>);
        disposer.into_js_value()
    });
    call_method(
        &ctx,
        "effect",
        &[installer, JsValue::from_str("client-hmr: event source")],
    )?;
    Ok(())
}

fn find_entry(loader: &JsValue, id: &str) -> anyhow::Result<Option<JsValue>> {
    let entries = call_method(loader, "entries", &[]).map_err(|error| js_error(&error))?;
    let iterator = js_sys::try_iter(&entries)
        .map_err(|error| js_error(&error))?
        .ok_or_else(|| anyhow::anyhow!("client-hmr: loader.entries() is not iterable"))?;
    for entry in iterator {
        let entry = entry.map_err(|error| js_error(&error))?;
        let options = Reflect::get(&entry, &JsValue::from_str("options"))
            .map_err(|error| js_error(&error))?;
        let name =
            Reflect::get(&options, &JsValue::from_str("name")).map_err(|error| js_error(&error))?;
        if name.as_string().as_deref() == Some(id) {
            return Ok(Some(entry));
        }
    }
    Ok(None)
}

fn remove_owned_styles(id: &str) {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let Ok(styles) = document.query_selector_all("style[data-plugin]") else {
        return;
    };
    for index in 0..styles.length() {
        let Some(element) = styles
            .get(index)
            .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
        else {
            continue;
        };
        if element.get_attribute("data-plugin").as_deref() == Some(id) {
            element.remove();
        }
    }
}

fn required_service(ctx: &JsValue, name: &str) -> Result<JsValue, JsValue> {
    let service = call_method(ctx, "get", &[JsValue::from_str(name)])?;
    if service.is_undefined() {
        Err(js_sys::Error::new(&format!("client-hmr requires service {name:?}")).into())
    } else {
        Ok(service)
    }
}

fn wasm_logger() -> ClientHmrLogger {
    Arc::new(|message, error| {
        web_sys::console::error_1(&JsValue::from_str(&message));
        if let Some(error) = error {
            web_sys::console::error_1(&JsValue::from_str(&error));
        }
    })
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

fn js_error(error: &JsValue) -> anyhow::Error {
    let message = Reflect::get(error, &JsValue::from_str("message"))
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_else(|| format!("{error:?}"));
    anyhow::anyhow!(message)
}
