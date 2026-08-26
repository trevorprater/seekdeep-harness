//! Host-backed portable Rust/WASM locale registry and language preference UI.

mod core;
#[cfg(not(target_arch = "wasm32"))]
mod host;
mod row_store;
#[cfg(target_arch = "wasm32")]
mod wasm;

pub use core::*;
#[cfg(not(target_arch = "wasm32"))]
pub use host::*;
pub use row_store::*;
#[cfg(target_arch = "wasm32")]
pub use wasm::*;
