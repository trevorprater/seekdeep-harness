//! The five embedded Host artifacts load, every boundary projects, and the message-feedback
//! put boundary behaves like the source's generated Zod schema.

use std::path::PathBuf;

use seekdeep_typert_host_artifact::{contribution, parse};
use seekdeep_typert_protocol::{TypertBoundaryValue, TypertCodec};
use serde_json::json;

const CRATES: [(&str, &str); 5] = [
    ("commands", "@seekdeep-ai/seekdeep-commands"),
    ("goal", "@seekdeep-ai/seekdeep-goal"),
    (
        "cordis-host-runner",
        "@seekdeep-ai/seekdeep-cordis-host-runner",
    ),
    (
        "host-plugin-inventory",
        "@seekdeep-ai/seekdeep-host-plugin-inventory",
    ),
    ("message-feedback", "@seekdeep-ai/seekdeep-message-feedback"),
];

fn artifact(krate: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(krate)
        .join("typert.host.json");
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[test]
fn every_embedded_artifact_builds_a_strict_contribution() {
    for (krate, package) in CRATES {
        let json = artifact(krate);
        let parsed = parse(&json).unwrap();
        assert_eq!(parsed.package, package);
        let built = contribution(&json).unwrap();
        assert_eq!(built.package, package);
        assert!(!built.invocations.is_empty(), "{package} has invocations");
        for invocation in &built.invocations {
            assert!(invocation.id.starts_with(package), "{}", invocation.id);
            assert!(
                matches!(invocation.result, TypertCodec::Strict { .. }),
                "{}",
                invocation.id
            );
            for parameter in &invocation.parameters {
                assert!(
                    matches!(parameter.codec, TypertCodec::Strict { .. }),
                    "{}",
                    invocation.id
                );
                let TypertCodec::Strict { schema, .. } = &parameter.codec else {
                    unreachable!()
                };
                schema.to_json_schema().unwrap();
            }
        }
    }
}

fn put_request_schema() -> std::sync::Arc<dyn seekdeep_typert_protocol::TypertSchema> {
    let built = contribution(&artifact("message-feedback")).unwrap();
    let put = built
        .invocations
        .iter()
        .find(|invocation| invocation.method == "put")
        .unwrap();
    let TypertCodec::Strict {
        type_symbol,
        schema,
    } = &put.parameters[0].codec
    else {
        panic!("put request must be strict");
    };
    assert_eq!(
        type_symbol,
        "@seekdeep-ai/seekdeep-message-feedback/types#MessageFeedbackPutRequest"
    );
    schema.clone()
}

#[test]
fn message_feedback_put_request_accepts_the_source_shape_and_strips_unknown_keys() {
    let schema = put_request_schema();
    let parsed = schema
        .parse(TypertBoundaryValue::json(json!({
            "sessionId": "s", "messageId": "m", "rating": "positive", "note": "Useful",
            "ifVersion": null, "extra": true
        })))
        .unwrap();
    assert_eq!(
        parsed.into_optional_json().unwrap(),
        json!({"sessionId": "s", "messageId": "m", "rating": "positive", "note": "Useful", "ifVersion": null})
    );
    // Optional note may be absent; ifVersion may be a version string.
    schema
        .parse(TypertBoundaryValue::json(json!({
            "sessionId": "s", "messageId": "m", "rating": "negative", "ifVersion": "v1"
        })))
        .unwrap();
}

#[test]
fn message_feedback_put_request_rejects_an_unknown_rating_and_missing_fields() {
    let schema = put_request_schema();
    let rejected = schema.parse(TypertBoundaryValue::json(json!({
        "sessionId": "s", "messageId": "m", "rating": "invalid-rating", "ifVersion": null
    })));
    assert!(rejected.is_err());
    assert!(
        schema
            .parse(TypertBoundaryValue::json(
                json!({"sessionId": "s", "rating": "positive", "ifVersion": null})
            ))
            .is_err()
    );
    assert!(schema.parse(TypertBoundaryValue::Undefined).is_err());
    assert!(
        schema
            .parse(TypertBoundaryValue::json(json!("not an object")))
            .is_err()
    );
}

#[test]
fn message_feedback_put_result_union_accepts_each_variant() {
    let built = contribution(&artifact("message-feedback")).unwrap();
    let put = built
        .invocations
        .iter()
        .find(|invocation| invocation.method == "put")
        .unwrap();
    let TypertCodec::Strict { schema, .. } = &put.result else {
        panic!("strict result")
    };
    let ok = json!({"ok": true, "value": {"messageId": "m", "rating": "positive", "version": "v", "createdAt": 1, "updatedAt": 2}});
    assert_eq!(
        schema
            .parse(TypertBoundaryValue::json(ok.clone()))
            .unwrap()
            .into_optional_json()
            .unwrap(),
        ok
    );
    let conflict = json!({"ok": false, "error": {"code": "version-conflict", "current": null}});
    assert_eq!(
        schema
            .parse(TypertBoundaryValue::json(conflict.clone()))
            .unwrap()
            .into_optional_json()
            .unwrap(),
        conflict
    );
    assert!(
        schema
            .parse(TypertBoundaryValue::json(json!({"ok": true, "value": {}})))
            .is_err()
    );
}
