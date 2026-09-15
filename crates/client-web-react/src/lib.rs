//! React bindings for framework-neutral Client snapshots, sessions, and slots.

mod invoke;
mod renderer;
mod session;
#[cfg(target_arch = "wasm32")]
mod wasm;

pub use invoke::*;
pub use renderer::*;
pub use session::*;
#[cfg(target_arch = "wasm32")]
pub use wasm::*;

/// Stable no-op invariant companion identity.
pub const INVARIANT_NAME: &str = "client-web-react-invariant";
