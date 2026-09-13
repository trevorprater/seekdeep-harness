//! Live browser sources for the Rust-owned Client Inspect provider directory.

use std::sync::Arc;

use futures::FutureExt;
use serde_json::{Value, json};
use wasm_bindgen::JsValue;

use crate::{
    ClientInspectProviderSources, LiveSlotNode, call_method, generated_client_inspect_sources,
    js_anyhow,
};

/// Connects generated catalogs to the live page Slot and Theme services.
#[must_use]
pub fn wasm_client_inspect_sources(ctx: JsValue) -> ClientInspectProviderSources {
    let slot_context = ctx.clone();
    let slots = Arc::new(move |root: Option<String>| {
        let ctx = slot_context.clone();
        async move {
            let service = call_method(&ctx, "get", &[JsValue::from_str("slots")])
                .map_err(|error| js_anyhow(&error))?;
            anyhow::ensure!(
                !service.is_undefined(),
                "Client Slots service is not running"
            );
            let root = root.map_or(JsValue::UNDEFINED, |root| JsValue::from_str(&root));
            let trees =
                call_method(&service, "snapshot", &[root]).map_err(|error| js_anyhow(&error))?;
            serde_wasm_bindgen::from_value::<Vec<LiveSlotNode>>(trees).map_err(Into::into)
        }
        .boxed()
    });
    let theme = Arc::new(move || {
        let ctx = ctx.clone();
        async move {
            let service = call_method(&ctx, "get", &[JsValue::from_str("theme")])
                .map_err(|error| js_anyhow(&error))?;
            anyhow::ensure!(
                !service.is_undefined(),
                "Client Theme service is not running"
            );
            let tokens = call_method(&service, "exportInspectTokens", &[])
                .map_err(|error| js_anyhow(&error))?;
            let tokens: Value = serde_wasm_bindgen::from_value(tokens)?;
            Ok(json!({"tokens": tokens, "referencedTypes": []}))
        }
        .boxed()
    });
    generated_client_inspect_sources(slots, theme)
}
