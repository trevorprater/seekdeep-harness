//! Parity mirror of the source perplexity.spec.ts provider suite.

mod support;

use std::sync::Arc;

use seekdeep_cordis::Context;
use seekdeep_llm::{AbortSignal, HarnessError};
use seekdeep_util::launch_environment::{
    LaunchEnvironmentLayerInput, LaunchEnvironmentSource, SEEKDEEP_LAUNCH_ENVIRONMENT,
    create_launch_environment_snapshot,
};
use seekdeep_web::{WebRuntime, WebRuntimeConfig, WebSearchProvider, WebSearchRequest};
use seekdeep_web_search_perplexity::{
    PERPLEXITY_PROVIDER_ID, PerplexityConfig, PerplexityRecency, PerplexityResponse,
    PerplexitySearchProvider, PerplexitySearchProviderOptions, install, map_perplexity_response,
    resolve_options,
};
use serde_json::{Value, json};
use support::{MockServer, ResponseSpec};

fn options(api_key: &str, base_url: &str) -> PerplexitySearchProviderOptions {
    PerplexitySearchProviderOptions {
        api_key: api_key.to_owned(),
        base_url: base_url.to_owned(),
        model: "sonar".to_owned(),
        max_tokens: 1024.0,
        search_recency: None,
    }
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

fn perplexity_response(value: Value) -> PerplexityResponse {
    serde_json::from_value(value).expect("fixture parses")
}

#[test]
fn maps_the_answer_and_prefers_structured_search_results() {
    let result = map_perplexity_response(&perplexity_response(json!({
        "choices": [{ "message": { "content": "the answer" } }],
        "search_results": [
            { "url": "https://a.test", "title": "A", "snippet": "snip", "date": "2026-02-02" },
            { "url": "https://b.test" },
        ],
        "citations": ["https://ignored.test"],
    })));
    assert_eq!(
        result,
        seekdeep_web::WebSearchResult {
            content: Some("the answer".to_owned()),
            sources: vec![
                seekdeep_web::WebSearchSource {
                    url: "https://a.test".to_owned(),
                    title: Some("A".to_owned()),
                    snippet: Some("snip".to_owned()),
                    published_at: Some("2026-02-02".to_owned()),
                },
                seekdeep_web::WebSearchSource {
                    url: "https://b.test".to_owned(),
                    title: None,
                    snippet: None,
                    published_at: None,
                },
            ],
            truncated: false,
        }
    );
}

#[test]
fn falls_back_to_url_only_citations_when_search_results_absent() {
    let result = map_perplexity_response(&perplexity_response(json!({
        "choices": [{ "message": { "content": "answer" } }],
        "citations": ["https://a.test", "https://b.test"],
    })));
    assert_eq!(
        result.sources,
        vec![
            seekdeep_web::WebSearchSource {
                url: "https://a.test".to_owned(),
                title: None,
                snippet: None,
                published_at: None,
            },
            seekdeep_web::WebSearchSource {
                url: "https://b.test".to_owned(),
                title: None,
                snippet: None,
                published_at: None,
            },
        ]
    );
}

#[test]
fn omits_content_when_answer_empty_or_missing() {
    assert!(
        map_perplexity_response(&perplexity_response(json!({"citations": []})))
            .content
            .is_none()
    );
    assert!(
        map_perplexity_response(&perplexity_response(json!({
            "choices": [{ "message": { "content": "" } }],
        })))
        .content
        .is_none()
    );
    assert!(
        map_perplexity_response(&perplexity_response(json!({
            "choices": [{ "message": { "content": null } }],
        })))
        .content
        .is_none()
    );
}

#[test]
fn omits_null_empty_optional_source_fields() {
    let result = map_perplexity_response(&perplexity_response(json!({
        "search_results": [{ "url": "https://a.test", "title": null, "snippet": "", "date": null }],
    })));
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
fn yields_no_sources_when_neither_search_results_nor_citations_present() {
    assert!(
        map_perplexity_response(&perplexity_response(json!({
            "choices": [{ "message": { "content": "a" } }],
        })))
        .sources
        .is_empty()
    );
}

#[test]
fn availability_gates_key_base_url_and_max_tokens() {
    assert!(!PerplexitySearchProvider::new(options("", "https://api.perplexity.test")).available());
    assert!(
        PerplexitySearchProvider::new(options("pplx-key", "https://api.perplexity.test"))
            .available()
    );
    assert!(!PerplexitySearchProvider::new(options("pplx-key", "not a url")).available());

    let mut zero_tokens = options("pplx-key", "https://api.perplexity.test");
    zero_tokens.max_tokens = 0.0;
    assert!(!PerplexitySearchProvider::new(zero_tokens).available());

    let mut fractional_tokens = options("pplx-key", "https://api.perplexity.test");
    fractional_tokens.max_tokens = 1.5;
    assert!(!PerplexitySearchProvider::new(fractional_tokens).available());
}

#[tokio::test]
async fn sends_chat_completions_request_with_query_model_and_max_tokens() {
    let server = MockServer::start(ResponseSpec::json(
        200,
        json!({"choices": [{ "message": { "content": "a" } }], "citations": []}),
    ))
    .await;
    let provider = PerplexitySearchProvider::new(options("pplx-key", &server.url));
    provider
        .search(&request("hello"), None)
        .await
        .expect("search");

    let requests = server.take_requests();
    assert_eq!(requests.len(), 1);
    let captured = &requests[0];
    assert_eq!(captured.method, "POST");
    assert_eq!(captured.path, "/chat/completions");
    assert_eq!(
        captured.headers.get("authorization").map(String::as_str),
        Some("Bearer pplx-key")
    );
    let body: Value = serde_json::from_slice(&captured.body).expect("json body");
    assert_eq!(
        body,
        json!({
            "model": "sonar",
            "max_tokens": 1024.0,
            "messages": [{ "role": "user", "content": "hello" }],
        })
    );
}

#[tokio::test]
async fn sends_search_recency_filter_when_configured_and_omits_otherwise() {
    let server = MockServer::start(ResponseSpec::json(
        200,
        json!({"choices": [{ "message": { "content": "a" } }], "citations": []}),
    ))
    .await;
    let mut with_recency = options("pplx-key", &server.url);
    with_recency.search_recency = Some(PerplexityRecency::Week);
    PerplexitySearchProvider::new(with_recency)
        .search(&request("q"), None)
        .await
        .expect("search");
    PerplexitySearchProvider::new(options("pplx-key", &server.url))
        .search(&request("q"), None)
        .await
        .expect("search");

    let requests = server.take_requests();
    let body: Value = serde_json::from_slice(&requests[0].body).expect("json body");
    assert_eq!(body["search_recency_filter"], json!("week"));
    let body2: Value = serde_json::from_slice(&requests[1].body).expect("json body");
    assert!(body2.get("search_recency_filter").is_none());
}

async fn search_error(response: ResponseSpec) -> anyhow::Error {
    let server = MockServer::start(response).await;
    let provider = PerplexitySearchProvider::new(options("pplx-key", &server.url));
    provider
        .search(&request("q"), None)
        .await
        .expect_err("error")
}

#[tokio::test]
async fn maps_http_error_object_to_provider_message() {
    let err = search_error(ResponseSpec::json(
        429,
        json!({"error": {"message": "rate limited"}}),
    ))
    .await;
    assert_eq!(error_code(&err), "WEB_PROVIDER_ERROR");
    assert_eq!(err.to_string(), "rate limited");
}

#[tokio::test]
async fn handles_string_form_error_body() {
    let err = search_error(ResponseSpec::json(400, json!({"error": "bad request"}))).await;
    assert_eq!(error_code(&err), "WEB_PROVIDER_ERROR");
    assert_eq!(err.to_string(), "bad request");
}

#[tokio::test]
async fn maps_wrong_shape_to_provider_error() {
    let err = search_error(ResponseSpec::json(200, json!({"search_results": null}))).await;
    assert_eq!(error_code(&err), "WEB_PROVIDER_ERROR");
}

#[tokio::test]
async fn keeps_status_line_when_error_body_not_json() {
    let err = search_error(ResponseSpec::plain(503, "upstream error")).await;
    assert_eq!(error_code(&err), "WEB_PROVIDER_ERROR");
    assert_eq!(err.to_string(), "Perplexity API error (HTTP 503)");
}

#[tokio::test]
async fn keeps_status_line_when_json_error_body_carries_no_detail() {
    let err = search_error(ResponseSpec::json(500, json!({}))).await;
    assert_eq!(error_code(&err), "WEB_PROVIDER_ERROR");
    assert_eq!(err.to_string(), "Perplexity API error (HTTP 500)");
}

#[tokio::test]
async fn maps_abort_to_web_aborted() {
    let server = MockServer::start(ResponseSpec::json(200, json!({"citations": []}))).await;
    let provider = PerplexitySearchProvider::new(options("pplx-key", &server.url));
    let signal = AbortSignal::default();
    signal.abort();
    let err = provider
        .search(&request("q"), Some(signal))
        .await
        .expect_err("aborted");
    assert_eq!(error_code(&err), "WEB_ABORTED");
}

#[tokio::test]
async fn maps_unparseable_success_body() {
    let err = search_error(ResponseSpec::plain(200, "not json")).await;
    assert_eq!(error_code(&err), "WEB_PROVIDER_ERROR");
}

#[tokio::test]
async fn maps_network_failure() {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind");
    let address = listener.local_addr().expect("addr");
    drop(listener);
    let provider = PerplexitySearchProvider::new(options("pplx-key", &format!("http://{address}")));
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
        values: [("PERPLEXITY_API_KEY".to_owned(), "env-key".to_owned())]
            .into_iter()
            .collect(),
    }]);
    context
        .provide(SEEKDEEP_LAUNCH_ENVIRONMENT, Arc::new(snapshot))
        .expect("provide env");
    let options = resolve_options(&context, &PerplexityConfig::default());
    assert_eq!(options.api_key, "env-key");
    assert_eq!(options.base_url, "https://api.perplexity.ai");
    assert_eq!(options.model, "sonar");
    assert!((options.max_tokens - 1024.0).abs() < f64::EPSILON);
    assert_eq!(options.search_recency, None);
}

