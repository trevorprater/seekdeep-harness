//! Behavioral mirror of the Gateway invariant companion.

use seekdeep_api_gateway::invariant::register_invariant;
use seekdeep_cordis::Context;
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};

#[tokio::test]
async fn explained_empty_invariant_reserves_and_releases_package_identity() {
    let context = Context::new();
    let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
    let registration = register_invariant(&registry).unwrap();
    registration.await_ready().await.unwrap();
    assert!(register_invariant(&registry).is_err());
    registration.dispose().await.unwrap();
    let replacement = register_invariant(&registry).unwrap();
    replacement.await_ready().await.unwrap();
}
