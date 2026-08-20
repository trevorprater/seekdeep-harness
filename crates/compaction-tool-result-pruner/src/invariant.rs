//! Package-owned invariant companion for `seekdeep-compaction-tool-result-pruner`.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

/// Package name reserved by this companion.
pub const PACKAGE_NAME: &str = "@deepseek-ai/seekdeep-compaction-tool-result-pruner";

/// Cordis companion plugin name.
pub const NAME: &str = "compaction-tool-result-pruner-invariant";

/// Services required before the companion can reserve package ownership.
pub const INJECT: &[&str] = &["invariants"];

/// Registers this package's invariant companion.
///
/// The pruner owns no independent event protocol; its replacements are
/// validated by the compaction and session invariants, so the runtime
/// invariant is intentionally empty.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}
