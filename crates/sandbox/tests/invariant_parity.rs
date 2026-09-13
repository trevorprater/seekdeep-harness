//! Explained-empty sandbox invariant companion parity.

use seekdeep_cordis::Context;
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};

#[tokio::test]
async fn companion_reserves_the_package_with_an_explained_empty_installer() {
    let context = Context::new();
    let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
    let registration = seekdeep_sandbox::invariant::register(&registry).unwrap();
    registration.await_ready().await.unwrap();
    assert!(seekdeep_sandbox::invariant::register(&registry).is_err());
}
