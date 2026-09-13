//! Explained-empty invariant companion for the owner-scoped terminal registry.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

const PACKAGE_NAME: &str = "seekdeep-terminal";

/// Reserves package ownership with no independent runtime invariant.
///
/// Backend and session registries are private mutable state; the service exposes
/// neither an independent lifecycle stream nor an unscoped snapshot.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}
