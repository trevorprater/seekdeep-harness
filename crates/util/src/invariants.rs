//! Explained empty invariant companions for pure utility modules.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

/// Product-renamed package identities reserved by the utility companions.
pub const UTILITY_INVARIANT_PACKAGES: [&str; 7] = [
    "@deepseek-ai/seekdeep-atomic-write",
    "@deepseek-ai/seekdeep-brand",
    "@deepseek-ai/seekdeep-home-paths",
    "@deepseek-ai/seekdeep-launch-environment",
    "@deepseek-ai/seekdeep-native-command",
    "@deepseek-ai/seekdeep-output-retention",
    "@deepseek-ai/seekdeep-timeout",
];

/// Registers every utility package's explained empty runtime invariant.
///
/// These modules own no event stream or mutable runtime state; their value
/// algebras and filesystem/process contracts are enforced by unit tests.
/// Registration still reserves package ownership, supports filtering, and
/// provides the same companion lifecycle as packages with live checks.
///
/// # Errors
///
/// Returns malformed/duplicate package reservation or inactive-owner errors.
pub fn register_utility_invariants(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<Vec<InvariantRegistration>> {
    UTILITY_INVARIANT_PACKAGES
        .iter()
        .map(|package| registry.register(package, InvariantInstaller::noop()))
        .collect()
}

#[cfg(test)]
mod tests {
    use seekdeep_cordis::Context;
    use seekdeep_invariants::InvariantConfig;

    use super::*;

    #[tokio::test]
    async fn companions_reserve_and_release_every_package() {
        let context = Context::new();
        let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
        let registrations = register_utility_invariants(&registry).unwrap();
        for registration in &registrations {
            registration.await_ready().await.unwrap();
        }
        for package in UTILITY_INVARIANT_PACKAGES {
            assert!(registry.is_registered(package));
            assert!(
                registry
                    .register(package, InvariantInstaller::noop())
                    .is_err()
            );
        }
        for registration in registrations {
            registration.dispose().await.unwrap();
        }
        for package in UTILITY_INVARIANT_PACKAGES {
            assert!(!registry.is_registered(package));
        }
    }
}
