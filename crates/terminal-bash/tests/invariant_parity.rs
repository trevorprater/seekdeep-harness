//! Explained-empty invariant companion parity.

use seekdeep_cordis::Context;
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_terminal_bash::invariant::register_invariant;

#[tokio::test]
async fn reserves_package_ownership_without_a_synthetic_lifecycle_stream() {
    let context = Context::new();
    let registry =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
    let registration = register_invariant(&registry).expect("registration");
    registration.await_ready().await.expect("ready");
}
