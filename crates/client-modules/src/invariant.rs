//! Package-owned graph/path consistency invariant companion.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

/// Stable invariant companion name.
pub const CLIENT_MODULES_INVARIANT_NAME: &str = "client-modules-invariant";
const PACKAGE_NAME: &str = "@seekdeep-ai/seekdeep-client-modules";

/// Reserves the graph/path relation, which the Rust host preserves by construction.
///
/// A row and its path are inserted and removed in the same `WebPluginRecord`
/// transaction, so there is no intermediate state for an event-time scan to
/// detect. The no-op installer records that stronger ownership boundary.
///
/// # Errors
///
/// Returns ordinary duplicate or unavailable invariant-registry failures.
pub fn register_client_modules_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}
