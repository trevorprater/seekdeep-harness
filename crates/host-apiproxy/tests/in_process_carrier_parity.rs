//! In-process fetch/SSE carrier behavior mirrored from `fetch-carrier.spec.ts`.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::{FutureExt as _, StreamExt as _, future::BoxFuture, stream};
use parking_lot::Mutex;
use seekdeep_client_connection::{HttpRequest, HttpResponse, RpcResult};
use seekdeep_core::session::SessionId;
use seekdeep_host_apiproxy::{
    ApiDownlinkStream, ApiProxyHandler, ApiProxyRuntime, ClientResponse, FetchBody, FetchHandler,
    FetchResponse, InProcessApiClient, RpcId, RpcMethod, RpcReceipt, RpcRequest, RpcResponse,
    api::{
        events::{HostFrame, MuxFrame},
        host::EmptyRequest,
        sessions::SessionListRequest,
    },
};
use seekdeep_llm::AbortSignal;
use serde_json::{Value, json};

#[derive(Default)]
struct CarrierApi {
    unary_calls: Mutex<Vec<RpcMethod>>,
    respond_calls: AtomicUsize,
    mux_opens: AtomicUsize,
    host_opens: AtomicUsize,
}

impl ApiProxyRuntime for CarrierApi {
    fn unary(
        &self,
        method: RpcMethod,
        request: RpcRequest,
        _signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcResponse>> {
        self.unary_calls.lock().push(method);
        async move {
            if method == RpcMethod::HostPickDirectory {
                tokio::time::sleep(Duration::from_millis(35)).await;
            }
            let value = match method {
                RpcMethod::SessionList => json!({ "items": [] }),
                RpcMethod::HostPickDirectory => json!({ "path": "/tmp/slow" }),
                RpcMethod::HostDescribe => json!({
                    "version": "1",
                    "cwd": "/tmp",
                    "attachedSessions": 0,
                    "canOpenPath": true,
                }),
                _ => request.payload,
            };
            Ok(RpcResponse::new(
                request.rpc_id,
                RpcResult::Success { value: Some(value) },
            ))
        }
        .boxed()
    }

    fn respond(
        &self,
        _message: ClientResponse,
        _signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcReceipt>> {
        self.respond_calls.fetch_add(1, Ordering::AcqRel);
        async { Ok(RpcReceipt::Accepted) }.boxed()
    }

    fn mux(&self, request: RpcRequest, _signal: AbortSignal) -> ApiDownlinkStream<MuxFrame> {
        self.mux_opens.fetch_add(1, Ordering::AcqRel);
        stream::iter([
            Ok(RpcRequest::new(
                request.rpc_id,
                MuxFrame::SessionSubscribed {
                    session_id: SessionId::new("s1"),
                    last_seq: 0,
                },
            )),
            Err(anyhow::anyhow!("stream source died")),
        ])
        .boxed()
    }

    fn host(&self, request: RpcRequest, _signal: AbortSignal) -> ApiDownlinkStream<HostFrame> {
        self.host_opens.fetch_add(1, Ordering::AcqRel);
        stream::once(async move {
            Ok(RpcRequest::new(
                request.rpc_id,
                HostFrame::SessionRemoved {
                    session_id: SessionId::new("gone"),
                },
            ))
        })
        .boxed()
    }

    fn session_log(
        &self,
        _query: seekdeep_host_apiproxy::api::downloads::SessionLogQuery,
        _signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<HttpResponse>> {
        async { anyhow::bail!("not used") }.boxed()
    }
}

#[tokio::test]
async fn full_typed_client_round_trips_without_network_and_responds() {
    let api = Arc::new(CarrierApi::default());
    let client = InProcessApiClient::new(ApiProxyHandler::new(api.clone()));

    let response = client
        .sessions
        .list(
            SessionListRequest {
                cursor: Some("next".to_owned()),
            },
            None,
        )
        .await
        .unwrap();
    assert!(matches!(
        response.result,
        RpcResult::Success {
            value: Some(ref value)
        } if value.items.is_empty()
    ));
    assert_eq!(api.unary_calls.lock().as_slice(), &[RpcMethod::SessionList]);

    let receipt = client
        .respond(
            ClientResponse::new(
                RpcId::new("known"),
                RpcResult::Success {
                    value: Some(Value::Null),
                },
            ),
            None,
        )
        .await
        .unwrap();
    assert_eq!(receipt, RpcReceipt::Accepted);
    assert_eq!(api.respond_calls.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn sse_streams_are_lazy_signal_open_and_convert_impl_failure_to_terminal_frame() {
    let api = Arc::new(CarrierApi::default());
    let client = InProcessApiClient::new(ApiProxyHandler::new(api.clone()));
    let opened = Arc::new(AtomicUsize::new(0));
    let on_open = {
        let opened = opened.clone();
        Arc::new(move || {
            opened.fetch_add(1, Ordering::AcqRel);
        })
    };
    let mut frames = client
        .events
        .mux(EmptyRequest {}, AbortSignal::default(), Some(on_open));
    assert_eq!(api.mux_opens.load(Ordering::Acquire), 0);
    assert_eq!(opened.load(Ordering::Acquire), 0);

    let first = frames.next().await.unwrap().unwrap();
    assert!(matches!(
        first.payload,
        MuxFrame::SessionSubscribed { last_seq: 0, .. }
    ));
    assert_eq!(api.mux_opens.load(Ordering::Acquire), 1);
    assert_eq!(opened.load(Ordering::Acquire), 1);

    let failure = frames.next().await.unwrap().unwrap();
    assert!(matches!(failure.payload, MuxFrame::StreamError { .. }));
    assert!(frames.next().await.is_none());
}

#[tokio::test]
async fn user_paced_picker_ignores_default_deadline_but_keeps_typed_result() {
    let api = Arc::new(CarrierApi::default());
    let client =
        InProcessApiClient::with_timeout(ApiProxyHandler::new(api), Duration::from_millis(5));
    let response = client
        .host
        .pick_directory(EmptyRequest {}, None)
        .await
        .unwrap();
    assert!(matches!(
        response.result,
        RpcResult::Success {
            value: Some(ref value)
        } if value.path.as_deref() == Some("/tmp/slow")
    ));
}

struct RawFetch {
    responder: Arc<dyn Fn(HttpRequest) -> FetchResponse + Send + Sync>,
}

impl FetchHandler for RawFetch {
    fn fetch_request(
        &self,
        request: HttpRequest,
    ) -> BoxFuture<'static, anyhow::Result<FetchResponse>> {
        let response = (self.responder)(request);
        async move { Ok(response) }.boxed()
    }
}

fn complete(status: u16, body: &Value) -> FetchResponse {
    FetchResponse {
        status,
        headers: HashMap::new(),
        body: FetchBody::Complete(serde_json::to_vec(body).unwrap()),
    }
}

#[tokio::test]
async fn injected_fetch_preserves_transport_status_and_rpc_id_checks() {
    let unavailable = Arc::new(RawFetch {
        responder: Arc::new(|_| FetchResponse {
            status: 503,
            headers: HashMap::new(),
            body: FetchBody::Complete(b"down".to_vec()),
        }),
    });
    let error = InProcessApiClient::new(unavailable)
        .sessions
        .list(SessionListRequest::default(), None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("HTTP 503"));

    let liar = Arc::new(RawFetch {
        responder: Arc::new(|_| {
            complete(
                200,
                &json!({
                    "type": "server-response",
                    "rpcId": "someone-else",
                    "result": { "ok": true, "value": { "items": [] } },
                }),
            )
        }),
    });
    let error = InProcessApiClient::new(liar)
        .sessions
        .list(SessionListRequest::default(), None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("rpcId mismatch"));
}

#[tokio::test]
async fn sse_decoder_handles_chunk_boundaries_and_skips_malformed_frames() {
    let fetch = Arc::new(RawFetch {
        responder: Arc::new(|request| {
            assert_eq!(request.path, "/api/events.mux");
            let valid = serde_json::to_string(&json!({
                "type": "server-request",
                "rpcId": "f0",
                "method": "session/subscribed",
                "payload": {
                    "type": "session/subscribed",
                    "sessionId": "s-unicode-λ",
                    "lastSeq": -1,
                },
            }))
            .unwrap();
            let bytes = format!(": connected\n\ndata: not-json\n\ndata: {valid}\n\n").into_bytes();
            let split = bytes.len() - 7;
            let chunks = vec![
                Ok(bytes[..11].to_vec()),
                Ok(bytes[11..split].to_vec()),
                Ok(bytes[split..].to_vec()),
            ];
            FetchResponse {
                status: 200,
                headers: [("content-type".to_owned(), "text/event-stream".to_owned())]
                    .into_iter()
                    .collect(),
                body: FetchBody::Stream(stream::iter(chunks).boxed()),
            }
        }),
    });
    let client = InProcessApiClient::new(fetch);
    let frames = client
        .events
        .mux(EmptyRequest {}, AbortSignal::default(), None)
        .collect::<Vec<_>>()
        .await;
    assert_eq!(frames.len(), 1);
    let frame = frames.into_iter().next().unwrap().unwrap();
    assert_eq!(frame.rpc_id, RpcId::new("f0"));
    assert!(matches!(
        frame.payload,
        MuxFrame::SessionSubscribed { session_id, last_seq: -1 }
            if session_id.as_str() == "s-unicode-λ"
    ));
}

#[tokio::test]
async fn envelope_observation_sees_full_request_response_and_unsubscribe_stops_it() {
    let api = Arc::new(CarrierApi::default());
    let client = InProcessApiClient::new(ApiProxyHandler::new(api));
    let observed = Arc::new(Mutex::new(Vec::<Value>::new()));
    let subscription = client.subscribe_envelopes({
        let observed = observed.clone();
        Arc::new(move |batch| observed.lock().extend_from_slice(batch))
    });
    client
        .sessions
        .list(SessionListRequest::default(), None)
        .await
        .unwrap();
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    assert_eq!(
        observed
            .lock()
            .iter()
            .map(|value| value["type"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        ["client-request", "server-response"]
    );

    subscription.dispose();
    client
        .sessions
        .list(SessionListRequest::default(), None)
        .await
        .unwrap();
    tokio::task::yield_now().await;
    assert_eq!(observed.lock().len(), 2);
}
