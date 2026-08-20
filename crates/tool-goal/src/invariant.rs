//! Package-owned invariant companion for `seekdeep-tool-goal`.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

/// Package name reserved by this companion.
pub const PACKAGE_NAME: &str = "@deepseek-ai/seekdeep-tool-goal";

/// Cordis companion plugin name.
pub const NAME: &str = "tool-goal-invariant";

/// Services required before the companion can reserve package ownership.
pub const INJECT: &[&str] = &["invariants"];

/// Registers this package's invariant companion.
///
/// This model-facing adapter owns no independent state or event protocol;
/// accepted mutations are checked by the goal domain and authority behavior is
/// package-tested, so the runtime invariant is intentionally empty.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}
