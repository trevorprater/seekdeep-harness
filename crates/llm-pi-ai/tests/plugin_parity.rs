//! Package mount, directory, registration, discovery, and disposal parity tests.

use seekdeep_cordis::Context;
use seekdeep_llm::{LlmError, LlmModelDiscoveryRequest, LlmRuntime, ProviderId};
use seekdeep_llm_pi_ai::plugin::{NAME, plugin};
use serde_json::json;

fn error_code(error: &anyhow::Error) -> Option<&str> {
    error.downcast_ref::<LlmError>().map(LlmError::code)
}

#[tokio::test]
async fn dormant_mount_offers_only_currently_executable_catalog_routes() {
    let context = Context::new();
    let runtime = LlmRuntime::install(&context).unwrap();
    let fiber = context.plugin(plugin(), json!({})).unwrap();
    fiber.await_settled().await.unwrap();
    assert!(runtime.list_providers().is_empty());
    let directory = runtime
        .list_configurable_providers()
        .into_iter()
        .map(|entry| entry.provider.to_string())
        .collect::<Vec<_>>();
    for provider in ["deepseek", "openai", "anthropic", "openrouter"] {
        assert!(directory.contains(&provider.to_owned()), "{provider}");
    }
    assert_eq!(
        runtime
            .list_configurable_providers()
            .iter()
            .find(|entry| entry.provider.as_str() == "openai-codex")
            .unwrap()
            .authentication,
        seekdeep_llm::LlmProviderAuthentication::CodexOauth
    );
    assert!(directory.contains(&"azure-openai-responses".to_owned()));
    assert!(directory.contains(&"amazon-bedrock".to_owned()));
    assert!(directory.contains(&"google-vertex".to_owned()));
    let discovered = runtime
        .discover_models(
            NAME,
            LlmModelDiscoveryRequest {
                provider: Some(ProviderId::new("deepseek")),
                ..LlmModelDiscoveryRequest::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(discovered.len(), 2);
    let wrong_namespace = runtime
        .discover_models(
            "llm-deepseek",
            LlmModelDiscoveryRequest {
                base_url: Some("https://api.deepseek.com".to_owned()),
                ..LlmModelDiscoveryRequest::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error_code(&wrong_namespace), Some("NO_DISCOVERY"));
    let invalid = runtime
        .discover_models(
            NAME,
            LlmModelDiscoveryRequest {
                base_url: Some(String::new()),
                ..LlmModelDiscoveryRequest::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error_code(&invalid), Some("INVALID_DISCOVERY"));
}

#[tokio::test]
async fn configured_routes_register_atomically_expose_metadata_and_unwind() {
    let context = Context::new();
    let runtime = LlmRuntime::install(&context).unwrap();
    let fiber = context
        .plugin(
            plugin(),
            json!({
                "providers": {
                    "openai": {
                        "displayName":"OpenAI Route",
                        "retryPolicy":{"mode":"always","backoff":{"initialDelayMs":25,"maxDelayMs":100,"jitterRatio":0.2}}
                    },
                    "anthropic": {}
                }
            }),
        )
        .unwrap();
    fiber.await_settled().await.unwrap();
    assert_eq!(
        runtime
            .list_providers()
            .iter()
            .map(|provider| (provider.id.as_str(), provider.name.as_str()))
            .collect::<Vec<_>>(),
        vec![("openai", "OpenAI Route"), ("anthropic", "anthropic")]
    );
    assert!(
        runtime
            .list_models("openai")
            .await
            .unwrap()
            .iter()
            .any(|model| model.id.as_str() == "gpt-4.1")
    );
    let info = runtime
        .resolve_model_info("openai", "gpt-4.1", None)
        .await
        .unwrap();
    assert!(info.context.unwrap().context_window > 0);
    fiber.dispose().await.unwrap();
    assert!(runtime.list_providers().is_empty());
    assert!(runtime.list_configurable_providers().is_empty());
    let unloaded = runtime
        .discover_models(
            NAME,
            LlmModelDiscoveryRequest {
                provider: Some(ProviderId::new("openai")),
                ..LlmModelDiscoveryRequest::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error_code(&unloaded), Some("NO_DISCOVERY"));
}

#[tokio::test]
async fn vertex_catalog_route_registers_without_requiring_credentials_until_request_time() {
    let context = Context::new();
    let runtime = LlmRuntime::install(&context).unwrap();
    let fiber = context
        .plugin(plugin(), json!({"providers":{"google-vertex":{}}}))
        .unwrap();
    fiber.await_settled().await.unwrap();
    assert_eq!(runtime.list_providers()[0].id.as_str(), "google-vertex");
    assert!(
        runtime
            .list_models("google-vertex")
            .await
            .unwrap()
            .iter()
            .any(|model| model.id.as_str() == "gemini-2.5-flash")
    );
}
