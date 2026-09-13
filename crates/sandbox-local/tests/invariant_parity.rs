//! Invariant companion parity.

use seekdeep_cordis::Context;
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};

#[tokio::test]
async fn companion_reserves_package_ownership_without_a_synthetic_stream() {
    let context = Context::new();
    let invariants = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
    let registration = seekdeep_sandbox_local::invariant::register(&invariants).unwrap();
    registration.await_ready().await.unwrap();
    assert!(seekdeep_sandbox_local::invariant::register(&invariants).is_err());
}
