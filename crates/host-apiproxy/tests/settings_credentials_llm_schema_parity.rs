//! Executable parity specifications for configuration-plane schemas.

use seekdeep_host_apiproxy::api::{
    credentials::{
        CREDENTIAL_DESCRIBE_MAX_REFS, CredentialsDescribeRequest, CredentialsDescribeValue,
        CredentialsSetRequest, CredentialsUnsetRequest,
    },
    llm::{LlmDiscoverModelsRequest, LlmDiscoverModelsValue, LlmModelsValue, LlmProvidersValue},
    settings::{
        SettingsDescribeValue, SettingsMutateRequest, SettingsNamespaceView,
        SettingsOpenDocumentValue, SettingsReplaceRequest, SettingsUpdateRequest,
    },
};
use serde_json::json;

fn namespace() -> serde_json::Value {
    json!({
        "ns": "llm-deepseek",
        "schema": {"type": "object"},
        "value": {"model": "deepseek-v4"},
        "base": null,
        "user": {"model": "deepseek-v4"},
        "applies": "live",
        "secrets": [{"path": ["apiKey"], "set": true}],
        "revision": 2
    })
}

#[test]
fn settings_namespace_views_preserve_explicit_unknown_nulls_and_redacted_secret_shape() {
    let parsed = SettingsNamespaceView::parse(&namespace()).unwrap();
    assert_eq!(parsed.ns, "llm-deepseek");
    assert_eq!(parsed.base, Some(serde_json::Value::Null));
    assert_eq!(parsed.secrets[0].path, ["apiKey"]);
    let encoded = serde_json::to_value(parsed).unwrap();
    assert!(encoded.get("base").unwrap().is_null());
    assert!(
        SettingsNamespaceView::parse(&json!({
            "ns": "", "schema": {}, "value": {}, "applies": "live", "secrets": [], "revision": 0
        }))
        .is_err()
    );
    assert!(
        SettingsNamespaceView::parse(&json!({
            "ns": "x", "schema": {}, "value": {}, "applies": "future", "secrets": [], "revision": 0
        }))
        .is_err()
    );
    assert!(
        SettingsNamespaceView::parse(&json!({
            "ns": "x", "schema": {}, "value": {}, "applies": "restart",
            "secrets": [{"path": [1], "set": true}], "revision": 0
        }))
        .is_err()
    );
    assert!(
        SettingsDescribeValue::parse(&json!({
            "writable": true, "hasDocument": false, "namespaces": [namespace()]
        }))
        .is_ok()
    );
    assert!(SettingsOpenDocumentValue::parse(&json!({"opened": true})).is_ok());
    assert!(SettingsOpenDocumentValue::parse(&json!({"opened": false})).is_err());
}

#[test]
fn settings_write_requests_keep_records_path_operations_and_revision_numbers_exact() {
    let update = SettingsUpdateRequest::parse(&json!({
        "ns": "llm-deepseek", "patch": {"nested": {"x": 1}}, "expectedRevision": 2.5
    }))
    .unwrap();
    assert_eq!(update.patch["nested"]["x"], 1);
    assert_eq!(update.expected_revision, Some(2.5));
    assert!(SettingsUpdateRequest::parse(&json!({"ns": "x", "patch": []})).is_err());
    assert!(
        SettingsUpdateRequest::parse(&json!({"ns": "x", "patch": {}, "expectedRevision": null}))
            .is_err()
    );
    assert!(SettingsReplaceRequest::parse(&json!({"ns": "x", "section": {}})).is_ok());
    let mutation = SettingsMutateRequest::parse(&json!({
        "ns": "x",
        "ops": [
            {"op": "set", "path": [], "value": null, "ignored": true},
            {"op": "unset", "path": ["apiKey"], "value": "ignored"}
        ],
        "expectedRevision": 3
    }))
    .unwrap();
    let encoded = serde_json::to_value(mutation).unwrap();
    assert!(encoded["ops"][0]["value"].is_null());
    assert!(encoded["ops"][0].get("ignored").is_none());
    assert!(encoded["ops"][1].get("value").is_none());
    assert!(
        SettingsMutateRequest::parse(&json!({"ns": "x", "ops": [{"op": "set", "path": []}]}))
            .is_err()
    );
    assert!(
        SettingsMutateRequest::parse(&json!({"ns": "x", "ops": [{"op": "future", "path": []}]}))
            .is_err()
    );
}

