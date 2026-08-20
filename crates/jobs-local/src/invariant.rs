//! Package-owned invariant companion for `seekdeep-jobs-local`.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

/// Package name reserved by this companion.
pub const PACKAGE_NAME: &str = "@deepseek-ai/seekdeep-jobs-local";

/// Cordis companion plugin name.
pub const NAME: &str = "jobs-local-invariant";

/// Services required before the companion can reserve package ownership.
pub const INJECT: &[&str] = &["invariants"];

/// Registers this package's invariant companion.
///
/// The `seekdeep-jobs` companion owns every per-snapshot identity, status,
/// timestamp, and owner check. This provider's admission decision uses private
/// configuration and must fail before a backend starter runs;
/// [`crate::LocalJobRegistry::start`] enforces it synchronously for current
/// producers. Repeating an aggregate after publication would expose private
/// configuration solely to this companion and would not verify the fail-closed
/// pre-start guarantee, so the runtime invariant is intentionally empty.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}
