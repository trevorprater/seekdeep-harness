//! Browser DOM, task, and microtask adapters for the Rust/WASM Client core.

use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    sync::Arc,
};

use js_sys::{Function, Promise};
use seekdeep_cordis_dynamic_types::CordisDynamicPluginId;
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;

use crate::{
    ClientMicrotaskScheduler, ClientTaskSpawner, DynamicCordisStyles, StyleDom, StyleTagId,
};

thread_local! {
    static STYLE_TAGS: RefCell<BTreeMap<StyleTagId, web_sys::HtmlStyleElement>> = const { RefCell::new(BTreeMap::new()) };
    static NEXT_STYLE_TAG_ID: Cell<u64> = const { Cell::new(0) };
}

/// `spawn_local` implementation for automatic Client work.
#[derive(Clone, Copy, Debug, Default)]
pub struct WasmClientTaskSpawner;

impl ClientTaskSpawner for WasmClientTaskSpawner {
    fn spawn(&self, future: futures::future::BoxFuture<'static, ()>) {
        wasm_bindgen_futures::spawn_local(future);
    }
}

/// Promise-microtask implementation for coalesced manifest publication.
#[derive(Clone, Copy, Debug, Default)]
pub struct WasmClientMicrotaskScheduler;

impl ClientMicrotaskScheduler for WasmClientMicrotaskScheduler {
    fn queue(&self, callback: Box<dyn FnOnce() + Send>) {
        wasm_bindgen_futures::spawn_local(async move {
            let _ = JsFuture::from(Promise::resolve(&JsValue::UNDEFINED)).await;
            callback();
        });
    }
}

#[derive(Debug, Default)]
pub(crate) struct WasmStyleDom;

impl StyleDom for WasmStyleDom {
    fn insert(&self, plugin_id: &CordisDynamicPluginId, css: &str) -> anyhow::Result<StyleTagId> {
        let document = web_sys::window()
            .and_then(|window| window.document())
            .ok_or_else(|| anyhow::anyhow!("styles.insert requires browser Document"))?;
        let tag = document
            .create_element("style")
            .map_err(|error| js_error(&error))?
            .dyn_into::<web_sys::HtmlStyleElement>()
            .map_err(|element| js_error(&element.into()))?;
        tag.set_attribute("data-dyn", plugin_id.as_str())
            .map_err(|error| js_error(&error))?;
        tag.set_text_content(Some(css));
        document
            .head()
            .ok_or_else(|| anyhow::anyhow!("styles.insert requires document.head"))?
            .append_child(&tag)
            .map_err(|error| js_error(&error))?;
        let id = NEXT_STYLE_TAG_ID.with(|next| {
            let id = next.get().checked_add(1).expect("style tag id exhausted");
            next.set(id);
            StyleTagId::new(id)
        });
        STYLE_TAGS.with(|tags| {
            tags.borrow_mut().insert(id, tag);
        });
        Ok(id)
    }

    fn remove(&self, tag: StyleTagId) {
        STYLE_TAGS.with(|tags| {
            if let Some(tag) = tags.borrow_mut().remove(&tag) {
                tag.remove();
            }
        });
    }
}

/// JavaScript-facing package-owned stylesheet binding.
#[wasm_bindgen]
pub struct WasmDynamicCordisStyles {
    inner: DynamicCordisStyles,
}

#[wasm_bindgen]
impl WasmDynamicCordisStyles {
    /// Creates style ownership for one stable Plugin.
    #[wasm_bindgen(constructor)]
    pub fn new(plugin_id: String) -> Self {
        Self {
            inner: DynamicCordisStyles::new(
                CordisDynamicPluginId::new(plugin_id),
                Arc::new(WasmStyleDom),
            ),
        }
    }

    /// Inserts CSS and returns an idempotent JavaScript disposer.
    ///
    /// # Errors
    ///
    /// Returns a JavaScript error for non-string CSS or DOM insertion failure.
    #[allow(clippy::needless_pass_by_value)]
    pub fn insert(&self, css: JsValue) -> Result<Function, JsValue> {
        let css = css
            .as_string()
            .ok_or_else(|| js_sys::Error::new("styles.insert(css) needs a CSS string"))?;
        let disposer = self
            .inner
            .insert(&css)
            .map_err(|error| js_sys::Error::new(&error.to_string()))?;
        let closure = Closure::wrap(Box::new(move || disposer.dispose()) as Box<dyn FnMut()>);
        Ok(closure.into_js_value().unchecked_into())
    }

    /// Live tag count.
    #[wasm_bindgen(getter)]
    pub fn count(&self) -> usize {
        self.inner.count()
    }

    /// Removes every still-live package style.
    pub fn dispose(&self) {
        self.inner.dispose();
    }
}

fn js_error(error: &JsValue) -> anyhow::Error {
    anyhow::anyhow!(format!("{error:?}"))
}
