//! Parity mirror of the source fetch-http.spec.ts suite.

mod support;

use std::sync::Arc;

use seekdeep_cordis::Context;
use seekdeep_llm::HarnessError;
use seekdeep_web::{
    WebFetchBody, WebFetchProvider, WebFetchRequest, WebFetchResult, WebRuntime, WebRuntimeConfig,
};
use seekdeep_web_fetch_http::{
    HttpFetchConfig, HttpFetchLimits, HttpFetchProvider, LOCAL_FETCH_PROVIDER_ID,
    classify_content_type, decoder_for_charset, install, is_same_origin, parse_charset,
    validate_fetch_url,
};
use support::{CapturedRequest, MockResponse, MockServer, ResponseSpec};
use url::Url;

fn default_limits() -> HttpFetchLimits {
    HttpFetchLimits {
        max_url_length: 2048.0,
        max_response_bytes: 5_000_000.0,
        max_body_chars: 100_000.0,
        timeout_ms: 5_000.0,
        max_redirects: 5,
        user_agent: "test-agent/1.0".to_owned(),
    }
}

fn provider(limits: HttpFetchLimits) -> HttpFetchProvider {
    HttpFetchProvider::new(limits)
}

fn req(url: &str) -> WebFetchRequest {
    WebFetchRequest { url: url.to_owned() }
}

fn code(error: &anyhow::Error) -> &str {
    error
        .downcast_ref::<HarnessError>()
        .map_or("?", HarnessError::code)
}

fn body_content(result: &WebFetchResult) -> &str {
    match &result.body {
        WebFetchBody::Html { content } | WebFetchBody::Text { content } => content,
    }
}

fn respond(status: u16, content_type: &str, body: &str) -> MockResponse {
    MockResponse::Respond(ResponseSpec::plain(status, content_type, body.as_bytes().to_vec()))
}

#[test]
fn policy_validates_scheme_credentials_and_length() {
    let url = validate_fetch_url("https://example.com/x", 2048.0).expect("valid");
    assert_eq!(url.host_str(), Some("example.com"));
    assert_eq!(code(&validate_fetch_url("ftp://example.com", 2048.0).expect_err("ftp")), "WEB_INVALID_URL");
    assert_eq!(code(&validate_fetch_url("not a url", 2048.0).expect_err("bad")), "WEB_INVALID_URL");
    assert_eq!(code(&validate_fetch_url("https://user:pass@example.com", 2048.0).expect_err("creds")), "WEB_BLOCKED_URL");
    let long = format!("https://example.com/{}", "a".repeat(3000));
    assert_eq!(code(&validate_fetch_url(&long, 2048.0).expect_err("long")), "WEB_INVALID_URL");
}

#[test]
fn policy_classifies_content_types() {
    assert_eq!(classify_content_type(Some("text/html; charset=utf-8")), Some(seekdeep_web_fetch_http::FetchableKind::Html));
    assert_eq!(classify_content_type(Some("application/xhtml+xml")), Some(seekdeep_web_fetch_http::FetchableKind::Html));
    assert_eq!(classify_content_type(Some("text/plain")), Some(seekdeep_web_fetch_http::FetchableKind::Text));
    assert_eq!(classify_content_type(Some("application/json")), Some(seekdeep_web_fetch_http::FetchableKind::Text));
    assert_eq!(classify_content_type(Some("image/png")), None);
    assert_eq!(classify_content_type(None), None);
}

#[test]
fn policy_compares_origins() {
    assert!(is_same_origin(
        &Url::parse("https://a.com/x").unwrap(),
        &Url::parse("https://a.com/y").unwrap()
    ));
    assert!(!is_same_origin(
        &Url::parse("https://a.com").unwrap(),
        &Url::parse("https://b.com").unwrap()
    ));
    assert!(!is_same_origin(
        &Url::parse("http://a.com").unwrap(),
        &Url::parse("https://a.com").unwrap()
    ));
}

#[test]
fn policy_parses_charset() {
    assert_eq!(parse_charset(Some("text/html; charset=UTF-8")).as_deref(), Some("utf-8"));
    assert_eq!(parse_charset(Some("text/plain; charset=\"iso-8859-1\"")).as_deref(), Some("iso-8859-1"));
    assert_eq!(parse_charset(Some("text/plain")), None);
    assert_eq!(parse_charset(None), None);
}

