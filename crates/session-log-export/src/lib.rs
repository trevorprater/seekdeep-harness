//! Session-log ZIP command and browser download controller.

#[cfg(not(target_arch = "wasm32"))]
pub mod command;
pub mod controller;
pub mod locales;

#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

#[cfg(not(target_arch = "wasm32"))]
pub use command::{INJECT, NAME, apply, plugin};
pub use controller::*;

/// Registers the package-owned explained-empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant-registry failures.
#[cfg(not(target_arch = "wasm32"))]
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        "@deepseek-ai/seekdeep-session-log-export",
        InvariantInstaller::noop(),
    )
}
