//! Behavioral parity tests for the basic-compaction configuration resolver.

use seekdeep_compaction_basic::{
    BasicCompactionConfig, CompactionTarget, ModelCompactPolicyConfig, ResolvedRetention,
    parse_config_value, resolve_compact_spec, resolve_config, resolve_target_policy,
};
use serde_json::{Value, json};

fn resolved_value(value: &Value) -> seekdeep_compaction_basic::ResolvedConfig {
    resolve_config(&parse_config_value(value).unwrap()).unwrap()
}

#[test]
fn resolves_defaults() {
    let resolved = resolve_config(&BasicCompactionConfig::default()).expect("defaults");
    assert!((resolved.threshold_ratio - 0.8).abs() < 1e-9);
    assert_eq!(resolved.retention, ResolvedRetention::Ratio(0.16));
    assert_eq!(resolved.max_tokens, 8192);
    assert_eq!(resolved.compaction_retries, 1);
    assert_eq!(resolved.max_overflow_retries, 1);
    assert!(resolved.auto);
    assert!(resolved.model_policies.is_empty());
    assert_eq!(resolved.summarization_provider, "");
    assert_eq!(resolved.summarization_model, "");
}

#[test]
fn resolves_threshold_and_retention_overrides_independently() {
    let threshold = resolved_value(&json!({"thresholdRatio": 0.5}));
    assert!((threshold.threshold_ratio - 0.5).abs() < 1e-9);
    assert_eq!(threshold.retention, ResolvedRetention::Ratio(0.16));

    let retention = resolved_value(&json!({"retainTokens": 70}));
    assert!((retention.threshold_ratio - 0.8).abs() < 1e-9);
    assert_eq!(retention.retention, ResolvedRetention::Tokens(70));
}

#[test]
fn rejects_invalid_ratio_and_conflicting_retention() {
    for config in [
        BasicCompactionConfig {
            threshold_ratio: Some(1.5),
            ..BasicCompactionConfig::default()
        },
        BasicCompactionConfig {
            retain_ratio: Some(0.0),
            ..BasicCompactionConfig::default()
        },
        BasicCompactionConfig {
            retain_ratio: Some(0.5),
            retain_tokens: Some(100),
            ..BasicCompactionConfig::default()
        },
        BasicCompactionConfig {
            retain_ratio: Some(0.9),
            ..BasicCompactionConfig::default()
        },
    ] {
        assert!(resolve_config(&config).is_err());
    }
}

#[test]
fn rejects_duplicate_model_policies() {
    let config = BasicCompactionConfig {
        model_policies: vec![
            ModelCompactPolicyConfig {
                provider: "p".to_owned(),
                model: "m".to_owned(),
                ..ModelCompactPolicyConfig::default()
            },
            ModelCompactPolicyConfig {
                provider: "p".to_owned(),
                model: "m".to_owned(),
                ..ModelCompactPolicyConfig::default()
            },
        ],
        ..BasicCompactionConfig::default()
    };
    let error = resolve_config(&config).expect_err("duplicate must fail");
    assert!(
        error.to_string().contains("duplicate model policy for p/m"),
        "{error}"
    );
}

