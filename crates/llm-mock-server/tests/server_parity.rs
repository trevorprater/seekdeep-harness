//! HTTP/SSE behavior, validation, capture, and deterministic random parity.

use std::sync::Arc;

use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_llm_mock_server::{
    ConcreteMockLlmBehavior as Concrete, MockLlmBehavior as Behavior, MockLlmRequestOutcome,
    MockLlmServer, MockLlmServerEvent, MockLlmServerOptions, start_mock_llm_server,
};
use serde_json::json;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

fn options(sequence: Vec<Behavior>) -> MockLlmServerOptions {
    MockLlmServerOptions {
        sequence,
        ..MockLlmServerOptions::default()
    }
}

async fn chat(
    server: &MockLlmServer,
    path: &str,
    key: Option<&str>,
    body: Option<&str>,
) -> reqwest::Result<reqwest::Response> {
    let mut request = reqwest::Client::new()
        .post(format!("{}{}", server.base_url, path))
        .header("content-type", "application/json");
    if let Some(key) = key {
        request = request.bearer_auth(key);
    }
    if let Some(body) = body {
        request = request.body(body.to_owned());
    }
    request.send().await
}

async fn ordinary_chat(server: &MockLlmServer) -> reqwest::Result<reqwest::Response> {
    chat(
        server,
        "/v1/chat/completions",
        None,
        Some(r#"{"model":"mock","messages":[],"stream":true}"#),
    )
    .await
}

async fn wait_for_outcome(
    server: &MockLlmServer,
    expected: MockLlmRequestOutcome,
) -> seekdeep_llm_mock_server::MockLlmRequestRecord {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Some(record) = server.requests().last()
                && record.outcome == Some(expected)
            {
                return record.clone();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("mock outcome timeout")
}

#[tokio::test]
async fn complete_text_captures_request_chunks_and_observational_events() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed = events.clone();
    let mut configured = options(vec![Behavior::Success]);
    configured.api_key = Some("mock-key".to_owned());
    configured.success_text = Some("recovered".to_owned());
    configured.chunk_size = Some(3.0);
    configured.on_event = Some(Arc::new(move |event| observed.lock().push(event)));
    let server = start_mock_llm_server(configured).await.unwrap();
    let response = chat(
        &server,
        "/v1/chat/completions",
        Some("mock-key"),
        Some(r#"{"model":"mock","messages":[],"stream":true}"#),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("text/event-stream")
    );
    let body = response.text().await.unwrap();
    for marker in [
        r#""content":"rec""#,
        r#""content":"ove""#,
        r#""content":"red""#,
        r#""finish_reason":"stop""#,
        "data: [DONE]",
    ] {
        assert!(body.contains(marker), "missing {marker}: {body}");
    }
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].attempt, 1);
    assert_eq!(requests[0].behavior, "success");
    assert_eq!(requests[0].path, "/v1/chat/completions");
    assert_eq!(
        requests[0].body,
        Some(json!({"model":"mock","messages":[],"stream":true}))
    );
    assert_eq!(requests[0].chunks_sent, 5);
    assert_eq!(requests[0].outcome, Some(MockLlmRequestOutcome::Completed));
    assert!(matches!(
        events.lock().as_slice(),
        [
            MockLlmServerEvent::Request { .. },
            MockLlmServerEvent::Result { .. }
        ]
    ));
    server.close().await.unwrap();
    server.close().await.unwrap();
}

