//! Explained-empty invariant companion.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

/// Reserves native Windows ACL package ownership.
///
/// # Errors
///
/// Returns duplicate registration failures.
pub fn register(registry: &Arc<InvariantRegistry>) -> anyhow::Result<InvariantRegistration> {
    registry.register("seekdeep-sandbox-windows-acl", InvariantInstaller::noop())
}
