//! Provider-profile parsing and resolution parity tests.

use seekdeep_llm::RetryPolicyMode;
use seekdeep_llm_pi_ai::{
    catalog::{CatalogIndex, PiModality},
    config::{
        DEFAULT_STREAM_IDLE_TIMEOUT_MS, assert_serviceable, materialize_config, resolve_config,
        resolve_profiles,
    },
};
use serde_json::{Value, json};

fn custom(overrides: Value) -> Value {
    let Value::Object(overrides) = overrides else {
        panic!("test overrides must be an object")
    };
    let mut route = json!({
        "api": "openai-completions",
        "baseURL": "https://acme.test",
        "models": [{"id": "m"}]
    });
    route.as_object_mut().unwrap().extend(overrides);
    json!({"acme-gateway": route})
}

#[test]
fn omitted_provider_dictionary_is_dormant_and_defaults_materialize() {
    assert!(resolve_config(&json!({})).unwrap().is_empty());
    let profiles = resolve_profiles(Some(&custom(json!({}))), &CatalogIndex::default()).unwrap();
    let profile = &profiles["acme-gateway"];
    assert_eq!(profile.display_name, "acme-gateway");
    assert_eq!(
        profile.stream_idle_timeout_ms.to_bits(),
        DEFAULT_STREAM_IDLE_TIMEOUT_MS.to_bits()
    );
    assert!(profile.api_key_env.is_none());
    assert_eq!(profile.catalog.models[0].context_window, 262_144);
    assert_eq!(profile.catalog.models[0].max_tokens, 32_768);
    assert_eq!(profile.catalog.models[0].input, vec![PiModality::Text]);
    assert_eq!(profile.retry_policy.mode(), RetryPolicyMode::Normal);
}

#[test]
fn runtime_schema_preserves_false_and_materializes_array_defaults() {
    assert_eq!(
        materialize_config(&json!({})).unwrap(),
        json!({"providers":{}})
    );
    let absent = materialize_config(&json!({
        "providers": {"acme-gateway": {
            "api":"openai-completions",
            "baseURL":"https://acme.test",
            "models":[{"id":"m"}]
        }}
    }))
    .unwrap();
    assert_eq!(
        absent["providers"]["acme-gateway"]["models"][0]["input"],
        json!([])
    );
    assert_eq!(
        absent["providers"]["acme-gateway"]["defaultInput"],
        json!(["text"])
    );
    assert!(
        absent["providers"]["acme-gateway"]["models"][0]
            .get("reasoningEfforts")
            .is_none()
    );
    let with_false = materialize_config(&json!({
        "providers": {"acme-gateway": {
            "api":"openai-completions",
            "baseURL":"https://acme.test",
            "models":[{"id":"m","reasoningEfforts":false}]
        }}
    }))
    .unwrap();
    assert_eq!(
        with_false["providers"]["acme-gateway"]["models"][0]["reasoningEfforts"],
        json!(false)
    );

    let empty_default = materialize_config(&json!({
        "providers": {"acme-gateway": {
            "api":"openai-completions",
            "baseURL":"https://acme.test",
            "defaultInput":[],
            "models":[{"id":"m"}]
        }}
    }))
    .unwrap();
    assert!(assert_serviceable(&empty_default).is_err());
}

