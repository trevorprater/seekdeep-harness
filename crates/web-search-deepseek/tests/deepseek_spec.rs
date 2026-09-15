//! Parity mirror of the source deepseek.spec.ts provider suite.

mod support;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use futures::FutureExt as _;
use parking_lot::Mutex;
use seekdeep_cordis::Context;
use seekdeep_llm::{AbortSignal, HarnessError};
use seekdeep_util::launch_environment::{
    LaunchEnvironmentLayerInput, LaunchEnvironmentSource, SEEKDEEP_LAUNCH_ENVIRONMENT,
    create_launch_environment_snapshot,
};
use seekdeep_web::{WebRuntime, WebRuntimeConfig, WebSearchProvider, WebSearchRequest};
use seekdeep_web_search_deepseek::{
    AnthropicResponse, DEEPSEEK_PROVIDER_ID, DeepSeekSearchConfig, DeepSeekSearchLlmRequest,
    DeepSeekSearchProvider, DeepSeekSearchProviderOptions, citation_snippets, install,
    map_anthropic_response, resolve_options,
};
use serde_json::{Value, json};
use support::{MemorySettings, MockServer, ResponseSpec};

fn base_options(base_url: String) -> DeepSeekSearchProviderOptions {
    DeepSeekSearchProviderOptions {
        api_key: Some("ds-key".to_owned()),
        resolve_api_key: None,
        api_key_env: None,
        base_url,
        model: "deepseek-chat".to_owned(),
        api_version: "2023-06-01".to_owned(),
        max_tokens: 4096,
        max_uses: 5,
        record_request: None,
    }
}

fn anthropic(value: Value) -> AnthropicResponse {
    serde_json::from_value(value).expect("fixture parses")
}

fn search_response() -> Value {
    json!({
        "content": [
            { "type": "text", "text": "Here is what I found.", "citations": [
                { "type": "web_search_result_location", "url": "https://a.test", "cited_text": "excerpt for A" }
            ]},
            { "type": "web_search_tool_result", "content": [
                { "type": "web_search_result", "url": "https://a.test", "title": "A", "page_age": "2026-02-02" },
                { "type": "web_search_result", "url": "https://b.test", "title": "B" }
            ]}
        ]
    })
}

fn request(query: &str) -> WebSearchRequest {
    WebSearchRequest {
        query: query.to_owned(),
        max_results: None,
    }
}

fn error_code(error: &anyhow::Error) -> String {
    error
        .downcast_ref::<HarnessError>()
        .map_or_else(|| "?".to_owned(), |e| e.code().to_owned())
}

#[test]
fn citation_snippets_maps_url_to_text_first_wins() {
    let blocks = anthropic(json!({
        "content": [
            { "type": "text", "citations": [
                { "url": "https://a.test", "cited_text": "first" },
                { "url": "https://a.test", "cited_text": "second" }
            ]},
            { "type": "text", "citations": [
                { "url": "https://b.test", "cited_text": "b text" }
            ]}
        ]
    }));
    let map = citation_snippets(blocks.content.as_deref().unwrap());
    assert_eq!(map.get("https://a.test").map(String::as_str), Some("first"));
    assert_eq!(
        map.get("https://b.test").map(String::as_str),
        Some("b text")
    );
}

#[test]
fn citation_snippets_ignores_missing_url_or_text() {
    let blocks = anthropic(json!({
        "content": [
            { "type": "text", "citations": [
                { "url": "https://a.test" },
                { "cited_text": "orphan" },
                { "url": "", "cited_text": "empty url" }
            ]}
        ]
    }));
    let map = citation_snippets(blocks.content.as_deref().unwrap());
    assert!(map.is_empty());
}

#[test]
fn map_response_joins_snippets_and_maps_page_age() {
    let result = map_anthropic_response(&anthropic(search_response())).expect("maps");
    assert_eq!(
        result,
        seekdeep_web::WebSearchResult {
            content: None,
            sources: vec![
                seekdeep_web::WebSearchSource {
                    url: "https://a.test".to_owned(),
                    title: Some("A".to_owned()),
                    snippet: Some("excerpt for A".to_owned()),
                    published_at: Some("2026-02-02".to_owned()),
                },
                seekdeep_web::WebSearchSource {
                    url: "https://b.test".to_owned(),
                    title: Some("B".to_owned()),
                    snippet: None,
                    published_at: None,
                },
            ],
            truncated: false,
        }
    );
}