#[test]
fn policy_builds_decoder_and_defaults_to_utf8() {
    assert_eq!(decoder_for_charset(None).expect("utf8"), encoding_rs::UTF_8);
    assert_eq!(decoder_for_charset(Some("iso-8859-1")).expect("latin"), encoding_rs::WINDOWS_1252);
    let err = decoder_for_charset(Some("not-a-charset")).expect_err("bad charset");
    assert_eq!(code(&err), "WEB_UNSUPPORTED_CONTENT_TYPE");
}

#[tokio::test]
async fn fetches_text_and_html_bodies() {
    let server = MockServer::start(Arc::new(|req: &CapturedRequest| {
        if req.path == "/html" {
            respond(200, "text/html", "<h1>hi</h1>")
        } else {
            respond(200, "text/plain", "hello world")
        }
    }))
    .await;
    assert!(provider(default_limits()).available());
    let text = provider(default_limits())
        .fetch(&req(&server.url), None)
        .await
        .expect("text");
    assert_eq!(text.status_code, 200);
    assert!(matches!(text.body, WebFetchBody::Text { .. }));
    assert_eq!(body_content(&text), "hello world");
    assert!(!text.truncated);

    let html = provider(default_limits())
        .fetch(&req(&format!("{}/html", server.url)), None)
        .await
        .expect("html");
    assert!(matches!(html.body, WebFetchBody::Html { .. }));
    assert_eq!(body_content(&html), "<h1>hi</h1>");
}

#[tokio::test]
async fn sends_configured_user_agent_and_returns_non_2xx() {
    let server = MockServer::start(Arc::new(|_req: &CapturedRequest| {
        respond(404, "text/plain", "nope")
    }))
    .await;
    let result = provider(default_limits())
        .fetch(&req(&server.url), None)
        .await
        .expect("404 result");
    assert_eq!(result.status_code, 404);
    assert_eq!(body_content(&result), "nope");
    let requests = server.take_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].headers.get("user-agent").map(String::as_str), Some("test-agent/1.0"));
}

#[tokio::test]
async fn rejects_over_cap_content_length() {
    let server = MockServer::start(Arc::new(|_req: &CapturedRequest| {
        MockResponse::Respond(ResponseSpec::new(
            200,
            vec![
                ("content-type".to_owned(), "text/plain".to_owned()),
                ("content-length".to_owned(), "999999".to_owned()),
            ],
            vec![],
        ))
    }))
    .await;
    let mut limits = default_limits();
    limits.max_response_bytes = 10.0;
    let err = provider(limits).fetch(&req(&server.url), None).await.expect_err("too large");
    assert_eq!(code(&err), "WEB_FETCH_TOO_LARGE");
}

#[tokio::test]
async fn truncates_stream_past_byte_cap_but_not_exact_fill() {
    let server = MockServer::start(Arc::new(|_req: &CapturedRequest| {
        respond(200, "text/plain", "abcdefghij")
    }))
    .await;
    let mut limits = default_limits();
    limits.max_response_bytes = 4.0;
    let result = provider(limits.clone())
        .fetch(&req(&server.url), None)
        .await
        .expect("truncated");
    assert_eq!(body_content(&result), "abcd");
    assert!(result.truncated);

    let server2 = MockServer::start(Arc::new(|_req: &CapturedRequest| {
        respond(200, "text/plain", "abcd")
    }))
    .await;
    let result = provider(limits)
        .fetch(&req(&server2.url), None)
        .await
        .expect("exact");
    assert_eq!(body_content(&result), "abcd");
    assert!(!result.truncated);
}

#[tokio::test]
async fn truncates_decoded_body_past_char_cap() {
    let server = MockServer::start(Arc::new(|_req: &CapturedRequest| {
        respond(200, "text/plain", "abcdefghij")
    }))
    .await;
    let mut limits = default_limits();
    limits.max_body_chars = 3.0;
    let result = provider(limits)
        .fetch(&req(&server.url), None)
        .await
        .expect("truncated");
    assert_eq!(body_content(&result), "abc");
    assert!(result.truncated);
}

#[tokio::test]
async fn rejects_unsupported_or_missing_content_type() {
    let server = MockServer::start(Arc::new(|_req: &CapturedRequest| {
        respond(200, "image/png", "binary")
    }))
    .await;
    let err = provider(default_limits()).fetch(&req(&server.url), None).await.expect_err("binary");
    assert_eq!(code(&err), "WEB_UNSUPPORTED_CONTENT_TYPE");

    let server2 = MockServer::start(Arc::new(|_req: &CapturedRequest| {
        MockResponse::Respond(ResponseSpec::new(200, vec![], b"no type".to_vec()))
    }))
    .await;
    let err = provider(default_limits()).fetch(&req(&server2.url), None).await.expect_err("none");
    assert_eq!(code(&err), "WEB_UNSUPPORTED_CONTENT_TYPE");
}

