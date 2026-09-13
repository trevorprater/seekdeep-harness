//! Explained-empty package invariant companion.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

/// Registers the package-owned explained-empty invariant.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register(registry: &Arc<InvariantRegistry>) -> anyhow::Result<InvariantRegistration> {
    registry.register("seekdeep-sandbox", InvariantInstaller::noop())
}
