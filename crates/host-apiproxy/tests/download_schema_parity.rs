//! Executable parity specification for the Host-only download query.

use seekdeep_host_apiproxy::api::downloads::SessionLogQuery;
use serde_json::json;

#[test]
fn session_log_query_accepts_only_exact_boolean_strings_and_omits_false_after_transform() {
    let descendants = SessionLogQuery::parse(
        &json!({"sessionId": "s", "includeDescendants": "true", "ignored": "x"}),
    )
    .unwrap();
    assert_eq!(descendants.include_descendants, Some(true));
    assert_eq!(
        serde_json::to_value(descendants).unwrap(),
        json!({"sessionId": "s", "includeDescendants": true})
    );
    let root_only =
        SessionLogQuery::parse(&json!({"sessionId": "s", "includeDescendants": "false"})).unwrap();
    assert_eq!(root_only.include_descendants, None);
    assert_eq!(
        serde_json::to_value(root_only).unwrap(),
        json!({"sessionId": "s"})
    );
    for invalid in [
        json!({"sessionId": ""}),
        json!({"sessionId": "s", "includeDescendants": true}),
        json!({"sessionId": "s", "includeDescendants": "yes"}),
    ] {
        assert!(
            SessionLogQuery::parse(&invalid).is_err(),
            "accepted {invalid}"
        );
    }
}
