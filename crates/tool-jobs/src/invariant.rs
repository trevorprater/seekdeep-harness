//! Package-owned invariant companion for `seekdeep-tool-jobs`.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

/// Package name reserved by this companion.
pub const PACKAGE_NAME: &str = "@deepseek-ai/seekdeep-tool-jobs";

/// Cordis companion plugin name.
pub const NAME: &str = "tool-jobs-invariant";

/// Services required before the companion can reserve package ownership.
pub const INJECT: &[&str] = &["invariants"];

/// Registers this package's invariant companion.
///
/// This model-facing adapter has no independent lifecycle stream; execution
/// relations are owned by the capability seam it calls, so the runtime
/// invariant is intentionally empty.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}
