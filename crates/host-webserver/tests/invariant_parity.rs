//! Behavioral mirror of the Webserver invariant companion.

use seekdeep_cordis::Context;
use seekdeep_host_webserver::register_invariant;
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};

#[tokio::test]
async fn invariant_reserves_and_releases_package_identity() {
    let context = Context::new();
    let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
    let registration = register_invariant(&registry).unwrap();
    registration.await_ready().await.unwrap();
    assert!(register_invariant(&registry).is_err());
    registration.dispose().await.unwrap();
    register_invariant(&registry)
        .unwrap()
        .await_ready()
        .await
        .unwrap();
}
