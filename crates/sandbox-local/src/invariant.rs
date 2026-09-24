//! Package-owned no-op invariant companion.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

/// Package ownership key.
pub const PACKAGE_NAME: &str = "seekdeep-sandbox-local";

/// Reserves package ownership without inventing an independent lifecycle stream.
///
/// # Errors
///
/// Returns duplicate registration or inactive-owner failures.
pub fn register(invariants: &Arc<InvariantRegistry>) -> anyhow::Result<InvariantRegistration> {
    invariants.register(PACKAGE_NAME, InvariantInstaller::noop())
}
