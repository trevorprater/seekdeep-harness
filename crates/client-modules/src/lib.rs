//! Lazy Client module table, boot-manifest contract, and browser bindings.

#[cfg(not(target_arch = "wasm32"))]
mod host;
#[cfg(not(target_arch = "wasm32"))]
mod invariant;
mod manifest;
mod system;
#[cfg(target_arch = "wasm32")]
mod wasm;

#[cfg(not(target_arch = "wasm32"))]
pub use host::*;
#[cfg(not(target_arch = "wasm32"))]
pub use invariant::*;
pub use manifest::*;
pub use system::*;
#[cfg(target_arch = "wasm32")]
pub use wasm::*;
