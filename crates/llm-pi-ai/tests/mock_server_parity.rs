//! Shared scripted HTTP/SSE fixture parity with the pinned source helper.

#[path = "support/mock_server.rs"]
mod mock_server;

use std::{collections::HashMap, time::Duration};

use mock_server::{Behavior, MockServer};
use serde_json::json;

#[tokio::test]
async fn captures_zstd_json_paths_headers_delays_errors_and_response_close_counts() {
    let server = MockServer::start(vec![
        Behavior {
            events: vec!["one".to_owned(), "two".to_owned()],
            delay: Some(Duration::from_millis(5)),
            ..Behavior::default()
        },
        Behavior {
            status: Some(429),
            body: Some(r#"{"error":{"message":"limited"}}"#.to_owned()),
            headers: HashMap::from([("retry-after".to_owned(), "3".to_owned())]),
            ..Behavior::default()
        },
        Behavior {
            events: vec!["first".to_owned(), "late".to_owned()],
            delay: Some(Duration::from_millis(100)),
            ..Behavior::default()
        },
    ])
    .await;
    let body = json!({"model":"fixture","input":[1]});
    let compressed =
        zstd::stream::encode_all(serde_json::to_vec(&body).unwrap().as_slice(), 3).unwrap();
    let started = tokio::time::Instant::now();
    let first = reqwest::Client::new()
        .post(format!("{}/v1/responses?alt=sse", server.url))
        .header("authorization", "Bearer fixture")
        .header("content-type", "application/json")
        .header("content-encoding", "zstd")
        .body(compressed)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 200);
    assert_eq!(first.text().await.unwrap(), "data: one\n\ndata: two\n\n");
    assert!(started.elapsed() >= Duration::from_millis(5));
    server.wait_for_closed(1).await;
    let captured = server.requests();
    assert_eq!(captured[0].path, "/v1/responses?alt=sse");
    assert_eq!(captured[0].body, Some(body));
    assert_eq!(captured[0].headers["authorization"], "Bearer fixture");
    assert_eq!(server.closed_responses(), 1);

    let second = reqwest::Client::new()
        .post(format!("{}/v1/responses", server.url))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 429);
    assert_eq!(second.headers()["retry-after"], "3");
    assert_eq!(
        second.text().await.unwrap(),
        r#"{"error":{"message":"limited"}}"#
    );
    server.wait_for_closed(2).await;
    assert_eq!(server.closed_responses(), 2);

    let cancelled = reqwest::Client::new()
        .post(format!("{}/v1/responses", server.url))
        .send()
        .await
        .unwrap();
    drop(cancelled);
    tokio::time::timeout(Duration::from_secs(1), server.wait_for_closed(3))
        .await
        .unwrap();
    assert_eq!(server.closed_responses(), 3);
}
