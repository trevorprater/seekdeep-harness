//! Behavioral mirror of the Typert registry invariant companion.

use seekdeep_cordis::Context;
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_typert_registry::invariant::register_invariant;

#[tokio::test]
async fn explained_empty_invariant_reserves_and_releases_package_identity() {
    let context = Context::new();
    let invariants = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
    let registration = register_invariant(&invariants).unwrap();
    registration.await_ready().await.unwrap();
    assert!(register_invariant(&invariants).is_err());
    registration.dispose().await.unwrap();
    register_invariant(&invariants)
        .unwrap()
        .await_ready()
        .await
        .unwrap();
}
