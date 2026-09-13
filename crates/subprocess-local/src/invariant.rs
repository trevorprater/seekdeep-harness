//! Explained-empty invariant companion for the local subprocess provider.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

/// Registers the local provider's explained-empty invariant package.
///
/// Process ownership and teardown are enforced directly by the runtime.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register("seekdeep-subprocess-local", InvariantInstaller::noop())
}