#[test]
fn parses_and_detaches_every_request_level_profile_option() {
    let profiles = resolve_profiles(
        Some(&custom(json!({
            "apiKeyEnv": "ACME_KEY",
            "displayName": "Acme",
            "defaultContextWindow": 4096,
            "defaultMaxTokens": 256,
            "defaultInput": ["text", "image"],
            "headers": {"Authorization": "Bearer local"},
            "reasoning": "high",
            "thinkingBudgets": {"minimal":1,"low":2,"medium":3,"high":4},
            "cacheRetention": "long",
            "transport": "websocket-cached",
            "timeoutMs": 0,
            "websocketConnectTimeoutMs": 100,
            "streamIdleTimeoutMs": 0.5,
            "retryPolicy": {"mode":"always"},
            "futureOption": {"kept":true}
        }))),
        &CatalogIndex::default(),
    )
    .unwrap();
    let profile = &profiles["acme-gateway"];
    assert_eq!(profile.display_name, "Acme");
    assert_eq!(profile.api_key_env.as_ref().unwrap().as_str(), "ACME_KEY");
    assert_eq!(profile.stream_idle_timeout_ms.to_bits(), 0.5_f64.to_bits());
    assert_eq!(profile.retry_policy.mode(), RetryPolicyMode::Always);
    assert_eq!(profile.catalog.models[0].context_window, 4096);
    assert_eq!(
        profile.catalog.models[0].input,
        vec![PiModality::Text, PiModality::Image]
    );
    assert_eq!(profile.options.timeout_ms, Some(0));
    assert_eq!(profile.options.websocket_connect_timeout_ms, Some(100));
    assert_eq!(profile.options.extra["futureOption"], json!({"kept":true}));
    assert_eq!(
        profile.options.headers.as_ref().unwrap()["Authorization"],
        json!("Bearer local")
    );
}

#[test]
fn reasoning_and_modality_schema_boundaries_reject_unknown_values() {
    for providers in [
        custom(json!({"models":[{"id":"m","reasoningEfforts":{"ultra":"x"}}]})),
        custom(json!({"models":[{"id":"m","reasoningEfforts":{"high":42}}]})),
        custom(json!({"models":[{"id":"m","compat":{"thinkingFormat":"quantum"}}]})),
        custom(json!({"models":[{"id":"m","input":["audio"]}]})),
        custom(json!({"defaultInput":["text","audio"]})),
    ] {
        assert!(resolve_profiles(Some(&providers), &CatalogIndex::default()).is_err());
    }
    let empty = custom(json!({"defaultInput":[]}));
    let error = resolve_profiles(Some(&empty), &CatalogIndex::default()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("defaultInput must name at least one modality")
    );
}

#[test]
fn rejects_removed_shapes_and_invalid_route_boundaries() {
    let cases = [
        (json!([]), "providers is now a dict keyed by provider route"),
        (
            custom(json!({"provider":"other"})),
            "moved to the providers dict key",
        ),
        (
            custom(json!({"maxRetries":2})),
            "compose agent recovery with seekdeep-llm-retry",
        ),
        (custom(json!({"baseURL":""})), "empty baseURL"),
        (custom(json!({"displayName":""})), "empty displayName"),
        (
            custom(json!({"streamIdleTimeoutMs":0})),
            "streamIdleTimeoutMs must be a positive finite number",
        ),
        (
            custom(json!({"api":"quantum"})),
            "supported protocols are openai-completions, openai-responses, anthropic-messages",
        ),
        (custom(json!({"apiKeyEnv":"not-a-ref"})), "must match"),
    ];
    for (providers, expected) in cases {
        let error = resolve_profiles(Some(&providers), &CatalogIndex::default()).unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
    }

    let empty_key =
        json!({"": {"api":"openai-completions","baseURL":"https://x","models":[{"id":"m"}]}});
    assert!(
        resolve_profiles(Some(&empty_key), &CatalogIndex::default())
            .unwrap_err()
            .to_string()
            .contains("provider names must be non-empty")
    );
}

#[test]
fn capacities_and_natural_timeouts_reject_fractional_or_negative_numbers() {
    for overrides in [
        json!({"defaultContextWindow":1.5}),
        json!({"defaultContextWindow":0}),
        json!({"defaultMaxTokens":1.5}),
        json!({"models":[{"id":"m","contextWindow":1.5}]}),
        json!({"models":[{"id":"m","maxTokens":0}]}),
        json!({"timeoutMs":-1}),
        json!({"websocketConnectTimeoutMs":1.5}),
    ] {
        let providers = custom(overrides);
        assert!(resolve_profiles(Some(&providers), &CatalogIndex::default()).is_err());
    }
}
