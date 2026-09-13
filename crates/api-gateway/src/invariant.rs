//! Explained-empty invariant for Gateway request-local validation.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

const PACKAGE_NAME: &str = "seekdeep-api-gateway";

/// Registers ownership for operation-local Gateway invariants.
///
/// # Errors
///
/// Returns ordinary invariant registry failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        PACKAGE_NAME,
        InvariantInstaller::new(std::iter::empty::<String>(), |_context, _failure| async {
            Ok(())
        }),
    )
}
