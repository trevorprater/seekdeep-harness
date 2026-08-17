//! Package-owned no-op invariant companion and reversible reservation.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

const PACKAGE_NAME: &str = "@deepseek-ai/seekdeep-token-meter";

/// Reserves token-meter ownership in the invariant registry.
///
/// Estimates are per-call values and the three JSON projection schemas own
/// their durable boundaries, so no additional live observer is installed.
///
/// # Errors
///
/// Returns ordinary invariant-registry failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        PACKAGE_NAME,
        InvariantInstaller::new(std::iter::empty::<&str>(), |_, _| async { Ok(()) }),
    )
}