#[test]
fn map_response_dedupes_repeated_urls() {
    let result = map_anthropic_response(&anthropic(json!({
        "content": [
            { "type": "web_search_tool_result", "content": [
                { "type": "web_search_result", "url": "https://a.test", "title": "first" }
            ]},
            { "type": "web_search_tool_result", "content": [
                { "type": "web_search_result", "url": "https://a.test", "title": "second" }
            ]}
        ]
    })))
    .expect("maps");
    assert_eq!(result.sources.len(), 1);
    assert_eq!(result.sources[0].title.as_deref(), Some("first"));
}

#[test]
fn map_response_skips_non_result_items_and_empty_urls() {
    let result = map_anthropic_response(&anthropic(json!({
        "content": [{ "type": "web_search_tool_result", "content": [
            { "type": "web_search_result_error", "url": "https://err.test" },
            { "type": "web_search_result", "url": "" },
            { "type": "web_search_result", "url": "https://ok.test" }
        ]}]
    })))
    .expect("maps");
    assert_eq!(result.sources.len(), 1);
    assert_eq!(result.sources[0].url, "https://ok.test");
}

#[test]
fn map_response_omits_empty_optional_fields() {
    let result = map_anthropic_response(&anthropic(json!({
        "content": [{ "type": "web_search_tool_result", "content": [
            { "type": "web_search_result", "url": "https://a.test", "title": "", "page_age": "" }
        ]}]
    })))
    .expect("maps");
    assert_eq!(
        result.sources,
        vec![seekdeep_web::WebSearchSource {
            url: "https://a.test".to_owned(),
            title: None,
            snippet: None,
            published_at: None,
        }]
    );
}

#[test]
fn map_response_tolerates_text_without_citations_and_empty_result_blocks() {
    let result = map_anthropic_response(&anthropic(json!({
        "content": [
            { "type": "text", "text": "no citations" },
            { "type": "web_search_tool_result" },
            { "type": "web_search_tool_result", "content": [
                { "type": "web_search_result", "url": "https://a.test", "title": "A" }
            ]}
        ]
    })))
    .expect("maps");
    assert_eq!(result.sources.len(), 1);
}

#[test]
fn map_response_strict_mode_errors_without_result_blocks() {
    let err = map_anthropic_response(&anthropic(json!({
        "content": [{ "type": "text", "text": "just prose" }]
    })))
    .expect_err("strict");
    assert_eq!(error_code(&err), "WEB_PROVIDER_ERROR");
}

#[test]
fn map_response_strict_mode_errors_without_content() {
    let err = map_anthropic_response(&anthropic(json!({}))).expect_err("strict");
    assert_eq!(error_code(&err), "WEB_PROVIDER_ERROR");
}

#[test]
fn availability_gates_key_url_and_limits() {
    let options = base_options("https://api.deepseek.test/anthropic/v1".to_owned());
    assert!(DeepSeekSearchProvider::new(move || options.clone()).available());

    let mut no_key = base_options("https://x.test".to_owned());
    no_key.api_key = Some(String::new());
    assert!(!DeepSeekSearchProvider::new(move || no_key.clone()).available());

    let mut bad_url = base_options("not a url".to_owned());
    bad_url.api_key = Some("k".to_owned());
    assert!(!DeepSeekSearchProvider::new(move || bad_url.clone()).available());

    let mut zero_tokens = base_options("https://x.test".to_owned());
    zero_tokens.max_tokens = 0;
    assert!(!DeepSeekSearchProvider::new(move || zero_tokens.clone()).available());

    let mut zero_uses = base_options("https://x.test".to_owned());
    zero_uses.max_uses = 0;
    assert!(!DeepSeekSearchProvider::new(move || zero_uses.clone()).available());
}

