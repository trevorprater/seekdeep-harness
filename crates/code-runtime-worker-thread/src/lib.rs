//! Pure-Rust isolated TypeScript code runtime.

use std::sync::Arc;

use seekdeep_cordis::Plugin;
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

mod engine;
mod job_executor;
mod runtime;
mod snapshot;

pub use runtime::{WorkerThreadCodeRuntime, WorkerThreadCodeRuntimeConfig, install};

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
