//! Host bundle polling/SSE and browser plugin-swap runtime.

#[cfg(not(target_arch = "wasm32"))]
mod host;
mod protocol;
mod runtime;
#[cfg(target_arch = "wasm32")]
mod wasm;

#[cfg(not(target_arch = "wasm32"))]
pub use host::*;
pub use protocol::*;
pub use runtime::*;
#[cfg(target_arch = "wasm32")]
pub use wasm::*;
