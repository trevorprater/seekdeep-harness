//! Resolver and registration mirror of the configuration half of `adapter.spec.ts`.

use std::collections::BTreeMap;

use seekdeep_cordis::Context;
use seekdeep_llm::{LLM, LlmProviderAuthentication, LlmRuntime, ReasoningEffortId};
use seekdeep_llm_deepseek::config::BASE_URL_ENV;
use seekdeep_llm_deepseek::{
    DEFAULT_CONTEXT_WINDOW, DEFAULT_MAX_TOKENS, DeepSeekCatalogModel, DeepSeekConfig,
    ReasoningEffort, install, resolve_adapter_options, types::ThinkingMode,
};
use seekdeep_util::launch_environment::{
    LaunchEnvironmentLayerInput, LaunchEnvironmentSource, create_launch_environment_snapshot,
};
use serde_json::json;

fn model(id: &str) -> DeepSeekCatalogModel {
    DeepSeekCatalogModel {
        id: id.to_owned(),
        name: None,
        description: None,
        context_window: None,
        max_tokens: None,
    }
}

fn environment(value: &str) -> seekdeep_util::launch_environment::LaunchEnvironmentSnapshot {
    create_launch_environment_snapshot(&[LaunchEnvironmentLayerInput {
        source: LaunchEnvironmentSource::ProjectEnv,
        path: Some("/work/.env".into()),
        values: BTreeMap::from([(BASE_URL_ENV.to_owned(), value.to_owned())]),
    }])
}

#[test]
fn resolves_defaults_environment_and_explicit_precedence() {
    let defaults = resolve_adapter_options(&DeepSeekConfig::default(), None).unwrap();
    assert_eq!(defaults.api_key_env.as_str(), "DEEPSEEK_API_KEY");
    assert_eq!(defaults.base_url, "https://api.deepseek.com");
    assert_eq!(defaults.max_tokens, DEFAULT_MAX_TOKENS);
    assert_eq!(defaults.default_context_window, DEFAULT_CONTEXT_WINDOW);
    assert_eq!(defaults.models.len(), 2);

    let layered = resolve_adapter_options(
        &DeepSeekConfig::default(),
        Some(&environment("https://project.example")),
    )
    .unwrap();
    assert_eq!(layered.base_url, "https://project.example");

    let explicit = resolve_adapter_options(
        &DeepSeekConfig {
            api_key_env: Some("PRIVATE_KEY".to_owned()),
            base_url: Some("https://gateway.internal".to_owned()),
            ..DeepSeekConfig::default()
        },
        Some(&environment("https://stale.example")),
    )
    .unwrap();
    assert_eq!(explicit.api_key_env.as_str(), "PRIVATE_KEY");
    assert_eq!(explicit.base_url, "https://gateway.internal");
}

#[test]
fn endpoint_config_preserves_the_source_base_url_acronym() {
    let config: DeepSeekConfig = serde_json::from_value(json!({
        "baseURL": "https://source-field.example"
    }))
    .unwrap();
    assert_eq!(
        config.base_url.as_deref(),
        Some("https://source-field.example")
    );
    assert_eq!(
        serde_json::to_value(&config).unwrap()["baseURL"],
        "https://source-field.example"
    );
    let misspelled: DeepSeekConfig = serde_json::from_value(json!({
        "baseUrl": "https://wrong-field.example"
    }))
    .unwrap();
    assert!(misspelled.base_url.is_none());
}

#[test]
fn validates_thinking_numeric_and_retry_bounds_before_registration() {
    for effort in [ReasoningEffort::High, ReasoningEffort::Max] {
        let error = resolve_adapter_options(
            &DeepSeekConfig {
                thinking: Some(ThinkingMode::Disabled),
                reasoning_effort: Some(effort),
                ..DeepSeekConfig::default()
            },
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("only reasoningEffort \"off\""));
    }
    assert!(
        resolve_adapter_options(
            &DeepSeekConfig {
                thinking: Some(ThinkingMode::Disabled),
                reasoning_effort: Some(ReasoningEffort::Off),
                ..DeepSeekConfig::default()
            },
            None
        )
        .is_ok()
    );
    for invalid in [0.0, 1.5, 9_007_199_254_740_992.0] {
        assert!(
            resolve_adapter_options(
                &DeepSeekConfig {
                    max_tokens: Some(invalid),
                    ..DeepSeekConfig::default()
                },
                None
            )
            .unwrap_err()
            .to_string()
            .contains("positive safe integer")
        );
    }
    for invalid in [0.0, 1.5] {
        assert!(
            resolve_adapter_options(
                &DeepSeekConfig {
                    default_context_window: Some(invalid),
                    ..DeepSeekConfig::default()
                },
                None
            )
            .unwrap_err()
            .to_string()
            .contains("defaultContextWindow")
        );
    }
    for invalid in [0.0, f64::INFINITY, 2_147_483_648.0] {
        assert!(
            resolve_adapter_options(
                &DeepSeekConfig {
                    stream_idle_timeout_ms: Some(invalid),
                    ..DeepSeekConfig::default()
                },
                None
            )
            .unwrap_err()
            .to_string()
            .contains("streamIdleTimeoutMs")
        );
    }
    assert!(
        resolve_adapter_options(
            &DeepSeekConfig {
                retry_policy: Some(json!({"mode":"normal","maxRetries":-1})),
                ..DeepSeekConfig::default()
            },
            None
        )
        .unwrap_err()
        .to_string()
        .contains("retryPolicy")
    );
}

