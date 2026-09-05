//! Behavioral mirror of the pinned SDK line-transport guarantees.

use std::sync::Arc;

use futures::FutureExt as _;
use parking_lot::Mutex;
use seekdeep_sdk_protocol::{JsonRpcLineTransport, JsonRpcResponseError};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, DuplexStream};

fn object(value: Value) -> Map<String, Value> {
    let Value::Object(value) = value else {
        panic!("object")
    };
    value
}

fn pair() -> (Arc<JsonRpcLineTransport>, Arc<JsonRpcLineTransport>) {
    let (left, right) = tokio::io::duplex(64 * 1024);
    let (left_read, left_write) = tokio::io::split(left);
    let (right_read, right_write) = tokio::io::split(right);
    (
        JsonRpcLineTransport::new(left_read, left_write),
        JsonRpcLineTransport::new(right_read, right_write),
    )
}

fn transport_and_peer() -> (Arc<JsonRpcLineTransport>, DuplexStream) {
    let (transport, peer) = tokio::io::duplex(64 * 1024);
    let (read, write) = tokio::io::split(transport);
    (JsonRpcLineTransport::new(read, write), peer)
}

async fn peer_frame(peer: &mut BufReader<DuplexStream>) -> Value {
    let mut line = String::new();
    tokio::time::timeout(std::time::Duration::from_secs(2), peer.read_line(&mut line))
        .await
        .expect("frame timeout")
        .expect("read frame");
    serde_json::from_str(line.trim()).expect("JSON frame")
}

#[tokio::test]
async fn supports_bidirectional_requests_notifications_and_remote_errors() {
    let (left, right) = pair();
    left.on_request(Arc::new(|method, params| {
        async move {
            anyhow::ensure!(method == "echo", "unexpected method");
            Ok(json!({"echoed": params}))
        }
        .boxed()
    }));
    let notifications = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&notifications);
    right.on_notification(Arc::new(move |method, params| {
        observed.lock().push((method, params));
    }));
    left.start();
    right.start();

    assert_eq!(
        right
            .request("echo", object(json!({"value":42})), None)
            .await
            .unwrap(),
        json!({"echoed":{"value":42}})
    );
    left.notify(
        "session.status",
        Some(object(json!({"sessionId":"main", "status":"idle"}))),
    )
    .await
    .unwrap();
    left.notify("heartbeat", None).await.unwrap();
    tokio::task::yield_now().await;
    assert_eq!(
        *notifications.lock(),
        [
            (
                "session.status".to_owned(),
                object(json!({"sessionId":"main", "status":"idle"}))
            ),
            ("heartbeat".to_owned(), Map::new())
        ]
    );

    left.on_request(Arc::new(|_, _| {
        async { anyhow::bail!("handler boom") }.boxed()
    }));
    let failure = right
        .request("explode", Map::new(), None)
        .await
        .expect_err("remote failure");
    let response = failure
        .downcast_ref::<JsonRpcResponseError>()
        .expect("typed response error");
    assert_eq!(response.code, Some(-32603));
    assert_eq!(response.message, "handler boom");
    assert_eq!(response.data, None);
    left.close();
    right.close();
}

#[tokio::test]
async fn preabort_and_pending_abort_remove_all_correlation_state() {
    let (transport, _peer) = transport_and_peer();
    transport.start();
    let preaborted = seekdeep_llm::AbortSignal::default();
    preaborted.abort_with_reason(json!("already gone"));
    let error = transport
        .request("never-sent", Map::new(), Some(preaborted))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("already gone"));
    assert_eq!(transport.pending_len(), 0);

    let signal = seekdeep_llm::AbortSignal::default();
    let pending = transport.request("never-answered", Map::new(), Some(signal.clone()));
    tokio::pin!(pending);
    tokio::select! {
        biased;
        result = &mut pending => panic!("request settled before abort: {result:?}"),
        () = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
    }
    signal.abort_with_reason(json!("plain-string-reason"));
    assert!(
        pending
            .await
            .unwrap_err()
            .to_string()
            .contains("JSON-RPC request aborted: plain-string-reason")
    );
    assert_eq!(transport.pending_len(), 0);
    transport.close();
}

#[tokio::test]
async fn correlated_abort_emits_the_requested_protocol_cancellation_frame() {
    let (transport, peer) = transport_and_peer();
    transport.start();
    let signal = seekdeep_llm::AbortSignal::default();
    let request_signal = signal.clone();
    let request = {
        let transport = Arc::clone(&transport);
        tokio::spawn(async move {
            transport
                .request_with_cancellation(
                    "tools/call",
                    object(json!({"name":"slow","arguments":{}})),
                    request_signal,
                    "notifications/cancelled",
                )
                .await
        })
    };
    let mut peer = BufReader::new(peer);
    let sent = peer_frame(&mut peer).await;
    signal.abort_with_reason(json!("user"));
    let cancelled = peer_frame(&mut peer).await;
    assert_eq!(cancelled["method"], "notifications/cancelled");
    assert_eq!(cancelled["params"]["requestId"], sent["id"]);
    assert_eq!(cancelled["params"]["reason"], "request cancelled");
    assert!(
        request
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("aborted")
    );
    assert_eq!(transport.pending_len(), 0);
    transport.close();
}

