//! Explained-empty invariant companion for the validating environment registry.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

const PACKAGE_NAME: &str = "seekdeep-shell-env";

/// Reserves package ownership with the invariant registry.
///
/// # Errors
///
/// Returns ordinary duplicate or lifecycle registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}