#[test]
fn credential_contract_is_value_free_on_reads_and_portable_name_checked_on_writes() {
    assert!(
        CredentialsDescribeRequest::parse(&json!({"refs": ["OPENAI_API_KEY", "_PRIVATE1"]}))
            .is_ok()
    );
    for reference in ["", "1BAD", "BAD-DASH", "BAD.DOT", "WITH SPACE"] {
        assert!(
            CredentialsDescribeRequest::parse(&json!({"refs": [reference]})).is_err(),
            "accepted {reference:?}"
        );
    }
    assert!(
        CredentialsDescribeRequest::parse(
            &json!({"refs": vec!["KEY"; CREDENTIAL_DESCRIBE_MAX_REFS + 1]})
        )
        .is_err()
    );
    let described = CredentialsDescribeValue::parse(&json!({
        "credentials": {
            "OPENAI_API_KEY": {"configured": true, "source": "env", "writable": false, "value": "stripped"}
        }
    }))
    .unwrap();
    let encoded = serde_json::to_value(described).unwrap();
    assert!(
        encoded["credentials"]["OPENAI_API_KEY"]
            .get("value")
            .is_none()
    );
    assert!(
        CredentialsSetRequest::parse(&json!({"ref": "OPENAI_API_KEY", "value": "secret"})).is_ok()
    );
    assert!(CredentialsSetRequest::parse(&json!({"ref": "OPENAI_API_KEY", "value": ""})).is_err());
    assert!(CredentialsUnsetRequest::parse(&json!({"ref": "OPENAI_API_KEY"})).is_ok());
}

#[test]
fn llm_provider_topology_keeps_authentication_closed_and_route_kind_fields_exact() {
    let value = LlmProvidersValue::parse(&json!({
        "providers": [
            {
                "provider": "openai", "displayName": "OpenAI", "settingsNs": "llm-openai",
                "settingsPath": ["profiles", "default"], "authentication": "api-key",
                "active": true, "declared": true
            },
            {
                "provider": "codex", "displayName": "Codex", "settingsNs": "",
                "settingsPath": [], "authentication": "codex-oauth", "active": false
            }
        ]
    }))
    .unwrap();
    assert_eq!(value.providers.len(), 2);
    assert!(
        LlmProvidersValue::parse(&json!({"providers": [{
            "provider": "p", "displayName": "P", "settingsNs": "", "settingsPath": [],
            "authentication": "password", "active": true
        }]}))
        .is_err()
    );
    assert!(
        LlmProvidersValue::parse(&json!({"providers": [{
            "provider": "p", "displayName": "P", "settingsNs": "", "settingsPath": [],
            "active": true, "declared": null
        }]}))
        .is_err()
    );
    assert!(
        LlmModelsValue::parse(&json!({
            "groups": [{"id": "p", "name": "P", "models": [{"id": "m", "name": "M"}]}],
            "failures": [{"id": "bad", "name": "Bad", "message": "offline"}]
        }))
        .is_ok()
    );
}

#[test]
fn draft_model_discovery_requires_nonempty_secrets_and_positive_model_limits() {
    let request = LlmDiscoverModelsRequest::parse(&json!({
        "settingsNs": "llm-openai",
        "provider": "openai",
        "baseURL": "https://example.test/v1",
        "api": "openai-responses",
        "apiKey": "secret"
    }))
    .unwrap();
    let encoded = serde_json::to_value(request).unwrap();
    assert_eq!(encoded["baseURL"], "https://example.test/v1");
    assert!(encoded.get("baseUrl").is_none());
    for (field, value) in [
        ("settingsNs", json!("")),
        ("provider", json!("")),
        ("baseURL", json!(null)),
        ("apiKey", json!("")),
    ] {
        let mut candidate = json!({"settingsNs": "llm-openai"});
        candidate
            .as_object_mut()
            .unwrap()
            .insert(field.to_owned(), value);
        assert!(
            LlmDiscoverModelsRequest::parse(&candidate).is_err(),
            "accepted {candidate}"
        );
    }
    let models = LlmDiscoverModelsValue::parse(&json!({
        "models": [{"id": "m", "name": "Model", "contextWindow": 128_000, "maxTokens": 8192}]
    }))
    .unwrap();
    assert_eq!(models.models[0].context_window, Some(128_000));
    assert!(
        LlmDiscoverModelsValue::parse(&json!({"models": [{"id": "m", "contextWindow": 0}]}))
            .is_err()
    );
    assert!(LlmDiscoverModelsValue::parse(&json!({"models": [{"id": "", "name": "M"}]})).is_err());
}
