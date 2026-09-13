//! Rust/WASM browser Loader over the compiled Cordis Context face.

#[cfg(target_arch = "wasm32")]
mod wasm;

#[cfg(target_arch = "wasm32")]
pub use wasm::*;

/// Loader package identity.
pub const NAME: &str = "loader";