#[tokio::test]
async fn decodes_non_utf8_and_rejects_unsupported_charset() {
    let server = MockServer::start(Arc::new(|_req: &CapturedRequest| {
        MockResponse::Respond(ResponseSpec::new(
            200,
            vec![("content-type".to_owned(), "text/plain; charset=iso-8859-1".to_owned())],
            vec![0x63, 0x61, 0x66, 0xE9],
        ))
    }))
    .await;
    let result = provider(default_limits())
        .fetch(&req(&server.url), None)
        .await
        .expect("latin");
    assert_eq!(body_content(&result), "café");

    let server2 = MockServer::start(Arc::new(|_req: &CapturedRequest| {
        respond(200, "text/plain; charset=not-a-charset", "x")
    }))
    .await;
    let err = provider(default_limits()).fetch(&req(&server2.url), None).await.expect_err("charset");
    assert_eq!(code(&err), "WEB_UNSUPPORTED_CONTENT_TYPE");
}

#[tokio::test]
async fn follows_same_origin_redirects_and_blocks_cross_origin() {
    let server = MockServer::start(Arc::new(|req: &CapturedRequest| {
        if req.path == "/start" {
            MockResponse::Respond(ResponseSpec::new(
                302,
                vec![("location".to_owned(), "/end".to_owned())],
                vec![],
            ))
        } else {
            respond(200, "text/plain", "arrived")
        }
    }))
    .await;
    let result = provider(default_limits())
        .fetch(&req(&format!("{}/start", server.url)), None)
        .await
        .expect("redirect");
    assert_eq!(body_content(&result), "arrived");
    assert_eq!(result.url, format!("{}/end", server.url));

    let cross = MockServer::start(Arc::new(|_req: &CapturedRequest| {
        MockResponse::Respond(ResponseSpec::new(
            302,
            vec![("location".to_owned(), "https://example.com/".to_owned())],
            vec![],
        ))
    }))
    .await;
    let err = provider(default_limits()).fetch(&req(&cross.url), None).await.expect_err("cross");
    assert_eq!(code(&err), "WEB_REDIRECT_BLOCKED");
}

#[tokio::test]
async fn rejects_credentials_in_redirect_target() {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    let server = MockServer::start(Arc::new(move |_req: &CapturedRequest| {
        MockResponse::Respond(ResponseSpec::new(
            302,
            vec![("location".to_owned(), format!("http://user:pass@127.0.0.1:{port}/"))],
            vec![],
        ))
    }))
    .await;
    let err = provider(default_limits()).fetch(&req(&server.url), None).await.expect_err("creds");
    assert_eq!(code(&err), "WEB_BLOCKED_URL");
}

fn query_n(path: &str) -> u64 {
    Url::parse(&format!("http://x{path}"))
        .ok()
        .and_then(|url| {
            url.query_pairs()
                .find(|(key, _)| key == "n")
                .and_then(|(_, value)| value.parse().ok())
        })
        .unwrap_or(0)
}

#[tokio::test]
async fn enforces_redirect_hop_cap_exactly() {
    let redirect_server = MockServer::start(Arc::new(|req: &CapturedRequest| {
        let n = query_n(&req.path);
        if n >= 2 {
            respond(200, "text/plain", "landed")
        } else {
            MockResponse::Respond(ResponseSpec::new(
                302,
                vec![("location".to_owned(), format!("/?n={}", n + 1))],
                vec![],
            ))
        }
    }))
    .await;
    let mut limits = default_limits();
    limits.max_redirects = 2;
    let result = provider(limits.clone())
        .fetch(&req(&format!("{}/?n=0", redirect_server.url)), None)
        .await
        .expect("landed");
    assert_eq!(body_content(&result), "landed");
    assert_eq!(redirect_server.take_requests().len(), 3);

    let infinite = MockServer::start(Arc::new(|req: &CapturedRequest| {
        let n = query_n(&req.path);
        MockResponse::Respond(ResponseSpec::new(
            302,
            vec![("location".to_owned(), format!("/?n={}", n + 1))],
            vec![],
        ))
    }))
    .await;
    let err = provider(limits)
        .fetch(&req(&format!("{}/?n=0", infinite.url)), None)
        .await
        .expect_err("exceeded");
    assert_eq!(code(&err), "WEB_REDIRECT_BLOCKED");
    assert!(err.to_string().contains("exceeded the maximum of 2 redirects"));
    assert_eq!(infinite.take_requests().len(), 3);
}

