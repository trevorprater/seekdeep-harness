//! Explained-empty invariant companion for the API Proxy contract layer.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

/// Stable invariant companion name.
pub const INVARIANT_NAME: &str = "host-apiproxy-invariant";
const PACKAGE_NAME: &str = "@deepseek-ai/seekdeep-host-apiproxy";

/// Registers package ownership for carrier-local schema/correlation invariants.
///
/// # Errors
///
/// Returns ordinary invariant registry failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}
