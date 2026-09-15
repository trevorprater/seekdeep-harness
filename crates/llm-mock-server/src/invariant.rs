//! Explained-empty invariant companion for the test-only HTTP server.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistry};

const PACKAGE_NAME: &str = "@seekdeep-ai/seekdeep-llm-mock-server";

/// Reserves the package's intentionally empty runtime invariant.
///
/// # Errors
///
/// Returns duplicate-registration or lifecycle failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<seekdeep_invariants::InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}
