//! Package-owned explained-empty invariant companion.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

/// Stable invariant companion name.
pub const INVARIANT_NAME: &str = "client-connection-invariant";
const PACKAGE_NAME: &str = "@deepseek-ai/seekdeep-client-connection";

/// Reserves package ownership with an intentionally empty installer.
///
/// # Errors
///
/// Returns duplicate registration failures from the invariant registry.
pub fn install_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}
