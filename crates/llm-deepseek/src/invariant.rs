//! Explained-empty invariant companion for the DeepSeek adapter.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

/// Companion plugin name.
pub const INVARIANT_NAME: &str = "llm-deepseek-invariant";
const PACKAGE_NAME: &str = "@deepseek-ai/seekdeep-llm-deepseek";

/// Reserves package identity; the adapter has no independent mutable relation.
///
/// # Errors
///
/// Returns ordinary invariant-registry failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}
