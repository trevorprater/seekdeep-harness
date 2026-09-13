//! Explained-empty package invariant for Typert protocol declarations.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

const PACKAGE_NAME: &str = "seekdeep-typert-protocol";

/// Registers ownership for a package whose declarations have no event stream.
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
