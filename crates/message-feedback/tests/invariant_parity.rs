//! Behavioral mirror of packages/feedback/message-feedback/tests/invariant.spec.ts.

use seekdeep_cordis::Context;
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_message_feedback::register_invariant;

#[tokio::test]
async fn removes_its_registry_contribution_when_its_fiber_is_disposed() {
    let context = Context::new();
    let registry =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("registry");
    let registration = register_invariant(&registry).expect("register");

    // Duplicate registration is rejected while the companion owns the package identity.
    let error = register_invariant(&registry).expect_err("duplicate");
    assert!(error.to_string().contains("already registered"));

    // Dispose releases the package identity so a reload can re-register.
    registration.dispose().await.expect("dispose");
    register_invariant(&registry).expect("replacement");
}
