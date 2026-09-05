//! Package-owned JSONL persistence invariant registration.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

/// Stable invariant companion name.
pub const INVARIANT_NAME: &str = "session-persistence-jsonl-invariant";
const PACKAGE_NAME: &str = "@deepseek-ai/seekdeep-session-persistence-jsonl";

/// Registers the explained-empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant-registry failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}