#[test]
fn merges_target_override_and_scales_budgets() {
    let resolved = resolve_config(&BasicCompactionConfig {
        threshold_ratio: Some(0.8),
        retain_ratio: Some(0.1),
        model_policies: vec![ModelCompactPolicyConfig {
            provider: "small-provider".to_owned(),
            model: "shared-id".to_owned(),
            threshold_ratio: Some(0.5),
            retain_tokens: Some(120),
            ..ModelCompactPolicyConfig::default()
        }],
        ..BasicCompactionConfig::default()
    })
    .expect("config");

    let target = CompactionTarget {
        provider: "small-provider".to_owned(),
        model: "shared-id".to_owned(),
    };
    let policy = resolve_target_policy(&resolved, &target);
    assert!((policy.threshold_ratio - 0.5).abs() < 1e-9);
    assert_eq!(policy.retention, ResolvedRetention::Tokens(120));

    let spec = resolve_compact_spec(&policy, 1000).expect("spec");
    assert_eq!(spec.context_window, 1000);
    assert_eq!(spec.threshold_tokens, 500);
    assert_eq!(spec.retain_tokens, 120);

    let other = resolve_target_policy(
        &resolved,
        &CompactionTarget {
            provider: "large-provider".to_owned(),
            model: "shared-id".to_owned(),
        },
    );
    let other = resolve_compact_spec(&other, 2000).unwrap();
    assert_eq!(other.threshold_tokens, 1600);
    assert_eq!(other.retain_tokens, 200);

    let ratio = resolved_value(&json!({
        "retainTokens": 200,
        "modelPolicies": [{
            "provider": "ratio-provider",
            "model": "ratio-model",
            "thresholdRatio": 0.6,
            "retainRatio": 0.2,
            "summarizationProvider": "summary-provider",
            "summarizationModel": "summary-model",
            "maxTokens": 512,
            "compactionRetries": 2,
            "maxOverflowRetries": 3
        }]
    }));
    let ratio = resolve_target_policy(
        &ratio,
        &CompactionTarget {
            provider: "ratio-provider".to_owned(),
            model: "ratio-model".to_owned(),
        },
    );
    let ratio = resolve_compact_spec(&ratio, 2000).unwrap();
    assert_eq!(ratio.threshold_tokens, 1200);
    assert_eq!(ratio.retain_tokens, 400);
    assert_eq!(ratio.summarization_provider, "summary-provider");
    assert_eq!(ratio.summarization_model, "summary-model");
    assert_eq!(ratio.max_tokens, 512);
    assert_eq!(ratio.compaction_retries, 2);
    assert_eq!(ratio.max_overflow_retries, 3);
}

#[test]
fn inherits_clears_and_replaces_summarization_target_as_a_pair() {
    let config = resolved_value(&json!({
        "summarizationProvider": "default-provider",
        "summarizationModel": "default-model",
        "modelPolicies": [
            {"provider": "inherit-provider", "model": "test-model"},
            {
                "provider": "clear-provider",
                "model": "test-model",
                "summarizationProvider": "",
                "summarizationModel": ""
            },
            {
                "provider": "replace-provider",
                "model": "test-model",
                "summarizationProvider": "replacement-provider",
                "summarizationModel": "replacement-model"
            }
        ]
    }));
    let route = |provider: &str| CompactionTarget {
        provider: provider.to_owned(),
        model: "test-model".to_owned(),
    };
    let inherited = resolve_target_policy(&config, &route("inherit-provider"));
    assert_eq!(inherited.summarization_provider, "default-provider");
    assert_eq!(inherited.summarization_model, "default-model");
    let cleared = resolve_target_policy(&config, &route("clear-provider"));
    assert_eq!(cleared.summarization_provider, "");
    assert_eq!(cleared.summarization_model, "");
    let replaced = resolve_target_policy(&config, &route("replace-provider"));
    assert_eq!(replaced.summarization_provider, "replacement-provider");
    assert_eq!(replaced.summarization_model, "replacement-model");
}

fn bad_scalar_values() -> Vec<(Value, &'static str)> {
    vec![
        (json!({"maxTokens": 0}), "maxTokens"),
        (json!({"compactionRetries": -1}), "compactionRetries"),
        (json!({"maxOverflowRetries": -1}), "maxOverflowRetries"),
        (json!({"auto": "yes"}), "auto must be a boolean"),
        (
            json!({"summarizationProvider": 1}),
            "summarizationProvider must be a string",
        ),
        (
            json!({"summarizationModel": 1}),
            "summarizationModel must be a string",
        ),
        (
            json!({"summarizationProvider": "test-model"}),
            "must be set together",
        ),
        (
            json!({"summarizationModel": "test-model"}),
            "must be set together",
        ),
        (json!({"summarizationProvider": ""}), "must be set together"),
        (json!({"summarizationModel": ""}), "must be set together"),
        (json!({"thresholdRatio": 0}), "number in (0, 1]"),
        (json!({"thresholdRatio": 1.1}), "number in (0, 1]"),
        (json!({"retainRatio": 0.9}), "retainRatio (0.9)"),
        (json!({"thresholdRatio": 0.1}), "retainRatio (0.16)"),
        (json!({"retainTokens": -1}), "non-negative integer"),
        (
            json!({"retainRatio": 0.2, "retainTokens": 100}),
            "mutually exclusive",
        ),
    ]
}

