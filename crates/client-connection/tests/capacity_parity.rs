//! Carrier body-cap invariants from the Connection plugin load boundary.

use seekdeep_client_connection::{assert_image_body_capacity, required_image_body_bytes};

#[test]
fn aggregate_base64_capacity_uses_ceiling_plus_exact_headroom() {
    assert_eq!(required_image_body_bytes(1).unwrap(), 1_048_578);
    assert_eq!(required_image_body_bytes(3).unwrap(), 1_048_580);
    let required = required_image_body_bytes(20 * 1024 * 1024).unwrap();
    assert!(assert_image_body_capacity(required, 20 * 1024 * 1024).is_ok());
    assert!(assert_image_body_capacity(required - 1, 20 * 1024 * 1024).is_err());
}
