//! Package-owned invariant companion for seekdeep-web.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

/// Package name reserved by this companion.
pub const PACKAGE_NAME: &str = "@deepseek-ai/seekdeep-web";

/// Cordis companion plugin name.
pub const NAME: &str = "web-invariant";

/// Services required before the companion can reserve package ownership.
pub const INJECT: &[&str] = &["invariants"];

/// Registers this package's invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}