fn bad_model_policy_values() -> Vec<(Value, &'static str)> {
    vec![
        (
            json!({"modelPolicies": {}}),
            "modelPolicies must be an array",
        ),
        (
            json!({"modelPolicies": [1]}),
            "modelPolicies[0] must be an object",
        ),
        (
            json!({"modelPolicies": [null]}),
            "modelPolicies[0] must be an object",
        ),
        (
            json!({"modelPolicies": [[]]}),
            "modelPolicies[0] must be an object",
        ),
        (
            json!({"modelPolicies": [{"provider": 1, "model": "test-model"}]}),
            "provider must be a non-empty string",
        ),
        (
            json!({"modelPolicies": [{"provider": "", "model": "test-model"}]}),
            "provider must be a non-empty string",
        ),
        (
            json!({"modelPolicies": [{"provider": "test-model", "model": 1}]}),
            "model must be a non-empty string",
        ),
        (
            json!({"modelPolicies": [{"provider": "test-model", "model": ""}]}),
            "model must be a non-empty string",
        ),
        (
            json!({"modelPolicies": [{"provider": "test-model", "model": "test-model", "summarizationProvider": 1}]}),
            "summarizationProvider must be a string",
        ),
        (
            json!({
                "summarizationProvider": "default-provider",
                "summarizationModel": "default-model",
                "modelPolicies": [{"provider": "test-model", "model": "test-model", "summarizationModel": ""}]
            }),
            "must be set together",
        ),
        (
            json!({
                "summarizationProvider": "default-provider",
                "summarizationModel": "default-model",
                "modelPolicies": [{"provider": "test-model", "model": "test-model", "summarizationProvider": ""}]
            }),
            "must be set together",
        ),
        (
            json!({"modelPolicies": [{"provider": "test-model", "model": "test-model", "retainRatio": 0.2, "retainTokens": 100}]}),
            "mutually exclusive",
        ),
        (
            json!({"modelPolicies": [{"provider": "test-model", "model": "test-model", "thresholdRatio": 0.1}]}),
            "retainRatio (0.16)",
        ),
        (
            json!({"modelPolicies": [{"provider": "test-model", "model": "test-model", "retainRatio": 0.9}]}),
            "retainRatio (0.9)",
        ),
        (
            json!({"modelPolicies": [
                {"provider": "test-model", "model": "test-model"},
                {"provider": "test-model", "model": "test-model"}
            ]}),
            "duplicate model policy",
        ),
        (
            json!({"models": {"test-model": {"retainTokens": 10}}}),
            "unknown key \"models\"",
        ),
        (
            json!({"thresholdRato": 0.5}),
            "unknown key \"thresholdRato\"",
        ),
    ]
}

#[test]
fn validates_every_untrusted_value_and_pressure_policy_invariant() {
    for (value, expected) in bad_scalar_values()
        .into_iter()
        .chain(bad_model_policy_values())
    {
        let error = parse_config_value(&value).unwrap_err().to_string();
        assert!(error.contains(expected), "{value}: {error}");
    }
    let pressure = resolved_value(&json!({"thresholdRatio": 0.5, "retainTokens": 500}));
    let pressure = resolve_target_policy(
        &pressure,
        &CompactionTarget {
            provider: "test-model".to_owned(),
            model: "test-model".to_owned(),
        },
    );
    assert!(
        resolve_compact_spec(&pressure, 1000)
            .unwrap_err()
            .to_string()
            .contains("less than threshold")
    );
    assert!(
        resolve_compact_spec(&pressure, 0)
            .unwrap_err()
            .to_string()
            .contains("positive integer")
    );
}

#[test]
fn rejects_retention_not_below_threshold() {
    let resolved = resolve_config(&BasicCompactionConfig {
        retain_tokens: Some(500),
        ..BasicCompactionConfig::default()
    })
    .expect("config");
    let target = CompactionTarget {
        provider: "p".to_owned(),
        model: "m".to_owned(),
    };
    let policy = resolve_target_policy(&resolved, &target);
    let error = resolve_compact_spec(&policy, 600).expect_err("must reject");
    assert!(
        error
            .to_string()
            .contains("retainTokens (500) must be less than threshold tokens 480"),
        "{error}"
    );
}
