//! Browser assembly of generated Host Remote contributions.

#[cfg(target_arch = "wasm32")]
mod wasm;

#[cfg(target_arch = "wasm32")]
pub use wasm::*;

/// Exact Client plugin dependency.
pub const INJECT: &[&str] = &["remote"];
