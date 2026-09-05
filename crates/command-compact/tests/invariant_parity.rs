//! Behavioral mirror of packages/compaction/command-compact/tests/invariant.spec.ts.

use seekdeep_command_compact::invariant::{INJECT, NAME, PACKAGE_NAME, register_invariant};
use seekdeep_cordis::Context;
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};

#[tokio::test]
async fn registers_the_package_owned_noop_installer() {
    assert_eq!(NAME, "command-compact-invariant");
    assert_eq!(INJECT, &["invariants"]);
    assert_eq!(PACKAGE_NAME, "@deepseek-ai/seekdeep-command-compact");

    let context = Context::new();
    let registry =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("registry");
    let registration = register_invariant(&registry).expect("register");
    // The installer is a no-op: joining ready completes without any validation.
    registration.await_ready().await.expect("ready");
    // A duplicate registration is rejected while the package identity is reserved.
    assert!(register_invariant(&registry).is_err());
    registration.dispose().await.expect("dispose");
    register_invariant(&registry).expect("replacement");
}
