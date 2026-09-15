//! Parity mirror of the source exa.spec.ts provider suite.

mod support;

use std::sync::Arc;

use seekdeep_cordis::Context;
use seekdeep_llm::{AbortSignal, HarnessError};
use seekdeep_util::launch_environment::{
    LaunchEnvironmentLayerInput, LaunchEnvironmentSource, SEEKDEEP_LAUNCH_ENVIRONMENT,
    create_launch_environment_snapshot,
};
use seekdeep_web::{WebRuntime, WebRuntimeConfig, WebSearchProvider, WebSearchRequest};
use seekdeep_web_search_exa::{
    EXA_PROVIDER_ID, ExaConfig, ExaResult, ExaSearchProvider, ExaSearchProviderOptions,
    ExaSearchResponse, SearchType, install, map_exa_response, map_exa_result, resolve_options,
};
use serde_json::{Value, json};
use support::{MockServer, ResponseSpec};

fn options(api_key: &str, base_url: &str) -> ExaSearchProviderOptions {
    ExaSearchProviderOptions {
        api_key: api_key.to_owned(),
        base_url: base_url.to_owned(),
        search_type: SearchType::Auto,
        num_results: None,
        highlights_per_result: 1.0,
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

fn exa_result(value: Value) -> ExaResult {
    serde_json::from_value(value).expect("fixture parses")
}

fn exa_response(value: Value) -> ExaSearchResponse {
    serde_json::from_value(value).expect("fixture parses")
}

#[test]
fn maps_a_full_result_entry() {
    let source = map_exa_result(&exa_result(json!({
        "url": "https://a.test",
        "title": "A",
        "publishedDate": "2026-01-01",
        "highlights": ["salient sentence", "second"],
    })))
    .expect("mapped");
    assert_eq!(
        source,
        seekdeep_web::WebSearchSource {
            url: "https://a.test".to_owned(),
            title: Some("A".to_owned()),
            snippet: Some("salient sentence".to_owned()),
            published_at: Some("2026-01-01".to_owned()),
        }
    );
}

#[test]
fn drops_a_result_with_no_usable_highlight() {
    assert!(
        map_exa_result(&exa_result(
            json!({"url": "https://a.test", "highlights": []})
        ))
        .is_none()
    );
    assert!(map_exa_result(&exa_result(json!({"url": "https://a.test"}))).is_none());
    assert!(
        map_exa_result(&exa_result(
            json!({"url": "https://a.test", "highlights": ["  "]})
        ))
        .is_none()
    );
}

#[test]
fn omits_null_empty_optional_fields() {
    assert_eq!(
        map_exa_result(&exa_result(json!({
            "url": "https://a.test",
            "title": null,
            "publishedDate": null,
            "highlights": ["hi"]
        }))),
        Some(seekdeep_web::WebSearchSource {
            url: "https://a.test".to_owned(),
            title: None,
            snippet: Some("hi".to_owned()),
            published_at: None,
        })
    );
    assert_eq!(
        map_exa_result(&exa_result(json!({
            "url": "https://a.test",
            "title": "",
            "publishedDate": "",
            "highlights": ["hi"]
        }))),
        Some(seekdeep_web::WebSearchSource {
            url: "https://a.test".to_owned(),
            title: None,
            snippet: Some("hi".to_owned()),
            published_at: None,
        })
    );
}

#[test]
fn maps_response_with_no_content_and_filtered_sources() {
    let result = map_exa_response(&exa_response(json!({
        "results": [
            { "url": "https://a.test", "highlights": ["one"] },
            { "url": "https://b.test" },
            { "url": "https://c.test", "title": "C", "highlights": ["three"] },
        ],
    })));
    assert_eq!(
        result,
        seekdeep_web::WebSearchResult {
            content: None,
            sources: vec![
                seekdeep_web::WebSearchSource {
                    url: "https://a.test".to_owned(),
                    title: None,
                    snippet: Some("one".to_owned()),
                    published_at: None,
                },
                seekdeep_web::WebSearchSource {
                    url: "https://c.test".to_owned(),
                    title: Some("C".to_owned()),
                    snippet: Some("three".to_owned()),
                    published_at: None,
                },
            ],
            truncated: false,
        }
    );
}

#[test]
fn tolerates_a_missing_results_array() {
    assert!(
        map_exa_response(&exa_response(json!({})))
            .sources
            .is_empty()
    );
}

#[test]
fn availability_gates_key_base_url_and_limits() {
    assert!(!ExaSearchProvider::new(options("", "https://api.exa.test")).available());
    assert!(ExaSearchProvider::new(options("exa-key", "https://api.exa.test")).available());
    assert!(!ExaSearchProvider::new(options("exa-key", "not a url")).available());

    let mut zero_highlights = options("exa-key", "https://api.exa.test");
    zero_highlights.highlights_per_result = 0.0;
    assert!(!ExaSearchProvider::new(zero_highlights).available());

    let mut fractional_highlights = options("exa-key", "https://api.exa.test");
    fractional_highlights.highlights_per_result = 1.5;
    assert!(!ExaSearchProvider::new(fractional_highlights).available());

    let mut negative_num_results = options("exa-key", "https://api.exa.test");
    negative_num_results.num_results = Some(-1.0);
    assert!(!ExaSearchProvider::new(negative_num_results).available());
}

#[tokio::test]
async fn sends_query_type_highlights_num_results_and_bearer_auth() {
    let server = MockServer::start(ResponseSpec::json(200, json!({"results": []}))).await;
    let mut o = options("exa-key", &server.url);
    o.search_type = SearchType::Neural;
    o.highlights_per_result = 3.0;
    let provider = ExaSearchProvider::new(o);
    let result = provider
        .search(
            &WebSearchRequest {
                query: "hello".to_owned(),
                max_results: Some(5),
            },
            None,
        )
        .await
        .expect("search");
    assert!(!result.truncated);

    let requests = server.take_requests();
    assert_eq!(requests.len(), 1);
    let captured = &requests[0];
    assert_eq!(captured.method, "POST");
    assert_eq!(captured.path, "/search");
    assert_eq!(
        captured.headers.get("authorization").map(String::as_str),
        Some("Bearer exa-key")
    );
    assert_eq!(
        captured.headers.get("content-type").map(String::as_str),
        Some("application/json")
    );
    let body: Value = serde_json::from_slice(&captured.body).expect("json body");
    assert_eq!(
        body,
        json!({
            "query": "hello",
            "type": "neural",
            "contents": { "highlights": { "highlightsPerUrl": 3.0 } },
            "numResults": 5,
        })
    );
}

#[tokio::test]
async fn falls_back_to_configured_num_results_when_request_omits_max_results() {
    let server = MockServer::start(ResponseSpec::json(200, json!({"results": []}))).await;
    let mut o = options("exa-key", &server.url);
    o.num_results = Some(7.0);
    let provider = ExaSearchProvider::new(o);
    provider.search(&request("q"), None).await.expect("search");
    let requests = server.take_requests();
    let body: Value = serde_json::from_slice(&requests[0].body).expect("json body");
    assert_eq!(body["numResults"], json!(7.0));
}

#[tokio::test]
async fn request_max_results_wins_over_configured_num_results() {
    let server = MockServer::start(ResponseSpec::json(200, json!({"results": []}))).await;
    let mut o = options("exa-key", &server.url);
    o.num_results = Some(7.0);
    let provider = ExaSearchProvider::new(o);
    provider
        .search(
            &WebSearchRequest {
                query: "q".to_owned(),
                max_results: Some(2),
            },
            None,
        )
        .await
        .expect("search");
    let requests = server.take_requests();
    let body: Value = serde_json::from_slice(&requests[0].body).expect("json body");
    assert_eq!(body["numResults"], json!(2));
}

#[tokio::test]
async fn omits_num_results_when_neither_configured_nor_requested() {
    let server = MockServer::start(ResponseSpec::json(200, json!({"results": []}))).await;
    let provider = ExaSearchProvider::new(options("exa-key", &server.url));
    provider.search(&request("q"), None).await.expect("search");
    let requests = server.take_requests();
    let body: Value = serde_json::from_slice(&requests[0].body).expect("json body");
    assert!(body.get("numResults").is_none());
}

async fn search_error(response: ResponseSpec) -> anyhow::Error {
    let server = MockServer::start(response).await;
    let provider = ExaSearchProvider::new(options("exa-key", &server.url));
    provider
        .search(&request("q"), None)
        .await
        .expect_err("error")
}

#[tokio::test]
async fn maps_http_error_to_provider_message() {
    let err = search_error(ResponseSpec::json(401, json!({"error": "bad key"}))).await;
    assert_eq!(error_code(&err), "WEB_PROVIDER_ERROR");
    assert_eq!(err.to_string(), "bad key");
}

#[tokio::test]
async fn keeps_status_line_when_error_body_not_json() {
    let err = search_error(ResponseSpec::plain(502, "gateway down")).await;
    assert_eq!(error_code(&err), "WEB_PROVIDER_ERROR");
    assert_eq!(err.to_string(), "Exa API error (HTTP 502)");
}

#[tokio::test]
async fn keeps_status_line_when_json_error_body_carries_no_detail() {
    let err = search_error(ResponseSpec::json(500, json!({}))).await;
    assert_eq!(error_code(&err), "WEB_PROVIDER_ERROR");
    assert_eq!(err.to_string(), "Exa API error (HTTP 500)");
}

#[tokio::test]
async fn maps_network_failure() {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind");
    let address = listener.local_addr().expect("addr");
    drop(listener);
    let provider = ExaSearchProvider::new(options("exa-key", &format!("http://{address}")));
    let err = provider
        .search(&request("q"), None)
        .await
        .expect_err("network");
    assert_eq!(error_code(&err), "WEB_PROVIDER_ERROR");
}

#[tokio::test]
async fn maps_abort_to_web_aborted() {
    let server = MockServer::start(ResponseSpec::json(200, json!({"results": []}))).await;
    let provider = ExaSearchProvider::new(options("exa-key", &server.url));
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
async fn maps_wrong_shape_to_provider_error() {
    let err = search_error(ResponseSpec::json(200, json!({"results": {}}))).await;
    assert_eq!(error_code(&err), "WEB_PROVIDER_ERROR");
}

#[tokio::test]
async fn resolve_options_defaults_and_env_fallback() {
    let context = Context::new();
    let snapshot = create_launch_environment_snapshot(&[LaunchEnvironmentLayerInput {
        source: LaunchEnvironmentSource::Process,
        path: None,
        values: [("EXA_API_KEY".to_owned(), "env-key".to_owned())]
            .into_iter()
            .collect(),
    }]);
    context
        .provide(SEEKDEEP_LAUNCH_ENVIRONMENT, Arc::new(snapshot))
        .expect("provide env");
    let options = resolve_options(&context, &ExaConfig::default());
    assert_eq!(options.api_key, "env-key");
    assert_eq!(options.base_url, "https://api.exa.ai");
    assert_eq!(options.search_type, SearchType::Auto);
    assert!((options.highlights_per_result - 1.0).abs() < f64::EPSILON);
    assert_eq!(options.num_results, None);
}

#[tokio::test]
async fn plugin_registration_and_unregistration() {
    let server = MockServer::start(ResponseSpec::json(200, json!({"results": []}))).await;
    let context = Context::new();
    let runtime = WebRuntime::new(
        &context,
        &WebRuntimeConfig {
            search_provider: Some(EXA_PROVIDER_ID.to_owned()),
            fetch_provider: None,
        },
    )
    .expect("web runtime");
    let config = ExaConfig {
        api_key: Some("exa-key".to_owned()),
        base_url: Some(server.url.clone()),
        ..ExaConfig::default()
    };
    let fiber = install(&context, config).expect("install");
    fiber.await_settled().await.expect("settled");
    let result = runtime.search(&request("q"), None).await.expect("search");
    assert!(!result.truncated);
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
    let server = MockServer::start(ResponseSpec::json(200, json!({"results": []}))).await;
    let context = Context::new();
    let runtime = WebRuntime::new(
        &context,
        &WebRuntimeConfig {
            search_provider: Some(EXA_PROVIDER_ID.to_owned()),
            fetch_provider: None,
        },
    )
    .expect("web runtime");
    let config = ExaConfig {
        api_key: Some("exa-key".to_owned()),
        base_url: Some(server.url.clone()),
        search_type: Some(SearchType::Keyword),
        num_results: Some(9.0),
        highlights_per_result: Some(2.0),
    };
    let fiber = install(&context, config).expect("install");
    fiber.await_settled().await.expect("settled");
    runtime.search(&request("q"), None).await.expect("search");
    let requests = server.take_requests();
    let body: Value = serde_json::from_slice(&requests[0].body).expect("json body");
    assert_eq!(body["type"], json!("keyword"));
    assert_eq!(
        body["contents"]["highlights"]["highlightsPerUrl"],
        json!(2.0)
    );
    assert_eq!(body["numResults"], json!(9.0));
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
            search_provider: Some(EXA_PROVIDER_ID.to_owned()),
            fetch_provider: None,
        },
    )
    .expect("web runtime");
    let fiber = install(&context, ExaConfig::default()).expect("install");
    fiber.await_settled().await.expect("settled");
    let err = runtime
        .search(&request("q"), None)
        .await
        .expect_err("unavailable");
    assert_eq!(error_code(&err), "WEB_PROVIDER_CONFIGURED_UNAVAILABLE");
}