#[tokio::test]
async fn records_and_posts_the_same_request() {
    let server = MockServer::start(ResponseSpec::json(200, search_response())).await;
    let recorded = Arc::new(Mutex::new(None));
    let recorded_clone = recorded.clone();
    let base = server.url.clone();
    let provider = DeepSeekSearchProvider::new(move || {
        let mut options = base_options(base.clone());
        let rec = recorded_clone.clone();
        options.record_request = Some(Arc::new(move |request: &DeepSeekSearchLlmRequest| {
            *rec.lock() = Some(request.clone());
            Ok(())
        }));
        options
    });
    let result = provider
        .search(&request("hello"), None)
        .await
        .expect("search");
    assert!(!result.truncated);

    let recorded = recorded.lock().clone().expect("recorded");
    assert_eq!(recorded.endpoint, format!("{}/messages", server.url));
    assert_eq!(recorded.api_version, "2023-06-01");
    assert_eq!(
        recorded.body,
        json!({
            "model": "deepseek-chat",
            "max_tokens": 4096,
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "Perform a web search for the query: hello"}
            ]}],
            "tools": [{"type": "web_search_20250305", "name": "web_search", "max_uses": 5}]
        })
    );

    let requests = server.take_requests();
    assert_eq!(requests.len(), 1);
    let captured = &requests[0];
    assert_eq!(captured.method, "POST");
    assert_eq!(captured.path, "/messages");
    assert_eq!(
        captured.headers.get("x-api-key").map(String::as_str),
        Some("ds-key")
    );
    assert_eq!(
        captured.headers.get("authorization").map(String::as_str),
        Some("Bearer ds-key")
    );
    assert_eq!(
        captured
            .headers
            .get("anthropic-version")
            .map(String::as_str),
        Some("2023-06-01")
    );
    assert_eq!(
        captured.headers.get("content-type").map(String::as_str),
        Some("application/json")
    );
    let body: Value = serde_json::from_slice(&captured.body).expect("json body");
    assert_eq!(body, recorded.body);
}

#[tokio::test]
async fn serves_one_search_from_one_snapshot() {
    let server = MockServer::start(ResponseSpec::json(200, search_response())).await;
    let target = server.url.clone();
    let current = Arc::new(Mutex::new(base_options(target)));
    let provider = DeepSeekSearchProvider::new(move || current.lock().clone());
    // Resolve the key then rewrite the endpoint mid-operation through a second thunk.
    let before = base_options(server.url.clone());
    let provider2 = DeepSeekSearchProvider::new(move || before.clone());
    let result = provider2.search(&request("q"), None).await.expect("search");
    assert!(!result.truncated);
    let requests = server.take_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/messages");
    // A settings write landing after the snapshot must not change this operation.
    drop(provider);
}

#[tokio::test]
async fn pre_aborted_call_does_not_resolve_or_dispatch() {
    let called = Arc::new(AtomicBool::new(false));
    let called_clone = called.clone();
    let signal = AbortSignal::default();
    signal.abort();
    let provider = DeepSeekSearchProvider::new(move || {
        let mut options = base_options("https://x.test".to_owned());
        options.api_key = None;
        let flag = called_clone.clone();
        options.resolve_api_key = Some(Arc::new(move || {
            flag.store(true, Ordering::SeqCst);
            async { Ok(Some("late-key".to_owned())) }.boxed()
        }));
        options
    });
    let err = provider
        .search(&request("q"), Some(signal))
        .await
        .expect_err("aborted");
    assert_eq!(error_code(&err), "WEB_ABORTED");
    assert!(!called.load(Ordering::SeqCst));
}

#[tokio::test]
async fn aborts_while_uncooperative_resolver_pending() {
    let signal = AbortSignal::default();
    let provider = DeepSeekSearchProvider::new(move || {
        let mut options = base_options("https://x.test".to_owned());
        options.api_key = None;
        options.resolve_api_key = Some(Arc::new(|| futures::future::pending().boxed()));
        options
    });
    let req = request("q");
    let search = provider.search(&req, Some(signal.clone()));
    signal.abort();
    let err = search.await.expect_err("aborted");
    assert_eq!(error_code(&err), "WEB_ABORTED");
}

#[tokio::test]
async fn maps_credential_resolver_rejection() {
    let signal = AbortSignal::default();
    let provider = DeepSeekSearchProvider::new(move || {
        let mut options = base_options("https://x.test".to_owned());
        options.api_key = None;
        options.resolve_api_key = Some(Arc::new(|| {
            async {
                Err::<Option<String>, anyhow::Error>(anyhow::anyhow!("credential backend failed"))
            }
            .boxed()
        }));
        options
    });
    let err = provider
        .search(&request("q"), Some(signal))
        .await
        .expect_err("rejected");
    assert_eq!(error_code(&err), "WEB_PROVIDER_ERROR");
    assert!(err.to_string().contains("credential resolution failed"));
}

