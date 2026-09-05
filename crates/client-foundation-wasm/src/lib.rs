//! Rust/WASM browser Connection, Typert, and API gateway foundations.

#[cfg(target_arch = "wasm32")]
mod wasm;
#[cfg(target_arch = "wasm32")]
mod wasm_remote;
#[cfg(target_arch = "wasm32")]
mod wasm_typert;

#[cfg(target_arch = "wasm32")]
pub use wasm::*;
#[cfg(target_arch = "wasm32")]
pub use wasm_remote::*;
