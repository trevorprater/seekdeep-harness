//! Behavioral parity tests for the basic-compaction configuration resolver.

use seekdeep_compaction_basic::{
    BasicCompactionConfig, CompactionTarget, ModelCompactPolicyConfig, ResolvedRetention,
    resolve_compact_spec, resolve_config, resolve_target_policy,
};

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
        model_policies: vec![ModelCompactPolicyConfig {
            provider: "p".to_owned(),
            model: "m".to_owned(),
            threshold_ratio: Some(0.5),
            ..ModelCompactPolicyConfig::default()
        }],
        ..BasicCompactionConfig::default()
    })
    .expect("config");

    let target = CompactionTarget {
        provider: "p".to_owned(),
        model: "m".to_owned(),
    };
    let policy = resolve_target_policy(&resolved, &target);
    assert!((policy.threshold_ratio - 0.5).abs() < 1e-9);
    assert_eq!(policy.retention, ResolvedRetention::Ratio(0.16));

    let spec = resolve_compact_spec(&policy, 1000).expect("spec");
    assert_eq!(spec.context_window, 1000);
    assert_eq!(spec.threshold_tokens, 500);
    assert_eq!(spec.retain_tokens, 160);
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