#[tokio::test]
async fn reports_over_limit_before_cross_origin() {
    let server = MockServer::start(Arc::new(|req: &CapturedRequest| {
        let n = query_n(&req.path);
        let location = if n == 0 { "/?n=1".to_owned() } else { "https://example.com/".to_owned() };
        MockResponse::Respond(ResponseSpec::new(
            302,
            vec![("location".to_owned(), location)],
            vec![],
        ))
    }))
    .await;
    let mut limits = default_limits();
    limits.max_redirects = 1;
    let err = provider(limits).fetch(&req(&format!("{}/?n=0", server.url)), None).await.expect_err("exceeded");
    assert_eq!(code(&err), "WEB_REDIRECT_BLOCKED");
    assert!(err.to_string().contains("exceeded the maximum of 1 redirects"));
}

#[tokio::test]
async fn max_redirects_zero_follows_none_and_relative_redirect_resolves() {
    let server = MockServer::start(Arc::new(|req: &CapturedRequest| {
        if req.path == "/a" {
            MockResponse::Respond(ResponseSpec::new(
                301,
                vec![("location".to_owned(), "b".to_owned())],
                vec![],
            ))
        } else if req.path == "/r" {
            MockResponse::Respond(ResponseSpec::new(
                302,
                vec![("location".to_owned(), "/done".to_owned())],
                vec![],
            ))
        } else {
            respond(200, "text/plain", "landed")
        }
    }))
    .await;
    // relative same-origin redirect follows
    let result = provider(default_limits())
        .fetch(&req(&format!("{}/a", server.url)), None)
        .await
        .expect("relative");
    assert_eq!(body_content(&result), "landed");

    // maxRedirects 0 refuses a redirect
    let mut limits = default_limits();
    limits.max_redirects = 0;
    let err = provider(limits.clone())
        .fetch(&req(&format!("{}/r", server.url)), None)
        .await
        .expect_err("zero redirect");
    assert_eq!(code(&err), "WEB_REDIRECT_BLOCKED");
    let direct = provider(limits)
        .fetch(&req(&format!("{}/done", server.url)), None)
        .await
        .expect("direct");
    assert_eq!(body_content(&direct), "landed");
}

#[tokio::test]
async fn redirect_without_location_is_provider_error() {
    let server = MockServer::start(Arc::new(|_req: &CapturedRequest| {
        MockResponse::Respond(ResponseSpec::new(302, vec![], vec![]))
    }))
    .await;
    let err = provider(default_limits()).fetch(&req(&server.url), None).await.expect_err("no location");
    assert_eq!(code(&err), "WEB_PROVIDER_ERROR");
}

#[tokio::test]
async fn rejects_invalid_scheme_and_credentials_before_network() {
    let err = provider(default_limits())
        .fetch(&req("ftp://example.com"), None)
        .await
        .expect_err("ftp");
    assert_eq!(code(&err), "WEB_INVALID_URL");
    let err = provider(default_limits())
        .fetch(&req("http://user:pass@127.0.0.1/"), None)
        .await
        .expect_err("creds");
    assert_eq!(code(&err), "WEB_BLOCKED_URL");
}

#[tokio::test]
async fn honors_pre_aborted_and_in_flight_abort() {
    let server = MockServer::start(Arc::new(|_req: &CapturedRequest| MockResponse::Stall)).await;
    let signal = seekdeep_llm::AbortSignal::default();
    signal.abort();
    let err = provider(default_limits())
        .fetch(&req(&server.url), Some(signal))
        .await
        .expect_err("pre-aborted");
    assert_eq!(code(&err), "WEB_ABORTED");

    let signal = seekdeep_llm::AbortSignal::default();
    let prov = provider(default_limits());
    let url = req(&server.url);
    let fetch = prov.fetch(&url, Some(signal.clone()));
    signal.abort();
    let err = fetch.await.expect_err("in-flight");
    assert_eq!(code(&err), "WEB_ABORTED");
}

