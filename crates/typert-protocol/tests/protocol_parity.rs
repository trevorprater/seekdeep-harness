//! Behavioral mirror of `packages/typert/protocol/tests/protocol.spec.ts`.

use std::sync::Arc;

use seekdeep_cordis::Context;
use seekdeep_typert_protocol::{
    RemoteFailure, RemoteInvocationMarker, RemoteMethodMarker, RemoteMethodTable, RemoteResult,
    TypertBoundaryValue, TypertGatewayBindingOptions, TypertLookupFailure, TypertRemoteService,
    bind_typert_remote, is_typert_remote_segment,
};
use serde_json::json;

#[derive(Debug)]
struct Goals {
    key: String,
    namespace: String,
}

impl Goals {
    fn new(key: &str, namespace: &str) -> Arc<Self> {
        Arc::new(Self {
            key: key.to_owned(),
            namespace: namespace.to_owned(),
        })
    }
}

impl TypertRemoteService for Goals {
    fn typert_service_key(&self) -> &str {
        &self.key
    }

    fn typert_namespace(&self) -> &str {
        &self.namespace
    }

    fn remote_methods(&self) -> Vec<RemoteMethodMarker> {
        vec![
            RemoteMethodMarker::direct("create", None).unwrap(),
            RemoteMethodMarker::scoped("scoped", "metaFixture", None).unwrap(),
        ]
    }
}

#[test]
fn binds_service_name_and_exposes_explicit_remote_declarations() {
    let goals = Goals::new("goals", "goals");
    let binding = goals.typert_remote().unwrap();
    assert!(Arc::ptr_eq(&binding.service, &goals));
    assert_eq!(binding.service_key, "goals");
    assert_eq!(binding.namespace, "goals");
    assert_eq!(
        goals.remote_methods(),
        [
            RemoteMethodMarker {
                method: "create".to_owned(),
                export_name: None,
                invocation: RemoteInvocationMarker::Direct,
            },
            RemoteMethodMarker {
                method: "scoped".to_owned(),
                export_name: None,
                invocation: RemoteInvocationMarker::Context {
                    context: "metaFixture".to_owned(),
                },
            },
        ]
    );

    let namespaced = Goals::new("internalGoals", "goals");
    let binding = namespaced.typert_remote().unwrap();
    assert_eq!(binding.service_key, "internalGoals");
    assert_eq!(binding.namespace, "goals");
}

#[test]
fn explicit_binding_is_immutable_owned_metadata_for_exact_service() {
    let goals = Goals::new("goals", "goals");
    let binding = bind_typert_remote(
        goals.clone(),
        "goals",
        TypertGatewayBindingOptions::default(),
    )
    .unwrap();
    assert!(Arc::ptr_eq(&binding.service, &goals));
    assert_eq!(binding.service_key, "goals");
    assert_eq!(binding.namespace, "goals");
}

#[test]
fn markers_are_idempotent_and_snapshots_are_detached() {
    let mut table = RemoteMethodTable::default();
    let marker = RemoteMethodMarker::direct("run", None).unwrap();
    table.mark(marker.clone()).unwrap();
    table.mark(marker).unwrap();
    let mut snapshot = table.snapshot();
    snapshot[0].method = "changed".to_owned();
    assert_eq!(table.snapshot()[0].method, "run");
}

#[test]
fn supports_explicit_export_names_without_exposing_storage() {
    let mut table = RemoteMethodTable::default();
    table
        .mark(RemoteMethodMarker::direct("run", Some("execute")).unwrap())
        .unwrap();
    table
        .mark(RemoteMethodMarker::scoped("scoped", "metaFixture", Some("inspect")).unwrap())
        .unwrap();
    assert_eq!(table.snapshot()[0].export_name.as_deref(), Some("execute"));
    assert_eq!(table.snapshot()[1].export_name.as_deref(), Some("inspect"));
}

#[test]
fn rejects_malformed_names_and_targets_by_construction() {
    for name in ["bad/name", "bad#name", "bad name", ".", "..", ""] {
        assert!(RemoteMethodMarker::direct("run", Some(name)).is_err());
        assert!(!is_typert_remote_segment(name));
    }
    assert!(RemoteMethodMarker::scoped("run", "", None).is_err());
    assert!(RemoteMethodMarker::scoped("run", "metaFixture", Some("bad/name")).is_err());
}

#[test]
fn rejects_conflicting_markers() {
    let mut table = RemoteMethodTable::default();
    table
        .mark(RemoteMethodMarker::direct("run", None).unwrap())
        .unwrap();
    let error = table
        .mark(RemoteMethodMarker::scoped("run", "metaFixture", None).unwrap())
        .unwrap_err();
    assert!(error.to_string().contains("conflicting invocation markers"));
}

#[test]
fn rejects_ambiguous_binding_names() {
    let goals = Goals::new("goals", "goals");
    assert!(bind_typert_remote(goals.clone(), "", TypertGatewayBindingOptions::default()).is_err());
    for namespace in ["api/goals", "api goals"] {
        assert!(
            bind_typert_remote(
                goals.clone(),
                "goals",
                TypertGatewayBindingOptions {
                    namespace: Some(namespace.to_owned()),
                },
            )
            .is_err()
        );
    }
}

#[test]
fn lookup_failure_preserves_adapter_payload_without_identity() {
    let failure = TypertLookupFailure::new(json!({"code": "denied"}));
    assert_eq!(failure.failure, json!({"code": "denied"}));
    assert_eq!(
        failure.to_string(),
        "Typert lookup policy rejected the requested identity"
    );
}

#[test]
fn valid_segment_grammar_matches_carrier() {
    for value in ["goals", "metaFixture", "a.b", "a-b", "a_b", "$root", "0"] {
        assert!(is_typert_remote_segment(value), "{value:?}");
    }
}

#[test]
fn remote_result_uses_exact_boolean_discriminants_and_closed_branches() {
    let success = RemoteResult::Success { value: json!(null) };
    assert_eq!(
        serde_json::to_value(&success).unwrap(),
        json!({"ok": true, "value": null})
    );
    assert_eq!(
        serde_json::from_value::<RemoteResult<serde_json::Value>>(json!({
            "ok": true, "value": null
        }))
        .unwrap(),
        success
    );
    let failure = RemoteResult::<serde_json::Value>::Failure {
        error: RemoteFailure {
            code: "denied".to_owned(),
            message: "no".to_owned(),
            details: serde_json::Map::new(),
        },
    };
    assert_eq!(
        serde_json::to_value(&failure).unwrap(),
        json!({"ok": false, "error": {"code": "denied", "message": "no", "details": {}}})
    );
    for invalid in [
        json!({"ok": false, "value": null}),
        json!({"ok": true, "error": {"code": "x", "message": "x", "details": {}}}),
        json!({"ok": "true", "value": null}),
        json!({"ok": true, "value": null, "extra": true}),
    ] {
        assert!(serde_json::from_value::<RemoteResult<serde_json::Value>>(invalid).is_err());
    }
}

#[test]
fn boundary_distinguishes_omitted_undefined_from_json_null() {
    assert!(TypertBoundaryValue::Undefined.is_undefined());
    assert_eq!(TypertBoundaryValue::Undefined.into_optional_json(), None);
    let null = TypertBoundaryValue::json(json!(null));
    assert!(!null.is_undefined());
    assert_eq!(null.as_json(), Some(&json!(null)));
    assert_eq!(null.into_optional_json(), Some(json!(null)));
}

#[test]
fn context_exists_for_provider_contracts() {
    let context = Context::new();
    let same = context.clone();
    assert_eq!(format!("{context:?}"), format!("{same:?}"));
}