#[tokio::test]
async fn uses_default_credential_reference_when_no_resolver() {
    let provider = DeepSeekSearchProvider::new(move || {
        let mut options = base_options("https://x.test".to_owned());
        options.api_key = None;
        options.resolve_api_key = None;
        options
    });
    let err = provider
        .search(&request("q"), None)
        .await
        .expect_err("missing");
    assert_eq!(error_code(&err), "WEB_PROVIDER_CREDENTIAL_MISSING");
    assert!(err.to_string().contains("DEEPSEEK_API_KEY"));
}

#[tokio::test]
async fn observes_synchronous_cancellation_from_resolver() {
    let signal = AbortSignal::default();
    let resolver_signal = signal.clone();
    let provider = DeepSeekSearchProvider::new(move || {
        let mut options = base_options("https://x.test".to_owned());
        options.api_key = None;
        let sig = resolver_signal.clone();
        options.resolve_api_key = Some(Arc::new(move || {
            sig.abort();
            async { Ok(Some("unused-key".to_owned())) }.boxed()
        }));
        options
    });
    let err = provider
        .search(&request("q"), Some(signal))
        .await
        .expect_err("aborted");
    assert_eq!(error_code(&err), "WEB_ABORTED");
}

async fn search_error(response: ResponseSpec) -> anyhow::Error {
    let server = MockServer::start(response).await;
    let base = server.url.clone();
    let provider = DeepSeekSearchProvider::new(move || base_options(base.clone()));
    provider
        .search(&request("q"), None)
        .await
        .expect_err("error")
}

#[tokio::test]
async fn maps_http_error_to_provider_message() {
    let err = search_error(ResponseSpec::json(
        429,
        json!({"error": {"message": "rate limited"}}),
    ))
    .await;
    assert_eq!(error_code(&err), "WEB_PROVIDER_ERROR");
    assert_eq!(err.to_string(), "rate limited");
}

#[tokio::test]
async fn handles_string_error_body() {
    let err = search_error(ResponseSpec::json(400, json!({"error": "bad request"}))).await;
    assert_eq!(error_code(&err), "WEB_PROVIDER_ERROR");
    assert_eq!(err.to_string(), "bad request");
}

#[tokio::test]
async fn keeps_status_line_when_error_body_not_json() {
    let err = search_error(ResponseSpec::plain(503, "upstream error")).await;
    assert_eq!(error_code(&err), "WEB_PROVIDER_ERROR");
    assert_eq!(err.to_string(), "DeepSeek API error (HTTP 503)");
}

#[tokio::test]
async fn keeps_status_line_when_json_error_body_no_detail() {
    let err = search_error(ResponseSpec::json(500, json!({}))).await;
    assert_eq!(error_code(&err), "WEB_PROVIDER_ERROR");
    assert_eq!(err.to_string(), "DeepSeek API error (HTTP 500)");
}

#[tokio::test]
async fn maps_unparseable_success_body() {
    let err = search_error(ResponseSpec::plain(200, "not json")).await;
    assert_eq!(error_code(&err), "WEB_PROVIDER_ERROR");
}

#[tokio::test]
async fn maps_wrong_shape_to_provider_error() {
    let err = search_error(ResponseSpec::json(200, json!({"content": {}}))).await;
    assert_eq!(error_code(&err), "WEB_PROVIDER_ERROR");
}

#[tokio::test]
async fn strict_mode_prose_only_response_errors() {
    let err = search_error(ResponseSpec::json(
        200,
        json!({"content": [{"type": "text", "text": "no search happened"}]}),
    ))
    .await;
    assert_eq!(error_code(&err), "WEB_PROVIDER_ERROR");
    assert!(err.to_string().contains("no web_search_tool_result"));
}

#[tokio::test]
async fn maps_network_failure() {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind");
    let address = listener.local_addr().expect("addr");
    drop(listener);
    let provider = DeepSeekSearchProvider::new(move || base_options(format!("http://{address}")));
    let err = provider
        .search(&request("q"), None)
        .await
        .expect_err("network");
    assert_eq!(error_code(&err), "WEB_PROVIDER_ERROR");
}

