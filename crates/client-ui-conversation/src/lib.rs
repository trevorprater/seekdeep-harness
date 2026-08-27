//! Conversation UI semantic core and Rust/WASM surfaces.

mod images;
mod metrics;
mod submission;

#[cfg(not(target_arch = "wasm32"))]
mod host;

pub use images::*;
pub use metrics::*;
pub use submission::*;

#[cfg(not(target_arch = "wasm32"))]
pub use host::*;

/// Stable no-op invariant companion identity.
pub const INVARIANT_NAME: &str = "client-ui-conversation-invariant";