#[tokio::test]
async fn preserves_structured_error_data_and_fallback_error_message() {
    let (transport, peer) = transport_and_peer();
    transport.start();
    let mut peer = BufReader::new(peer);

    let request = {
        let transport = Arc::clone(&transport);
        tokio::spawn(async move {
            transport
                .request("remote-error-data", Map::new(), None)
                .await
        })
    };
    let frame = peer_frame(&mut peer).await;
    let id = frame["id"].clone();
    peer.get_mut()
        .write_all(
            format!(
                "{}\n",
                json!({"jsonrpc":"2.0", "id":id, "error":{"code":7,"message":"structured","data":{"detail":"x"}}})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let error = request.await.unwrap().unwrap_err();
    let response = error.downcast_ref::<JsonRpcResponseError>().unwrap();
    assert_eq!(response.code, Some(7));
    assert_eq!(response.message, "structured");
    assert_eq!(response.data, Some(json!({"detail":"x"})));

    let request = {
        let transport = Arc::clone(&transport);
        tokio::spawn(async move { transport.request("remote-error", Map::new(), None).await })
    };
    let frame = peer_frame(&mut peer).await;
    peer.get_mut()
        .write_all(
            format!(
                "{}\n",
                json!({"jsonrpc":"2.0", "id":frame["id"], "error":{}})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    assert_eq!(
        request.await.unwrap().unwrap_err().to_string(),
        "JSON-RPC error"
    );
    transport.close();
}

#[tokio::test]
async fn normalizes_params_ignores_malformed_frames_and_preserves_split_utf8() {
    let (transport, mut peer) = transport_and_peer();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let request_log = Arc::clone(&requests);
    transport.on_request(Arc::new(move |method, params| {
        request_log.lock().push((method, params));
        async { Ok(json!({"ok":true})) }.boxed()
    }));
    let notifications = Arc::new(Mutex::new(Vec::new()));
    let notification_log = Arc::clone(&notifications);
    transport.on_notification(Arc::new(move |method, params| {
        notification_log.lock().push((method, params));
    }));
    transport.start();

    peer.write_all(b"not json\n\nnull\n{\"jsonrpc\":\"2.0\",\"params\":{}}\n")
        .await
        .unwrap();
    peer.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"array-params\",\"params\":[]}\n")
        .await
        .unwrap();
    peer.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"tick\"}\n")
        .await
        .unwrap();
    let frame = serde_json::to_vec(&json!({
        "jsonrpc":"2.0", "method":"message", "params":{"text":"你好"}
    }))
    .unwrap();
    let needle = "你".as_bytes();
    let at = frame
        .windows(needle.len())
        .position(|window| window == needle)
        .unwrap();
    peer.write_all(&frame[..=at]).await.unwrap();
    peer.write_all(&frame[at + 1..]).await.unwrap();
    peer.write_all(b"\n").await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    assert_eq!(*requests.lock(), [("array-params".to_owned(), Map::new())]);
    assert_eq!(
        *notifications.lock(),
        [
            ("tick".to_owned(), Map::new()),
            ("message".to_owned(), object(json!({"text":"你好"})))
        ]
    );
    transport.close();
}

#[tokio::test]
async fn method_not_found_input_end_close_and_unknown_responses_are_fail_closed() {
    let (left, right) = pair();
    left.start();
    right.start();
    let error = right
        .request("missing", Map::new(), None)
        .await
        .unwrap_err();
    let response = error.downcast_ref::<JsonRpcResponseError>().unwrap();
    assert_eq!(response.code, Some(-32601));
    assert_eq!(response.message, "method not found: missing");
    left.close();
    right.close();

    let (transport, mut peer) = transport_and_peer();
    transport.start();
    peer.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"unknown\",\"result\":1}\n")
        .await
        .unwrap();
    tokio::task::yield_now().await;
    let pending = {
        let transport = Arc::clone(&transport);
        tokio::spawn(async move { transport.request("never-replies", Map::new(), None).await })
    };
    let mut line = String::new();
    BufReader::new(&mut peer)
        .read_line(&mut line)
        .await
        .unwrap();
    drop(peer);
    assert!(
        pending
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("input closed")
    );

    let (transport, _peer) = transport_and_peer();
    let pending = {
        let transport = Arc::clone(&transport);
        tokio::spawn(async move { transport.request("never-replies", Map::new(), None).await })
    };
    while transport.pending_len() == 0 {
        tokio::task::yield_now().await;
    }
    transport.close();
    assert!(
        pending
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("transport closed")
    );
}

#[test]
fn named_wire_types_preserve_exact_method_and_field_spellings() {
    let request = seekdeep_sdk_protocol::HarnessSdkRequest::Initialize(
        seekdeep_sdk_protocol::InitializeParams {
            cwd: "/workspace".to_owned(),
            provider: seekdeep_llm::ProviderId::new("deepseek"),
            model: seekdeep_llm::ModelId::new("model"),
            max_tokens: Some(100),
        },
    );
    assert_eq!(
        serde_json::to_value(request).unwrap(),
        json!({
            "method":"initialize",
            "params":{
                "cwd":"/workspace",
                "provider":"deepseek",
                "model":"model",
                "maxTokens":100
            }
        })
    );
}
