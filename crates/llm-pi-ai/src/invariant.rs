//! Explained-empty package invariant companion.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

/// Companion plugin identity.
pub const INVARIANT_NAME: &str = "llm-pi-ai-invariant";
const PACKAGE_NAME: &str = "@deepseek-ai/seekdeep-llm-pi-ai";

/// Reserves package identity; the adapter owns no independent mutable relation.
///
/// # Errors
///
/// Returns ordinary invariant-registry failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}
