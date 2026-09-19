//! Node installation behavior compiled into the package's self-contained bootstrap.
//!
//! The public compatibility declarations retain node-pty types. Its prebuilt helper
//! repair must run before a checkout has built its Rust workspace and on npm consumers
//! with only Node installed, so the binding includes the compiled WebAssembly bytes.

#[cfg(target_arch = "wasm32")]
mod wasm;

#[cfg(target_arch = "wasm32")]
pub use wasm::ensure_spawn_helpers;
