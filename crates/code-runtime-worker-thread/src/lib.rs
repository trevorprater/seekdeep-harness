//! Rust-owned isolated TypeScript code runtime.

use std::sync::Arc;

use seekdeep_cordis::Plugin;
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

#[cfg(test)]
mod engine;
#[cfg(test)]
mod modules;
mod node;
pub mod node_assets;
mod outcome;
mod runtime;
#[cfg(test)]
mod snapshot;
#[cfg(test)]
mod watchdog;
#[cfg(test)]
mod worker_globals;

pub use runtime::{WorkerThreadCodeRuntime, WorkerThreadCodeRuntimeConfig, install};
pub use seekdeep_code_runtime::{CodeJsonString, CodeJsonValue, json::CodeJsonToken};

/// Locates and validates the compiled Node runtime shipped with this installation.
///
/// # Errors
///
/// Returns a missing-asset or filesystem error for an incomplete installation.
pub fn node_runtime_assets() -> anyhow::Result<std::path::PathBuf> {
    node::assets()
}

/// Selects the explicit, bundled, or development/npm Node executable.
///
/// # Errors
///
/// Rejects a missing or non-executable Node binary in a standalone package.
pub fn node_runtime_executable(directory: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    node::executable(directory)
}

/// Loader plugin identity.
pub const PLUGIN_NAME: &str = "code-runtime-worker-thread";
/// Worker code runtime has no service prerequisites.
pub const PLUGIN_INJECT: &[&str] = &[];

/// Builds the Loader-compatible worker-thread code runtime plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(
        PLUGIN_NAME,
        PLUGIN_INJECT.iter().copied(),
        |context, config| {
            Box::pin(async move {
                install(&context, &serde_json::from_value(config)?)?;
                Ok(())
            })
        },
    )
}

/// Registers this process-boundary implementation's explained empty invariant.
///
/// Worker protocol and built-runtime tests own the cross-process relation; the
/// package exposes no additional same-process mutable event relation.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        "seekdeep-code-runtime-worker-thread",
        InvariantInstaller::noop(),
    )
}

/// Exact combined-output JSON accounting.
pub mod output_json;
/// Worker and host outer-output ledgers.
pub mod output_ledger;
/// Evaluable-only TypeScript stripping.
pub mod typescript;
/// Flat bounded-depth lossless JSON wire format.
pub mod worker_json;

#[cfg(test)]
mod tests {
    use seekdeep_cordis::Context;

    use super::*;

    #[test]
    fn registers_package_invariant_companion() {
        let context = Context::new();
        let registry = Arc::new(
            InvariantRegistry::new(&context, &seekdeep_invariants::InvariantConfig::default())
                .unwrap(),
        );
        let _registration = register_invariant(&registry).unwrap();
        assert!(registry.is_registered("seekdeep-code-runtime-worker-thread"));
    }
}