#[tokio::test]
async fn clean_eof_and_malformed_variants_preserve_their_exact_sse_shapes() {
    for (behavior, chunks, marker, done) in [
        (Behavior::EmptyBody, 0, "", false),
        (Behavior::StreamEof, 1, r#""role":"assistant""#, false),
        (Behavior::PartialEof, 1, "discarded partial response", false),
        (Behavior::MalformedJson, 2, "data: {not-json", true),
        (Behavior::MalformedEvent, 2, r#""choices":[null]"#, true),
    ] {
        let mut configured = options(vec![behavior]);
        configured.chunk_size = Some(100.0);
        let server = start_mock_llm_server(configured).await.unwrap();
        let response = ordinary_chat(&server).await.unwrap();
        let body = response.text().await.unwrap();
        assert!(body.contains(marker));
        assert_eq!(body.contains("[DONE]"), done);
        let record = &server.requests()[0];
        assert_eq!(record.chunks_sent, chunks);
        assert_eq!(record.outcome, Some(MockLlmRequestOutcome::Completed));
        server.close().await.unwrap();
    }
}

#[tokio::test]
async fn disconnect_stall_and_client_close_boundaries_are_real() {
    let reset = start_mock_llm_server(options(vec![Behavior::ConnectionReset]))
        .await
        .unwrap();
    assert!(ordinary_chat(&reset).await.is_err());
    wait_for_outcome(&reset, MockLlmRequestOutcome::Reset).await;
    reset.close().await.unwrap();

    for behavior in [Behavior::StreamDisconnect, Behavior::PartialDisconnect] {
        let mut configured = options(vec![behavior]);
        configured.partial_text = Some("half".to_owned());
        configured.disconnect_delay_ms = Some(5.0);
        configured.chunk_delay_ms = Some(0.0);
        let server = start_mock_llm_server(configured).await.unwrap();
        let response = ordinary_chat(&server).await.unwrap();
        assert!(response.text().await.is_err());
        let record = wait_for_outcome(&server, MockLlmRequestOutcome::Reset).await;
        assert_eq!(
            record.chunks_sent,
            usize::from(behavior == Behavior::PartialDisconnect)
        );
        server.close().await.unwrap();
    }

    let stall = start_mock_llm_server(options(vec![Behavior::Stall]))
        .await
        .unwrap();
    let response = ordinary_chat(&stall).await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        stall.requests()[0].outcome,
        Some(MockLlmRequestOutcome::Stalled)
    );
    drop(response);
    stall.close().await.unwrap();

    let mut configured = options(vec![Behavior::SlowSuccess]);
    configured.success_text = Some("slow".to_owned());
    configured.chunk_size = Some(1.0);
    configured.chunk_delay_ms = Some(100.0);
    let slow = start_mock_llm_server(configured).await.unwrap();
    let response = ordinary_chat(&slow).await.unwrap();
    drop(response);
    wait_for_outcome(&slow, MockLlmRequestOutcome::ClientClosed).await;
    slow.close().await.unwrap();

    for behavior in [Behavior::StreamDisconnect, Behavior::PartialDisconnect] {
        let mut configured = options(vec![behavior]);
        configured.partial_text = Some("half".to_owned());
        configured.chunk_size = Some(1.0);
        configured.chunk_delay_ms = Some(100.0);
        configured.disconnect_delay_ms = Some(100.0);
        let server = start_mock_llm_server(configured).await.unwrap();
        let response = ordinary_chat(&server).await.unwrap();
        drop(response);
        wait_for_outcome(&server, MockLlmRequestOutcome::ClientClosed).await;
        server.close().await.unwrap();
    }
}

#[tokio::test]
async fn reasoning_tools_finish_reasons_timing_and_content_type_are_exact() {
    let mut configured = options(vec![
        Behavior::ReasoningSuccess,
        Behavior::ToolCallSuccess,
        Behavior::MaxTokens,
        Behavior::SlowSuccess,
        Behavior::WrongContentType,
    ]);
    configured.success_text = Some("answer".to_owned());
    configured.reasoning_text = Some("think".to_owned());
    configured.tool_name = Some("lookup".to_owned());
    configured.tool_arguments = Some(r#"{"id":7}"#.to_owned());
    configured.chunk_delay_ms = Some(1.0);
    configured.chunk_size = Some(2.0);
    let server = start_mock_llm_server(configured).await.unwrap();
    let mut bodies = Vec::new();
    let mut types = Vec::new();
    for _ in 0..5 {
        let response = ordinary_chat(&server).await.unwrap();
        types.push(
            response
                .headers()
                .get("content-type")
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned(),
        );
        bodies.push(response.text().await.unwrap());
    }
    assert!(bodies[0].contains(r#""reasoning_content":"th""#));
    assert!(bodies[1].contains(r#""name":"lookup""#));
    assert!(bodies[1].contains(r#""finish_reason":"tool_calls""#));
    assert!(bodies[2].contains(r#""finish_reason":"length""#));
    assert!(bodies[3].contains(r#""finish_reason":"stop""#));
    assert_eq!(types[4], "application/json");
    assert!(
        server
            .requests()
            .iter()
            .all(|record| record.outcome == Some(MockLlmRequestOutcome::Completed))
    );
    server.close().await.unwrap();
}

#[tokio::test]
async fn structured_http_errors_carry_status_retry_delay_and_request_id() {
    for (behavior, status, marker) in [
        (Behavior::RateLimit, 429, "mock rate limit"),
        (Behavior::ServerError, 500, "mock server error"),
        (
            Behavior::ServiceUnavailable,
            503,
            "mock service unavailable",
        ),
        (Behavior::AuthError, 401, "mock authentication failed"),
        (Behavior::InvalidRequest, 400, "mock invalid request"),
        (Behavior::ContextOverflow, 400, "context_length_exceeded"),
        (Behavior::QuotaExceeded, 429, "insufficient_quota"),
    ] {
        let mut configured = options(vec![behavior]);
        configured.retry_after_ms = Some(1_001.0);
        configured.request_id = Some("mock-request-1".to_owned());
        let server = start_mock_llm_server(configured).await.unwrap();
        let response = ordinary_chat(&server).await.unwrap();
        assert_eq!(response.status(), status);
        assert_eq!(
            response.headers().get("x-request-id").unwrap(),
            "mock-request-1"
        );
        if behavior == Behavior::RateLimit {
            assert_eq!(response.headers().get("retry-after").unwrap(), "2");
        } else {
            assert!(response.headers().get("retry-after").is_none());
        }
        assert!(response.text().await.unwrap().contains(marker));
        assert_eq!(
            server.requests()[0].outcome,
            Some(MockLlmRequestOutcome::Completed)
        );
        server.close().await.unwrap();
    }
}

#[tokio::test]
async fn exhaustion_repeat_and_seeded_weighted_random_are_reproducible() {
    let mut once = options(vec![Behavior::Success]);
    once.success_text = Some("once".to_owned());
    let exhausted = start_mock_llm_server(once).await.unwrap();
    ordinary_chat(&exhausted)
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let response = ordinary_chat(&exhausted).await.unwrap();
    assert_eq!(response.status(), 500);
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("mock script exhausted")
    );
    assert_eq!(
        exhausted
            .requests()
            .iter()
            .map(|record| record.behavior.as_str())
            .collect::<Vec<_>>(),
        ["success", "script_exhausted"]
    );
    exhausted.close().await.unwrap();

    let mut repeating = options(vec![Behavior::Empty]);
    repeating.repeat_last = true;
    let repeating = start_mock_llm_server(repeating).await.unwrap();
    ordinary_chat(&repeating)
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    ordinary_chat(&repeating)
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(
        repeating
            .requests()
            .iter()
            .map(|record| record.behavior.as_str())
            .collect::<Vec<_>>(),
        ["empty", "empty"]
    );
    repeating.close().await.unwrap();

    let weighted = IndexMap::from([(Concrete::Success, 1.0), (Concrete::Empty, 1.0)]);
    let mut first_options = options(vec![Behavior::Random]);
    first_options.repeat_last = true;
    first_options.random_seed = Some(42.0);
    first_options.random_weights = Some(weighted.clone());
    let second_options = first_options.clone();
    let first = start_mock_llm_server(first_options).await.unwrap();
    let second = start_mock_llm_server(second_options).await.unwrap();
    for _ in 0..12 {
        ordinary_chat(&first).await.unwrap().text().await.unwrap();
        ordinary_chat(&second).await.unwrap().text().await.unwrap();
    }
    let first_choices = first
        .requests()
        .iter()
        .map(|record| record.behavior.clone())
        .collect::<Vec<_>>();
    assert_eq!(first.random_seed, 42);
    assert_eq!(
        first_choices,
        second
            .requests()
            .iter()
            .map(|record| record.behavior.clone())
            .collect::<Vec<_>>()
    );
    assert!(first_choices.contains(&"success".to_owned()));
    assert!(first_choices.contains(&"empty".to_owned()));
    assert!(
        first
            .requests()
            .iter()
            .all(|record| record.script_behavior == "random")
    );
    first.close().await.unwrap();
    second.close().await.unwrap();
}

#[tokio::test]
async fn invalid_http_requests_do_not_consume_the_script_and_root_path_is_supported() {
    let mut configured = options(vec![Behavior::Success]);
    configured.api_key = Some("expected".to_owned());
    configured.on_event = Some(Arc::new(|_| panic!("observer failure is contained")));
    let server = start_mock_llm_server(configured).await.unwrap();
    let client = reqwest::Client::new();
    let method = client
        .get(format!("{}/v1/chat/completions", server.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(method.status(), 405);
    assert_eq!(method.headers().get("allow").unwrap(), "POST");
    assert_eq!(
        client
            .post(format!("{}/v1/other", server.base_url))
            .body("{}")
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        chat(&server, "/v1/chat/completions", Some("wrong"), Some("{}"))
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        chat(&server, "/v1/chat/completions", Some("expected"), Some("{"))
            .await
            .unwrap()
            .status(),
        400
    );
    assert!(server.requests().is_empty());
    let response = chat(&server, "/chat/completions", Some("expected"), None)
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.text().await.unwrap();
    assert_eq!(server.requests()[0].path, "/chat/completions");
    assert_eq!(server.requests()[0].body, None);
    server.close().await.unwrap();
}

#[tokio::test]
async fn chunked_request_body_preserves_utf8_code_points_split_across_chunks() {
    let server = start_mock_llm_server(options(vec![Behavior::Success]))
        .await
        .unwrap();
    let encoded = serde_json::to_vec(&json!({
        "messages":[{"role":"user","content":"你好"}]
    }))
    .unwrap();
    let point = encoded
        .windows("你".len())
        .position(|window| window == "你".as_bytes())
        .unwrap()
        + 1;
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", server.port))
        .await
        .unwrap();
    stream
        .write_all(
            concat!(
                "POST /v1/chat/completions HTTP/1.1\r\n",
                "host: localhost\r\n",
                "content-type: application/json\r\n",
                "transfer-encoding: chunked\r\n",
                "connection: close\r\n\r\n",
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    for chunk in [&encoded[..point], &encoded[point..]] {
        stream
            .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
            .await
            .unwrap();
        stream.write_all(chunk).await.unwrap();
        stream.write_all(b"\r\n").await.unwrap();
    }
    stream.write_all(b"0\r\n\r\n").await.unwrap();
    stream.shutdown().await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 200"));
    assert_eq!(
        server.requests()[0].body,
        Some(json!({"messages":[{"role":"user","content":"你好"}]}))
    );
    server.close().await.unwrap();
}

#[tokio::test]
async fn ipv6_listener_formats_a_valid_base_url_when_available() {
    let mut configured = options(vec![Behavior::Success]);
    configured.host = Some("::1".to_owned());
    let Ok(server) = start_mock_llm_server(configured).await else {
        return;
    };
    assert!(server.base_url.starts_with("http://[::1]:"));
    assert_eq!(ordinary_chat(&server).await.unwrap().status(), 200);
    server.close().await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn rejects_every_invalid_option_family() {
    let cases = [
        (MockLlmServerOptions::default(), "sequence"),
        (
            {
                let mut o = options(vec![Behavior::Success]);
                o.host = Some(String::new());
                o
            },
            "host",
        ),
        (
            {
                let mut o = options(vec![Behavior::Success]);
                o.port = Some(-1.0);
                o
            },
            "port",
        ),
        (
            {
                let mut o = options(vec![Behavior::Success]);
                o.port = Some(65_536.0);
                o
            },
            "port",
        ),
        (
            {
                let mut o = options(vec![Behavior::Success]);
                o.api_key = Some(String::new());
                o
            },
            "apiKey",
        ),
        (
            {
                let mut o = options(vec![Behavior::Success]);
                o.success_text = Some(String::new());
                o
            },
            "successText",
        ),
        (
            {
                let mut o = options(vec![Behavior::Success]);
                o.partial_text = Some(String::new());
                o
            },
            "partialText",
        ),
        (
            {
                let mut o = options(vec![Behavior::Success]);
                o.reasoning_text = Some(String::new());
                o
            },
            "reasoningText",
        ),
        (
            {
                let mut o = options(vec![Behavior::Success]);
                o.chunk_size = Some(0.0);
                o
            },
            "chunkSize",
        ),
        (
            {
                let mut o = options(vec![Behavior::Success]);
                o.chunk_delay_ms = Some(-1.0);
                o
            },
            "chunkDelayMs",
        ),
        (
            {
                let mut o = options(vec![Behavior::Success]);
                o.disconnect_delay_ms = Some(f64::INFINITY);
                o
            },
            "disconnectDelayMs",
        ),
        (
            {
                let mut o = options(vec![Behavior::Success]);
                o.retry_after_ms = Some(0.0);
                o
            },
            "retryAfterMs",
        ),
        (
            {
                let mut o = options(vec![Behavior::Success]);
                o.request_id = Some(String::new());
                o
            },
            "requestId",
        ),
        (
            {
                let mut o = options(vec![Behavior::Success]);
                o.tool_name = Some(String::new());
                o
            },
            "toolName",
        ),
        (
            {
                let mut o = options(vec![Behavior::Success]);
                o.tool_arguments = Some("{".to_owned());
                o
            },
            "toolArguments",
        ),
        (
            {
                let mut o = options(vec![Behavior::Random]);
                o.random_seed = Some(-1.0);
                o
            },
            "randomSeed",
        ),
        (
            {
                let mut o = options(vec![Behavior::Random]);
                o.random_weights = Some(IndexMap::from([(Concrete::Success, -1.0)]));
                o
            },
            "non-negative",
        ),
        (
            {
                let mut o = options(vec![Behavior::Random]);
                o.random_weights = Some(IndexMap::from([(Concrete::Success, 0.0)]));
                o
            },
            "positive weight",
        ),
    ];
    for (configured, marker) in cases {
        let error = start_mock_llm_server(configured).await.unwrap_err();
        assert!(error.to_string().contains(marker), "{error:#}");
    }
}

#[test]
fn request_body_and_event_types_are_json_round_trip_safe() {
    let event = MockLlmServerEvent::Result {
        attempt: 1,
        script_behavior: "success".to_owned(),
        behavior: "success".to_owned(),
        outcome: MockLlmRequestOutcome::Completed,
        chunks_sent: 3,
    };
    let encoded = serde_json::to_value(&event).unwrap();
    assert_eq!(encoded["type"], "result");
    assert_eq!(encoded["scriptBehavior"], "success");
    assert_eq!(encoded["chunksSent"], 3);
    assert!(encoded.get("script_behavior").is_none());
    assert_eq!(
        serde_json::from_value::<MockLlmServerEvent>(encoded).unwrap(),
        event
    );
}
