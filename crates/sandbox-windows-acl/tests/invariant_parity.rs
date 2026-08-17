//! Invariant companion parity.

use seekdeep_cordis::Context;
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};

#[tokio::test]
async fn companion_reserves_package_without_a_synthetic_lifecycle_stream() {
    let context = Context::new();
    let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
    let registration = seekdeep_sandbox_windows_acl::invariant::register(&registry).unwrap();
    registration.await_ready().await.unwrap();
    assert!(seekdeep_sandbox_windows_acl::invariant::register(&registry).is_err());
}
