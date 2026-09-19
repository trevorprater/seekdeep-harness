//! Parity mirror of the source settings.spec.ts settings-section suite.

use super::support;

use seekdeep_cordis::Context;
use seekdeep_web::{WebRuntime, WebRuntimeConfig, WebSearchRequest};
use seekdeep_web_search_deepseek::{
    DEEPSEEK_PROVIDER_ID, DeepSeekSearchConfig, install, web_search_deepseek_settings_namespace,
};
use serde_json::{Value, json};
use support::{MemorySettings, MockServer, ResponseSpec};

fn one_result() -> Value {
    json!({
        "content": [
            { "type": "text", "text": "ok" },
            { "type": "web_search_tool_result", "content": [
                { "type": "web_search_result", "url": "https://a.test", "title": "A" }
            ]}
        ]
    })
}

async fn search_once(runtime: &WebRuntime) {
    runtime
        .search(
            &WebSearchRequest {
                query: "anything".to_owned(),
                max_results: None,
            },
            None,
        )
        .await
        .expect("search");
}

#[tokio::test]
async fn serves_stored_endpoint_without_re_registration() {
    let entry = MockServer::start(ResponseSpec::json(200, one_result())).await;
    let stored = MockServer::start(ResponseSpec::json(200, one_result())).await;

    let context = Context::new();
    let runtime = WebRuntime::new(
        &context,
        &WebRuntimeConfig {
            search_provider: Some(DEEPSEEK_PROVIDER_ID.to_owned()),
            fetch_provider: None,
        },
    )
    .expect("web runtime");
    seekdeep_settings::SettingsService::install(&context, MemorySettings::new())
        .await
        .expect("settings");
    let config = DeepSeekSearchConfig {
        api_key: Some("ds-key".to_owned()),
        base_url: Some(entry.url.clone()),
        ..DeepSeekSearchConfig::default()
    };
    let fiber = install(&context, config).expect("install");
    fiber.await_settled().await.expect("settled");

    search_once(&runtime).await;
    assert_eq!(entry.take_requests().len(), 1);
    assert!(stored.take_requests().is_empty());

    let ns = web_search_deepseek_settings_namespace().expect("ns");
    let settings = context
        .get(seekdeep_settings::SETTINGS)
        .expect("settings service");
    settings
        .update(&ns, json!({ "baseURL": stored.url }), None)
        .await
        .expect("update");

    search_once(&runtime).await;
    assert!(entry.take_requests().is_empty());
    assert_eq!(stored.take_requests().len(), 1);
}

#[tokio::test]
async fn rejects_an_invalid_credential_reference_at_install() {
    let context = Context::new();
    let _runtime = WebRuntime::new(
        &context,
        &WebRuntimeConfig {
            search_provider: Some(DEEPSEEK_PROVIDER_ID.to_owned()),
            fetch_provider: None,
        },
    )
    .expect("web runtime");
    seekdeep_settings::SettingsService::install(&context, MemorySettings::new())
        .await
        .expect("settings");

    // A reference that is a valid string but not a valid credential reference must not survive
    // installation: the provider's options closure cannot report the failure, so accepting it
    // here would abort the first search instead.
    let fiber = install(
        &context,
        DeepSeekSearchConfig {
            api_key_env: Some("BAD-NAME".to_owned()),
            ..DeepSeekSearchConfig::default()
        },
    )
    .expect("fiber");
    let error = fiber
        .await_settled()
        .await
        .expect_err("an invalid credential reference must be rejected at install");
    assert!(error.to_string().contains("credential"), "{error}");

    // The same install with a valid reference still settles.
    let fiber =
        install(&context, DeepSeekSearchConfig::default()).expect("valid reference installs");
    fiber.await_settled().await.expect("settled");
}