#[tokio::test]
async fn times_out_slow_response_and_mid_body_read() {
    let server = MockServer::start(Arc::new(|_req: &CapturedRequest| MockResponse::Stall)).await;
    let mut limits = default_limits();
    limits.timeout_ms = 50.0;
    let err = provider(limits.clone())
        .fetch(&req(&server.url), None)
        .await
        .expect_err("timeout");
    assert_eq!(code(&err), "WEB_FETCH_TIMEOUT");

    let server2 = MockServer::start(Arc::new(|_req: &CapturedRequest| {
        MockResponse::StallAfterPartial {
            head: ResponseSpec::new(
                200,
                vec![
                    ("content-type".to_owned(), "text/plain".to_owned()),
                    ("content-length".to_owned(), "100".to_owned()),
                ],
                vec![],
            ),
            partial: b"partial".to_vec(),
        }
    }))
    .await;
    limits.timeout_ms = 80.0;
    let err = provider(limits)
        .fetch(&req(&server2.url), None)
        .await
        .expect_err("mid-read timeout");
    assert_eq!(code(&err), "WEB_FETCH_TIMEOUT");
}

#[tokio::test]
async fn maps_connection_failure() {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let address = listener.local_addr().expect("addr");
    drop(listener);
    let err = provider(default_limits())
        .fetch(&req(&format!("http://{address}/")), None)
        .await
        .expect_err("connection");
    assert_eq!(code(&err), "WEB_PROVIDER_ERROR");
}

#[tokio::test]
async fn plugin_registers_and_unregisters_hmr_safe() {
    let server = MockServer::start(Arc::new(|_req: &CapturedRequest| {
        respond(200, "text/plain", "ok")
    }))
    .await;
    let context = Context::new();
    let runtime = WebRuntime::new(
        &context,
        &WebRuntimeConfig {
            search_provider: None,
            fetch_provider: Some(LOCAL_FETCH_PROVIDER_ID.to_owned()),
        },
    )
    .expect("web runtime");
    let config = HttpFetchConfig {
        user_agent: Some("test-agent/1.0".to_owned()),
        ..HttpFetchConfig::default()
    };
    let fiber = install(&context, config).expect("install");
    fiber.await_settled().await.expect("settled");
    let result = runtime.fetch(&req(&server.url), None).await.expect("fetch");
    assert_eq!(result.status_code, 200);
    fiber.dispose().await.expect("dispose");
    let err = runtime.fetch(&req(&server.url), None).await.expect_err("missing");
    assert_eq!(code(&err), "WEB_PROVIDER_CONFIGURED_MISSING");
}

#[tokio::test]
async fn plugin_rejects_invalid_limits_at_construction() {
    let context = Context::new();
    WebRuntime::new(
        &context,
        &WebRuntimeConfig {
            search_provider: None,
            fetch_provider: Some(LOCAL_FETCH_PROVIDER_ID.to_owned()),
        },
    )
    .expect("web runtime");

    let config = HttpFetchConfig {
        max_response_bytes: Some(-1.0),
        ..HttpFetchConfig::default()
    };
    let fiber = install(&context, config).expect("install");
    let err = fiber.await_settled().await.expect_err("invalid");
    assert!(err.to_string().contains("maxResponseBytes must be a positive finite number"));

    let config = HttpFetchConfig {
        timeout_ms: Some(0.0),
        ..HttpFetchConfig::default()
    };
    let fiber = install(&context, config).expect("install");
    let err = fiber.await_settled().await.expect_err("invalid");
    assert!(err.to_string().contains("timeoutMs must be a positive finite number"));

    let config = HttpFetchConfig {
        timeout_ms: Some(2_147_483_648.0),
        ..HttpFetchConfig::default()
    };
    let fiber = install(&context, config).expect("install");
    let err = fiber.await_settled().await.expect_err("invalid");
    assert!(err.to_string().contains("timeoutMs must be no greater than 2147483647"));

    let config = HttpFetchConfig {
        max_redirects: Some(1.5),
        ..HttpFetchConfig::default()
    };
    let fiber = install(&context, config).expect("install");
    let err = fiber.await_settled().await.expect_err("invalid");
    assert!(err.to_string().contains("maxRedirects must be a non-negative integer"));

    let config = HttpFetchConfig {
        max_redirects: Some(-1.0),
        ..HttpFetchConfig::default()
    };
    let fiber = install(&context, config).expect("install");
    let err = fiber.await_settled().await.expect_err("invalid");
    assert!(err.to_string().contains("maxRedirects must be a non-negative integer"));
}
