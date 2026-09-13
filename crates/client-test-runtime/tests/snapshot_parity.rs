//! Pure snapshot class and fingerprint parity.

#![cfg(not(target_arch = "wasm32"))]

use seekdeep_client_test_runtime::{normalize_snapshot_class_value, snapshot_markup_fingerprint};

#[test]
fn class_tokens_fold_only_the_exact_scoped_shape() {
    assert_eq!(
        normalize_snapshot_class_value(" _frame_a1b2c3 foreign _wide_name_0z "),
        "frame foreign wide_name"
    );
    assert_eq!(
        normalize_snapshot_class_value("_frame_HASH _missinghash_ _x_a-B"),
        "_frame_HASH _missinghash_ _x_a-B"
    );
}

#[test]
fn fingerprint_uses_javascript_utf16_and_fixed_lower_hex() {
    assert_eq!(
        snapshot_markup_fingerprint("<path d=\"M0 0\"></path>"),
        "66dc961a"
    );
    assert_eq!(snapshot_markup_fingerprint(""), "811c9dc5");
    assert_ne!(
        snapshot_markup_fingerprint("😀"),
        snapshot_markup_fingerprint("�")
    );
}
