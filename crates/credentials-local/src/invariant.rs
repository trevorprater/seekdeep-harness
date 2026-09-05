//! Explained-empty invariant companion for the local credentials provider.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

/// Stable invariant companion name.
pub const INVARIANT_NAME: &str = "credentials-local-invariant";
const PACKAGE_NAME: &str = "@deepseek-ai/seekdeep-credentials-local";

/// Reserves the package's invariant identity.
///
/// The credentials seam owns the synchronous update lifecycle relation; this
/// provider's filesystem and environment relations require asynchronous I/O
/// and are covered by its behavioral suite.
///
/// # Errors
///
/// Returns ordinary invariant-registry failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}
