//! Behavioral mirror of the package's explained-empty invariant companion.

use seekdeep_cordis::Context;
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_llm_deepseek::invariant::register_invariant;

#[tokio::test]
async fn reserves_and_releases_package_identity() {
    let context = Context::new();
    let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
    let registration = register_invariant(&registry).unwrap();
    registration.await_ready().await.unwrap();
    assert!(registry.is_registered("@deepseek-ai/seekdeep-llm-deepseek"));
    registration.dispose().await.unwrap();
    assert!(!registry.is_registered("@deepseek-ai/seekdeep-llm-deepseek"));
}
