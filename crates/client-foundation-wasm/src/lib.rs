//! Rust/WASM browser Connection, Typert, and API gateway foundations.

#[cfg(target_arch = "wasm32")]
mod wasm;

#[cfg(target_arch = "wasm32")]
pub use wasm::*;