#[tokio::test]
async fn plugin_registration_and_unregistration() {
    let server = MockServer::start(ResponseSpec::json(
        200,
        json!({"choices": [{ "message": { "content": "a" } }], "citations": []}),
    ))
    .await;
    let context = Context::new();
    let runtime = WebRuntime::new(
        &context,
        &WebRuntimeConfig {
            search_provider: Some(PERPLEXITY_PROVIDER_ID.to_owned()),
            fetch_provider: None,
        },
    )
    .expect("web runtime");
    let config = PerplexityConfig {
        api_key: Some("pplx-key".to_owned()),
        base_url: Some(server.url.clone()),
        ..PerplexityConfig::default()
    };
    let fiber = install(&context, config).expect("install");
    fiber.await_settled().await.expect("settled");
    let result = runtime.search(&request("q"), None).await.expect("search");
    assert_eq!(result.content.as_deref(), Some("a"));
    assert!(result.sources.is_empty());

    fiber.dispose().await.expect("dispose");
    let err = runtime
        .search(&request("q"), None)
        .await
        .expect_err("missing");
    assert_eq!(error_code(&err), "WEB_PROVIDER_CONFIGURED_MISSING");
}

#[tokio::test]
async fn plugin_threads_config_into_request() {
    let server = MockServer::start(ResponseSpec::json(
        200,
        json!({"choices": [{ "message": { "content": "a" } }], "citations": []}),
    ))
    .await;
    let context = Context::new();
    let runtime = WebRuntime::new(
        &context,
        &WebRuntimeConfig {
            search_provider: Some(PERPLEXITY_PROVIDER_ID.to_owned()),
            fetch_provider: None,
        },
    )
    .expect("web runtime");
    let config = PerplexityConfig {
        api_key: Some("pplx-key".to_owned()),
        base_url: Some(server.url.clone()),
        max_tokens: Some(256.0),
        search_recency: Some(PerplexityRecency::Month),
        ..PerplexityConfig::default()
    };
    let fiber = install(&context, config).expect("install");
    fiber.await_settled().await.expect("settled");
    runtime.search(&request("q"), None).await.expect("search");
    let requests = server.take_requests();
    let body: Value = serde_json::from_slice(&requests[0].body).expect("json body");
    assert_eq!(body["max_tokens"], json!(256.0));
    assert_eq!(body["search_recency_filter"], json!("month"));
    fiber.dispose().await.expect("dispose");
}

#[tokio::test]
async fn plugin_unavailable_without_key() {
    let context = Context::new();
    let snapshot = create_launch_environment_snapshot(&[]);
    context
        .provide(SEEKDEEP_LAUNCH_ENVIRONMENT, Arc::new(snapshot))
        .expect("provide env");
    let runtime = WebRuntime::new(
        &context,
        &WebRuntimeConfig {
            search_provider: Some(PERPLEXITY_PROVIDER_ID.to_owned()),
            fetch_provider: None,
        },
    )
    .expect("web runtime");
    let fiber = install(&context, PerplexityConfig::default()).expect("install");
    fiber.await_settled().await.expect("settled");
    let err = runtime
        .search(&request("q"), None)
        .await
        .expect_err("unavailable");
    assert_eq!(error_code(&err), "WEB_PROVIDER_CONFIGURED_UNAVAILABLE");
}
