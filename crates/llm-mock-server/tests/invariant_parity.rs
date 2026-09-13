//! Explained-empty invariant registration parity.

use seekdeep_cordis::Context;
use seekdeep_invariants::{InvariantConfig, InvariantInstaller, InvariantRegistry};
use seekdeep_llm_mock_server::invariant::register_invariant;

#[tokio::test]
async fn reserves_releases_and_replaces_the_package_invariant() {
    let context = Context::new();
    let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
    let registration = register_invariant(&registry).unwrap();
    registration.await_ready().await.unwrap();
    assert!(
        registry
            .register(
                "@seekdeep-ai/seekdeep-llm-mock-server",
                InvariantInstaller::noop(),
            )
            .is_err()
    );
    registration.dispose().await.unwrap();
    let replacement = register_invariant(&registry).unwrap();
    replacement.await_ready().await.unwrap();
    context.fiber().dispose().await.unwrap();
}
