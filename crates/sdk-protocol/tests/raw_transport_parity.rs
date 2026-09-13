//! Raw JSON-RPC payloads retain ECMAScript strings through real byte streams.

use std::sync::Arc;

use futures::FutureExt as _;
use parking_lot::Mutex;
use seekdeep_sdk_protocol::{
    HarnessSdkNotification, HarnessSdkRequest, JsonRpcLineTransport, JsonRpcRawResponseError,
    JsonRpcResponseError, JsonString, JsonValue,
};
use serde_json::{Map, Value, json};
use tokio::{
    io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, DuplexStream},
    sync::oneshot,
};

fn raw(value: &str) -> JsonValue {
    JsonValue::parse(value.to_owned()).unwrap()
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

fn transport_and_peer() -> (Arc<JsonRpcLineTransport>, BufReader<DuplexStream>) {
    let (transport, peer) = tokio::io::duplex(64 * 1024);
    let (read, write) = tokio::io::split(transport);
    (JsonRpcLineTransport::new(read, write), BufReader::new(peer))
}

async fn frame(peer: &mut BufReader<DuplexStream>) -> JsonValue {
    let mut line = String::new();
    let read = tokio::time::timeout(std::time::Duration::from_secs(2), peer.read_line(&mut line))
        .await
        .expect("frame timeout")
        .unwrap();
    assert!(read > 0, "unexpected EOF");
    assert!(!line.contains("$serde_json::private"));
    raw(line.trim())
}

#[tokio::test]
async fn raw_requests_and_notifications_keep_surrogates_keys_and_json_types() {
    let (left, right) = pair();
    left.on_request_json(Arc::new(|method, params| {
        assert_eq!(method, "echo");
        async move { Ok(params) }.boxed()
    }));
    let (sent, received) = oneshot::channel();
    let sent = Mutex::new(Some(sent));
    right.on_notification_json(Arc::new(move |method, params| {
        assert_eq!(method, "session.event");
        sent.lock().take().unwrap().send(params).unwrap();
    }));
    left.start();
    right.start();
    let payload = raw(
        r#"{"\ud800":["\udfff",{"text":"\ud800x\udfff","literal":"\\ud800"}],"null":null,"number":7,"bool":true}"#,
    );
    let result = right
        .request_json("echo", payload.clone(), None)
        .await
        .unwrap();
    assert_eq!(result, payload);
    assert!(result.clone().try_into_serde_json().is_err());
    left.notify_json("session.event", Some(payload.clone()))
        .await
        .unwrap();
    assert_eq!(received.await.unwrap(), payload);
    assert_eq!(right.pending_len(), 0);
    left.close();
    right.close();
}

#[tokio::test]
async fn incoming_raw_ids_unknown_keys_and_params_normalization_match_the_source() {
    let (transport, mut peer) = transport_and_peer();
    transport.on_request_json(Arc::new(|_, params| async move { Ok(params) }.boxed()));
    transport.start();
    peer.get_mut()
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"\\ud800\",\"method\":\"echo\",\"params\":{\"\\udfff\":\"\\ud800\"},\"\\udfff\":\"ignored\"}\n")
        .await.unwrap();
    let response = frame(&mut peer).await;
    assert_eq!(response.get("id").unwrap().to_utf16(), Some(vec![0xd800]));
    assert_eq!(
        response.get("result").unwrap().to_owned(),
        raw(r#"{"\udfff":"\ud800"}"#)
    );
    for params in ["null", "[]", "false", r#""\ud800""#] {
        let request = format!("{{\"id\":7,\"method\":\"echo\",\"params\":{params}}}\n");
        peer.get_mut().write_all(request.as_bytes()).await.unwrap();
        assert_eq!(
            frame(&mut peer).await.get("result").unwrap().to_owned(),
            raw("{}")
        );
    }
    transport.close();
}

#[tokio::test]
async fn raw_error_message_and_data_survive_while_legacy_errors_stay_checked() {
    let (left, right) = pair();
    left.on_request_json(Arc::new(|_, _| {
        async {
            Err(JsonRpcRawResponseError {
                code: Some(-32001),
                message: JsonString::from_utf16(&[0xd800, 0x61, 0xdfff]),
                data: Some(raw(r#"{"\udfff":"\ud800"}"#)),
            }
            .into())
        }
        .boxed()
    }));
    left.start();
    right.start();
    let error = right
        .request_json("failure", raw("{}"), None)
        .await
        .unwrap_err();
    let response = error.downcast_ref::<JsonRpcRawResponseError>().unwrap();
    assert_eq!(response.code, Some(-32001));
    assert_eq!(response.message.to_utf16(), [0xd800, 0x61, 0xdfff]);
    assert_eq!(response.data, Some(raw(r#"{"\udfff":"\ud800"}"#)));
    let rejected = right
        .request("failure", Map::new(), None)
        .await
        .unwrap_err();
    assert!(rejected.to_string().contains("cannot be represented"));
    assert!(rejected.downcast_ref::<JsonRpcRawResponseError>().is_some());
    assert!(rejected.downcast_ref::<JsonRpcResponseError>().is_none());
    let checked = JsonRpcRawResponseError {
        code: Some(7),
        message: "ordinary".into(),
        data: Some(raw(r#"{"detail":true}"#)),
    }
    .try_into_legacy()
    .unwrap();
    assert_eq!(checked.message, "ordinary");
    assert_eq!(checked.data, Some(json!({"detail":true})));
    let rejected_data = JsonRpcRawResponseError {
        code: Some(7),
        message: "ordinary".into(),
        data: Some(raw(r#""\ud800""#)),
    }
    .try_into_legacy()
    .unwrap_err();
    assert!(
        rejected_data
            .to_string()
            .contains("error data cannot be represented")
    );
    left.close();
    right.close();
}

#[tokio::test]
async fn ordinary_result_and_handler_adapters_reject_unrepresentable_payloads() {
    let (left, right) = pair();
    left.on_request_json(Arc::new(|_, _| async { Ok(raw(r#""\ud800""#)) }.boxed()));
    left.start();
    right.start();
    assert!(
        right
            .request("result", Map::new(), None)
            .await
            .unwrap_err()
            .to_string()
            .contains("result cannot be represented")
    );
    left.on_request(Arc::new(|_, _| {
        panic!("unrepresentable parameters reached handler")
    }));
    let error = right
        .request_json("request", raw(r#"{"text":"\ud800"}"#), None)
        .await
        .unwrap_err();
    let error = error.downcast_ref::<JsonRpcRawResponseError>().unwrap();
    assert_eq!(error.code, Some(-32603));
    assert!(
        error
            .message
            .as_str()
            .unwrap()
            .contains("parameters cannot be represented")
    );
    let (sent, received) = oneshot::channel();
    let sent = Mutex::new(Some(sent));
    left.on_input_failure(Arc::new(move |error| {
        if let Some(sent) = sent.lock().take() {
            sent.send(error.to_string()).unwrap();
        }
    }));
    left.on_notification(Arc::new(|_, _| {
        panic!("unrepresentable notification reached observer")
    }));
    right
        .notify_json("event", Some(raw(r#"{"text":"\ud800"}"#)))
        .await
        .unwrap();
    assert!(
        received
            .await
            .unwrap()
            .contains("parameters cannot be represented")
    );
    left.close();
    right.close();
}

#[tokio::test]
async fn raw_cancellation_keeps_payload_and_correlated_control_frames() {
    let (transport, mut peer) = transport_and_peer();
    transport.start();
    let signal = seekdeep_llm::AbortSignal::default();
    let request_signal = signal.clone();
    let request = {
        let transport = transport.clone();
        tokio::spawn(async move {
            transport
                .request_json_with_cancellation(
                    "tools/call",
                    raw(r#"{"arguments":{"text":"\ud800"}}"#),
                    request_signal,
                    "notifications/cancelled",
                )
                .await
        })
    };
    let sent = frame(&mut peer).await;
    assert_eq!(
        sent.pointer("/params/arguments/text").unwrap().to_utf16(),
        Some(vec![0xd800])
    );
    signal.abort_with_reason(Value::String("stop".to_owned()));
    let cancellation = frame(&mut peer).await;
    assert_eq!(
        cancellation
            .pointer("/params/requestId")
            .unwrap()
            .to_owned(),
        sent.get("id").unwrap().to_owned()
    );
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

#[test]
fn typed_sdk_envelopes_decode_raw_payloads_in_either_member_order() {
    let request: HarnessSdkRequest = serde_json::from_str(r#"{"params":{"sessionId":"main","contentBlocks":[{"type":"text","text":"\ud800x\udfff"}]},"method":"session/prompt","\udfff":0}"#).unwrap();
    let HarnessSdkRequest::SessionPrompt(params) = &request else {
        panic!("prompt")
    };
    let seekdeep_llm::ContentBlock::Text { text } = &params.content_blocks[0] else {
        panic!("text")
    };
    assert_eq!(text.to_utf16(), [0xd800, 0x78, 0xdfff]);
    let encoded = JsonValue::from_serialize(&request).unwrap();
    assert_eq!(
        encoded
            .pointer("/params/contentBlocks/0/text")
            .unwrap()
            .to_utf16(),
        Some(vec![0xd800, 0x78, 0xdfff])
    );
    let notification: HarnessSdkNotification = serde_json::from_str(r#"{"params":{"sessionId":"main","event":{"type":"plugin/event","seq":1,"time":0,"data":{"\udfff":"\ud800"}}},"method":"session.event"}"#).unwrap();
    let HarnessSdkNotification::SessionEvent(event) = &notification else {
        panic!("event")
    };
    assert_eq!(event.event.data, raw(r#"{"\udfff":"\ud800"}"#));
    let encoded = JsonValue::from_serialize(&notification).unwrap();
    assert_eq!(
        encoded.pointer("/params/event/data").unwrap().to_owned(),
        event.event.data
    );
    assert!(matches!(
        serde_json::from_value::<HarnessSdkRequest>(json!({"method":"shutdown"})).unwrap(),
        HarnessSdkRequest::Shutdown
    ));
    assert!(
        serde_json::from_str::<HarnessSdkRequest>(r#"{"method":"shutdown","params":{}}"#).is_err()
    );
}

#[tokio::test]
async fn lossless_transport_interoperates_with_the_pinned_source_endpoint() {
    let snapshot = include_str!("../../../SOURCE_SNAPSHOT");
    let source = snapshot
        .lines()
        .find_map(|line| line.strip_prefix("repository="))
        .unwrap();
    let expected = snapshot
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .unwrap();
    let head = tokio::process::Command::new("git")
        .args(["-C", source, "rev-parse", "HEAD"])
        .output()
        .await
        .unwrap();
    assert!(head.status.success());
    assert_eq!(String::from_utf8(head.stdout).unwrap().trim(), expected);
    let script = r"
const { JsonRpcLineTransport } = await import(process.argv[1]);
const rpc = new JsonRpcLineTransport(process.stdin, process.stdout);
rpc.onRequest(async (method, params) => {
  if (method === 'failure') throw new Error(String.fromCharCode(0xd800) + ' source failure');
  rpc.notify('mirrored', params);
  return params;
});
rpc.start();
";
    let mut child = tokio::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--no-warnings",
            "--input-type=module",
            "-e",
            script,
        ])
        .arg(format!("{source}/packages/sdk/protocol/src/transport.ts"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let transport =
        JsonRpcLineTransport::new(child.stdout.take().unwrap(), child.stdin.take().unwrap());
    let (sent, received) = oneshot::channel();
    let sent = Mutex::new(Some(sent));
    transport.on_notification_json(Arc::new(move |method, params| {
        assert_eq!(method, "mirrored");
        sent.lock().take().unwrap().send(params).unwrap();
    }));
    transport.start();
    let payload = raw(r#"{"\ud800":{"text":"\udfff","literal":"\\ud800"},"pair":"😀"}"#);
    let returned = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        transport.request_json("echo", payload.clone(), None),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(returned, payload);
    assert_eq!(received.await.unwrap(), payload);
    let error = transport
        .request_json("failure", raw("{}"), None)
        .await
        .unwrap_err();
    let error = error.downcast_ref::<JsonRpcRawResponseError>().unwrap();
    assert_eq!(error.code, Some(-32603));
    assert_eq!(error.message.utf16_units()[0], 0xd800);
    assert!(error.message.ends_with(" source failure"));
    transport.shutdown_output().await.unwrap();
    let output = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    transport.close();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
