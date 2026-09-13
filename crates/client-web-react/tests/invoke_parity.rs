//! Pending transition and invariant identity parity.

use seekdeep_client_web_react::{INVARIANT_NAME, InvokeCounter};

#[test]
fn concurrent_pending_notifies_only_at_zero_boundaries() {
    let mut counter = InvokeCounter::default();
    assert!(!counter.pending());
    assert!(counter.begin());
    assert!(!counter.begin());
    assert_eq!(counter.inflight(), 2);
    assert!(!counter.finish());
    assert!(counter.pending());
    assert!(counter.finish());
    assert!(!counter.pending());
    assert!(!counter.finish());
}

#[test]
fn invariant_identity_is_exact() {
    assert_eq!(INVARIANT_NAME, "client-web-react-invariant");
}