#[test]
fn validates_and_detaches_every_catalog_entry() {
    let invalid = [
        vec![model("")],
        vec![DeepSeekCatalogModel {
            name: Some(String::new()),
            ..model("m")
        }],
        vec![DeepSeekCatalogModel {
            context_window: Some(0.0),
            ..model("m")
        }],
        vec![DeepSeekCatalogModel {
            context_window: Some(1.5),
            ..model("m")
        }],
        vec![model("m"), model("m")],
        vec![DeepSeekCatalogModel {
            max_tokens: Some(0.0),
            ..model("m")
        }],
    ];
    for models in invalid {
        assert!(
            resolve_adapter_options(
                &DeepSeekConfig {
                    models: Some(models),
                    ..DeepSeekConfig::default()
                },
                None
            )
            .is_err()
        );
    }

    let resolved = resolve_adapter_options(
        &DeepSeekConfig {
            max_tokens: Some(4_096.0),
            default_context_window: Some(256_000.0),
            models: Some(vec![
                DeepSeekCatalogModel {
                    context_window: Some(64_000.0),
                    max_tokens: Some(512.0),
                    ..model("exact")
                },
                model("inherits"),
            ]),
            ..DeepSeekConfig::default()
        },
        None,
    )
    .unwrap();
    assert_eq!(resolved.models[0].context_window, Some(64_000));
    assert_eq!(resolved.models[0].max_tokens, Some(512));
    assert_eq!(resolved.models[1].context_window, None);

    assert!(
        resolve_adapter_options(
            &DeepSeekConfig {
                models: Some(vec![]),
                ..DeepSeekConfig::default()
            },
            None
        )
        .unwrap()
        .models
        .is_empty()
    );
}

#[tokio::test]
async fn plugin_registers_directory_catalog_metadata_and_unwinds() {
    let context = Context::new();
    let runtime = LlmRuntime::install(&context).unwrap();
    let fiber = install(
        &context,
        DeepSeekConfig {
            base_url: Some("http://127.0.0.1:1".to_owned()),
            max_tokens: Some(4_096.0),
            default_context_window: Some(256_000.0),
            models: Some(vec![
                DeepSeekCatalogModel {
                    name: Some("Exact Model".to_owned()),
                    description: Some("detail".to_owned()),
                    context_window: Some(64_000.0),
                    max_tokens: Some(512.0),
                    ..model("exact")
                },
                model("inherits"),
            ]),
            ..DeepSeekConfig::default()
        },
    )
    .unwrap();
    fiber.await_settled().await.unwrap();

    assert_eq!(runtime.list_providers()[0].name, "DeepSeek");
    let directory = runtime.list_configurable_providers();
    assert_eq!(directory.len(), 1);
    assert_eq!(directory[0].provider.as_str(), "deepseek-official");
    assert_eq!(
        directory[0].authentication,
        LlmProviderAuthentication::ApiKey
    );
    let catalog = runtime.list_models("deepseek-official").await.unwrap();
    assert_eq!(catalog[0].name, "Exact Model");
    assert_eq!(catalog[0].description.as_deref(), Some("detail"));

    let exact = runtime
        .resolve_model_info("deepseek-official", "exact", None)
        .await
        .unwrap();
    assert_eq!(exact.context.unwrap().context_window, 64_000);
    assert_eq!(exact.default_max_tokens, Some(512));
    assert_eq!(
        exact.reasoning.unwrap().default_effort,
        Some(ReasoningEffortId::new("high"))
    );
    let uncatalogued = runtime
        .resolve_model_info("deepseek-official", "anything", None)
        .await
        .unwrap();
    assert_eq!(uncatalogued.context.unwrap().context_window, 256_000);
    assert_eq!(uncatalogued.default_max_tokens, Some(4_096));

    fiber.dispose().await.unwrap();
    assert!(runtime.list_providers().is_empty());
    assert!(runtime.list_configurable_providers().is_empty());
    assert!(context.get(LLM).is_some());
}

#[tokio::test]
async fn disabled_thinking_advertises_only_off() {
    let context = Context::new();
    let runtime = LlmRuntime::install(&context).unwrap();
    let fiber = install(
        &context,
        DeepSeekConfig {
            thinking: Some(ThinkingMode::Disabled),
            reasoning_effort: Some(ReasoningEffort::Off),
            ..DeepSeekConfig::default()
        },
    )
    .unwrap();
    fiber.await_settled().await.unwrap();
    let info = runtime
        .resolve_model_info("deepseek-official", "pass-through", None)
        .await
        .unwrap();
    let reasoning = info.reasoning.unwrap();
    assert_eq!(reasoning.efforts.len(), 1);
    assert_eq!(reasoning.efforts[0].id, ReasoningEffortId::new("off"));
    assert_eq!(
        reasoning.default_effort,
        Some(ReasoningEffortId::new("off"))
    );
}
