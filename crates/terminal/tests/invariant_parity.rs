//! Explained-empty invariant companion parity.

use seekdeep_cordis::Context;
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_terminal::invariant::register_invariant;

#[tokio::test]
async fn reserves_package_ownership_without_inventing_a_runtime_dependency() {
    let context = Context::new();
    let registry =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
    let registration = register_invariant(&registry).expect("terminal invariant");
    registration.await_ready().await.expect("ready");
    assert!(register_invariant(&registry).is_err());
}
