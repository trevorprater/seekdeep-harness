//! Route/disposer symmetry invariant companion.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

/// Stable invariant companion name.
pub const INVARIANT_NAME: &str = "host-webserver-invariant";
const PACKAGE_NAME: &str = "@deepseek-ai/seekdeep-host-webserver";

/// Registers ownership of route-registration symmetry.
///
/// # Errors
///
/// Returns ordinary invariant registry failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}
