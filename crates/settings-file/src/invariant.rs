//! Package-owned file-settings invariant companion.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

/// Full source-compatible package identity.
pub const PACKAGE_NAME: &str = "@deepseek-ai/seekdeep-settings-file";
/// Stable companion name.
pub const INVARIANT_NAME: &str = "settings-file-invariant";

/// Registers the explained empty runtime invariant.
///
/// File round-trip, watcher timing, and atomic-write behavior are IO contracts
/// covered by provider tests; in-process commit relations belong to settings.
///
/// # Errors
///
/// Returns invariant-registry failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}
