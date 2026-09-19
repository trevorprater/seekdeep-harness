//! Compiled worker and supervisor boundaries over the native Node runtime.

#[cfg(target_arch = "wasm32")]
mod bridge;
#[cfg(target_arch = "wasm32")]
mod host;
#[cfg(target_arch = "wasm32")]
mod loader;
#[cfg(target_arch = "wasm32")]
mod loader_context;
#[cfg(target_arch = "wasm32")]
mod loader_tools;
#[cfg(target_arch = "wasm32")]
mod loader_watch;
#[cfg(target_arch = "wasm32")]
mod snapshot;
#[cfg(target_arch = "wasm32")]
mod worker;

#[path = "../../src/output_json.rs"]
pub mod output_json;
#[path = "../../src/worker_json.rs"]
pub mod worker_json;
pub use seekdeep_lossless_json::{
    JsonString as CodeJsonString, JsonToken as CodeJsonToken, JsonValue as CodeJsonValue,
};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

/// Starts the Rust-owned Node supervisor or one model worker.
///
/// # Errors
///
/// Returns native Node bootstrap and compatibility-protocol failures.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn start(apis: &JsValue) -> Result<(), JsValue> {
    bridge::install(apis.clone())?;
    if bridge::get(apis, "isMainThread")?.as_bool() == Some(true) {
        host::start(apis)
    } else {
        worker::start(apis)
    }
}

/// Starts one persistent, native Node module realm for a Host plugin catalog.
///
/// # Errors
///
/// Returns unavailable internal loaders, transport, and bootstrap failures.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn start_loader(apis: &JsValue) -> Result<(), JsValue> {
    bridge::install(apis.clone())?;
    loader::start(apis)
}
