//! Compiled Rust/WASM attachment atoms.

#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(target_arch = "wasm32")]
mod browser_lightbox;
#[cfg(target_arch = "wasm32")]
mod browser_message;
#[cfg(target_arch = "wasm32")]
mod browser_overlay;
#[cfg(target_arch = "wasm32")]
mod browser_rail;
#[cfg(target_arch = "wasm32")]
pub use browser::*;
