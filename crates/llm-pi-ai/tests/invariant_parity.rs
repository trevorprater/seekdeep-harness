//! Explained-empty invariant registration parity tests.

use std::sync::Arc;

use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_llm_pi_ai::invariant::{INVARIANT_NAME, register_invariant};

#[tokio::test]
async fn reserves_exact_renamed_package_identity_and_releases_it() {
    assert_eq!(INVARIANT_NAME, "llm-pi-ai-invariant");
    let context = seekdeep_cordis::Context::new();
    let registry = Arc::new(InvariantRegistry::new(&context, &InvariantConfig::default()).unwrap());
    let registration = register_invariant(&registry).unwrap();
    registration.await_ready().await.unwrap();
    let duplicate = register_invariant(&registry).unwrap_err();
    assert!(
        duplicate
            .to_string()
            .contains("@deepseek-ai/seekdeep-llm-pi-ai")
    );
    registration.dispose().await.unwrap();
    register_invariant(&registry)
        .unwrap()
        .await_ready()
        .await
        .unwrap();
}
