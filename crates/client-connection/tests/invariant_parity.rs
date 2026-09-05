//! Behavioral mirror of the Connection explained-empty invariant companion.

use seekdeep_client_connection::install_invariant;
use seekdeep_cordis::Context;
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};

#[tokio::test]
async fn explained_empty_invariant_reserves_and_releases_package_identity() {
    let context = Context::new();
    let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
    let registration = install_invariant(&registry).unwrap();
    registration.await_ready().await.unwrap();
    assert!(install_invariant(&registry).is_err());
    registration.dispose().await.unwrap();
    install_invariant(&registry)
        .unwrap()
        .await_ready()
        .await
        .unwrap();
}