#[tokio::test]
async fn resolve_options_defaults_and_env_fallback() {
    let context = Context::new();
    let snapshot = create_launch_environment_snapshot(&[LaunchEnvironmentLayerInput {
        source: LaunchEnvironmentSource::Process,
        path: None,
        values: [("DEEPSEEK_API_KEY".to_owned(), "env-key".to_owned())]
            .into_iter()
            .collect(),
    }]);
    context
        .provide(SEEKDEEP_LAUNCH_ENVIRONMENT, Arc::new(snapshot))
        .expect("provide env");
    let options = resolve_options(&context, &DeepSeekSearchConfig::default()).expect("resolve");
    assert_eq!(options.base_url, "https://api.deepseek.com/anthropic/v1");
    assert_eq!(options.model, "deepseek-v4-flash");
    assert_eq!(options.api_version, "2023-06-01");
    assert_eq!(options.max_tokens, 4096);
    assert_eq!(options.max_uses, 5);
    assert_eq!(options.api_key, None);
    let resolved = (options.resolve_api_key.as_ref().expect("resolver"))()
        .await
        .expect("key");
    assert_eq!(resolved.as_deref(), Some("env-key"));
}

#[tokio::test]
async fn plugin_registration_and_unregistration() {
    let server = MockServer::start(ResponseSpec::json(200, search_response())).await;
    let context = Context::new();
    let runtime = WebRuntime::new(
        &context,
        &WebRuntimeConfig {
            search_provider: Some(DEEPSEEK_PROVIDER_ID.to_owned()),
            fetch_provider: None,
        },
    )
    .expect("web runtime");
    let config = DeepSeekSearchConfig {
        api_key: Some("ds-key".to_owned()),
        base_url: Some(server.url.clone()),
        ..DeepSeekSearchConfig::default()
    };
    let fiber = install(&context, config).expect("install");
    fiber.await_settled().await.expect("settled");
    let result = runtime.search(&request("q"), None).await.expect("search");
    assert!(!result.truncated);

    fiber.dispose().await.expect("dispose");
    let err = runtime
        .search(&request("q"), None)
        .await
        .expect_err("missing");
    assert_eq!(error_code(&err), "WEB_PROVIDER_CONFIGURED_MISSING");
}

#[tokio::test]
async fn plugin_rejects_invalid_limits_at_construction() {
    let context = Context::new();
    WebRuntime::new(
        &context,
        &WebRuntimeConfig {
            search_provider: Some(DEEPSEEK_PROVIDER_ID.to_owned()),
            fetch_provider: None,
        },
    )
    .expect("web runtime");
    let config = DeepSeekSearchConfig {
        api_key: Some("ds-key".to_owned()),
        max_tokens: Some(0.0),
        ..DeepSeekSearchConfig::default()
    };
    let fiber = install(&context, config).expect("install");
    let err = fiber.await_settled().await.expect_err("invalid");
    assert!(err.to_string().contains("maxTokens"));

    let config = DeepSeekSearchConfig {
        api_key: Some("ds-key".to_owned()),
        max_uses: Some(1.5),
        ..DeepSeekSearchConfig::default()
    };
    let fiber = install(&context, config).expect("install");
    let err = fiber.await_settled().await.expect_err("invalid");
    assert!(err.to_string().contains("maxUses"));
}

#[tokio::test]
async fn settings_namespace_lifecycle() {
    let context = Context::new();
    WebRuntime::new(
        &context,
        &WebRuntimeConfig {
            search_provider: Some(DEEPSEEK_PROVIDER_ID.to_owned()),
            fetch_provider: None,
        },
    )
    .expect("web runtime");
    let storage = MemorySettings::new();
    let settings = seekdeep_settings::SettingsService::install(&context, storage)
        .await
        .expect("settings");
    let config = DeepSeekSearchConfig {
        api_key: Some("ds-key".to_owned()),
        base_url: Some("https://search.entry.test/v1".to_owned()),
        ..DeepSeekSearchConfig::default()
    };
    let fiber = install(&context, config).expect("install");
    fiber.await_settled().await.expect("settled");
    let ns = seekdeep_web_search_deepseek::web_search_deepseek_settings_namespace().expect("ns");
    assert!(
        settings
            .describe(false)
            .iter()
            .any(|row| row.ns.as_str() == ns.as_str())
    );
    fiber.dispose().await.expect("dispose");
    assert!(
        !settings
            .describe(false)
            .iter()
            .any(|row| row.ns.as_str() == ns.as_str())
    );
}
