//! Host carrier behavior mirrored from `fetch/handler.ts`.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use futures::{FutureExt as _, StreamExt as _, stream};
use parking_lot::Mutex;
use seekdeep_client_connection::{
    ConnectionHostConfig, HOST_API_PROXY, HttpMethod, HttpRequest, HttpResponse,
    HttpResponseStream, HttpTransport, RpcResult, install_host,
};
use seekdeep_cordis::Context;
use seekdeep_host_apiproxy::{
    ApiClient, ApiDownlinkStream, ApiProxyHandler, ApiProxyRuntime, ClientResponse, RpcId,
    RpcMethod, RpcReceipt, RpcRequest, RpcResponse,
    api::{
        downloads::SessionLogQuery,
        events::{HostFrame, MuxFrame},
        host::EmptyRequest,
        sessions::SessionListRequest,
    },
    new_web_api_client,
};
use seekdeep_host_webserver::{ListenHost, WebServer, WebServerConfig};
use seekdeep_llm::AbortSignal;
use serde_json::{Value, json};

#[derive(Clone, Debug)]
struct UnaryCall {
    method: RpcMethod,
    request: RpcRequest,
    signal: AbortSignal,
}

#[derive(Default)]
struct ScriptedApi {
    calls: Mutex<Vec<UnaryCall>>,
    responses: AtomicUsize,
    downloads: Mutex<Vec<SessionLogQuery>>,
    crash: AtomicBool,
    forge_id: AtomicBool,
    stream_download: AtomicBool,
    download_consumer_signal: Mutex<Option<AbortSignal>>,
}

impl ApiProxyRuntime for ScriptedApi {
    fn unary(
        &self,
        method: RpcMethod,
        request: RpcRequest,
        signal: AbortSignal,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<RpcResponse>> {
        self.calls.lock().push(UnaryCall {
            method,
            request: request.clone(),
            signal,
        });
        let crash = self.crash.load(Ordering::Acquire);
        let forge_id = self.forge_id.load(Ordering::Acquire);
        async move {
            if crash {
                anyhow::bail!("impl exploded");
            }
            let rpc_id = if forge_id {
                RpcId::new("forged")
            } else {
                request.rpc_id
            };
            Ok(RpcResponse::new(
                rpc_id,
                RpcResult::Success {
                    value: Some(match method {
                        RpcMethod::SessionList => json!({ "items": [] }),
                        _ => request.payload,
                    }),
                },
            ))
        }
        .boxed()
    }

    fn respond(
        &self,
        _message: ClientResponse,
        _signal: AbortSignal,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<RpcReceipt>> {
        self.responses.fetch_add(1, Ordering::AcqRel);
        async { Ok(RpcReceipt::Accepted) }.boxed()
    }

    fn mux(&self, request: RpcRequest, _signal: AbortSignal) -> ApiDownlinkStream<MuxFrame> {
        stream::once(async move {
            Ok(RpcRequest::new(
                request.rpc_id,
                MuxFrame::SessionSubscribed {
                    session_id: seekdeep_core::session::SessionId::new("s1"),
                    last_seq: 0,
                },
            ))
        })
        .boxed()
    }

    fn host(&self, request: RpcRequest, _signal: AbortSignal) -> ApiDownlinkStream<HostFrame> {
        stream::once(async move {
            Ok(RpcRequest::new(
                request.rpc_id,
                HostFrame::RemoteEvent {
                    event: "commands/change".to_owned(),
                    args: Vec::new(),
                },
            ))
        })
        .boxed()
    }

    fn session_log(
        &self,
        query: SessionLogQuery,
        _signal: AbortSignal,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<HttpResponse>> {
        self.downloads.lock().push(query);
        if self.stream_download.load(Ordering::Acquire) {
            let consumer_signal = AbortSignal::default();
            *self.download_consumer_signal.lock() = Some(consumer_signal.clone());
            return async move {
                Ok(HttpResponse {
                    status: 201,
                    headers: [("content-type".to_owned(), "application/zip".to_owned())]
                        .into_iter()
                        .collect(),
                    body: Vec::new(),
                    body_stream: Some(HttpResponseStream::new(
                        stream::iter([Ok(b"zip-".to_vec()), Ok(b"stream".to_vec())]).boxed(),
                        consumer_signal,
                    )),
                })
            }
            .boxed();
        }
        async {
            Ok(HttpResponse {
                status: 201,
                headers: [("content-type".to_owned(), "application/zip".to_owned())]
                    .into_iter()
                    .collect(),
                body: b"zip-body".to_vec(),
                body_stream: None,
            })
        }
        .boxed()
    }
}

fn post(path: &str, body: &Value) -> HttpRequest {
    let mut request = HttpRequest::new(HttpMethod::Post, path);
    request
        .headers
        .insert("content-type".to_owned(), "application/json".to_owned());
    request.body = serde_json::to_vec(body).unwrap();
    request
}

fn full_request(rpc_id: &str, method: &str, payload: &Value) -> Value {
    json!({
        "type": "client-request",
        "rpcId": rpc_id,
        "method": method,
        "payload": payload,
    })
}

fn body(response: &HttpResponse) -> Value {
    serde_json::from_slice(&response.body).unwrap()
}

#[tokio::test]
async fn unary_dispatch_normalizes_payload_and_preserves_impl_echo_for_client_correlation() {
    let api = Arc::new(ScriptedApi::default());
    let handler = ApiProxyHandler::new(api.clone());
    let response = handler
        .fetch(post(
            "/api/session.list",
            &full_request(
                "r1",
                "session.list",
                &json!({ "cursor": "next", "ignored": true }),
            ),
        ))
        .await;
    assert_eq!(response.status, 200);
    assert_eq!(
        body(&response),
        json!({
            "type": "server-response",
            "rpcId": "r1",
            "result": { "ok": true, "value": { "items": [] } }
        })
    );
    {
        let calls = api.calls.lock();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, RpcMethod::SessionList);
        assert_eq!(calls[0].request.payload, json!({ "cursor": "next" }));
        assert!(!calls[0].signal.is_aborted());
    }

    api.forge_id.store(true, Ordering::Release);
    let forged = handler
        .fetch(post(
            "/api/session.list",
            &full_request("r2", "session.list", &json!({})),
        ))
        .await;
    assert_eq!(body(&forged)["rpcId"], "forged");
}

#[tokio::test]
async fn malformed_envelopes_payloads_and_path_mismatches_are_correlated_business_errors() {
    let api = Arc::new(ScriptedApi::default());
    let handler = ApiProxyHandler::new(api.clone());

    let no_id = handler
        .fetch(post("/api/session.list", &json!({ "nonsense": true })))
        .await;
    assert_eq!(body(&no_id)["rpcId"], "invalid-request");
    assert_eq!(body(&no_id)["result"]["error"]["code"], "bad-request");
    let salvaged = handler
        .fetch(post(
            "/api/session.list",
            &json!({ "rpcId": "salvage-me", "nonsense": true }),
        ))
        .await;
    assert_eq!(body(&salvaged)["rpcId"], "salvage-me");

    let mismatch = handler
        .fetch(post(
            "/api/session.list",
            &full_request("r3", "session.create", &json!({})),
        ))
        .await;
    assert!(
        body(&mismatch)["result"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("does not match path")
    );
    let invalid = handler
        .fetch(post(
            "/api/session.history",
            &full_request("r4", "session.history", &json!({ "sessionId": 123 })),
        ))
        .await;
    assert_eq!(body(&invalid)["result"]["error"]["code"], "bad-request");
    assert!(
        body(&invalid)["result"]["error"]["details"]["issues"]
            .as_array()
            .is_some_and(|issues| !issues.is_empty())
    );
    assert!(api.calls.lock().is_empty());
}

#[tokio::test]
async fn carrier_status_fence_runs_before_impl_and_crashes_are_http_500() {
    let api = Arc::new(ScriptedApi::default());
    let handler = ApiProxyHandler::new(api.clone());
    assert_eq!(
        handler.fetch(post("/api/no.such", &json!({}))).await.status,
        404
    );
    let mut wrong_media = HttpRequest::new(HttpMethod::Post, "/api/session.list");
    wrong_media
        .headers
        .insert("content-type".to_owned(), "text/plain".to_owned());
    wrong_media.body = b"{}".to_vec();
    assert_eq!(handler.fetch(wrong_media).await.status, 415);
    let mut bad_json = HttpRequest::new(HttpMethod::Post, "/api/session.list");
    bad_json
        .headers
        .insert("content-type".to_owned(), "application/json".to_owned());
    bad_json.body = b"{oops".to_vec();
    assert_eq!(handler.fetch(bad_json).await.status, 400);

    api.crash.store(true, Ordering::Release);
    let crashed = handler
        .fetch(post(
            "/api/session.list",
            &full_request("r", "session.list", &json!({})),
        ))
        .await;
    assert_eq!(crashed.status, 500);
    assert_eq!(
        String::from_utf8(crashed.body).unwrap(),
        "handler failure: impl exploded"
    );
}

#[tokio::test]
async fn respond_rejects_bad_full_forms_without_reaching_runtime() {
    let api = Arc::new(ScriptedApi::default());
    let handler = ApiProxyHandler::new(api.clone());
    let malformed = handler
        .fetch(post("/api/respond", &json!({ "type": "client-response" })))
        .await;
    assert_eq!(
        body(&malformed),
        json!({ "accepted": false, "reason": "bad-response" })
    );
    assert_eq!(api.responses.load(Ordering::Acquire), 0);

    let accepted = handler
        .fetch(post(
            "/api/respond",
            &json!({
                "type": "client-response",
                "rpcId": "answer-1",
                "result": { "ok": true, "value": { "behavior": "allow" } }
            }),
        ))
        .await;
    assert_eq!(body(&accepted), json!({ "accepted": true }));
    assert_eq!(api.responses.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn get_and_head_download_queries_transform_exact_boolean_strings() {
    let api = Arc::new(ScriptedApi::default());
    let handler = ApiProxyHandler::new(api.clone());
    let mut get = HttpRequest::new(HttpMethod::Get, "/api/session.export");
    get.query = Some("sessionId=s1&includeDescendants=true".to_owned());
    let response = handler.fetch(get).await;
    assert_eq!(response.status, 201);
    assert_eq!(response.body, b"zip-body");

    let mut head = HttpRequest::new(HttpMethod::Other("HEAD".to_owned()), "/api/session.export");
    head.query = Some("sessionId=s2&includeDescendants=false".to_owned());
    let response = handler.fetch(head).await;
    assert_eq!(response.status, 201);
    assert!(response.body.is_empty());
    assert_eq!(response.headers["content-type"], "application/zip");

    {
        let downloads = api.downloads.lock();
        assert_eq!(downloads[0].session_id.as_str(), "s1");
        assert_eq!(downloads[0].include_descendants, Some(true));
        assert_eq!(downloads[1].session_id.as_str(), "s2");
        assert_eq!(downloads[1].include_descendants, None);
    }

    let mut invalid = HttpRequest::new(HttpMethod::Get, "/api/session.export");
    invalid.query = Some("sessionId=s3&includeDescendants=1".to_owned());
    assert_eq!(handler.fetch(invalid).await.status, 400);
}

#[tokio::test]
async fn get_preserves_pull_driven_download_and_head_cancels_it_before_body_poll() {
    let api = Arc::new(ScriptedApi::default());
    api.stream_download.store(true, Ordering::Release);
    let handler = ApiProxyHandler::new(api.clone());

    let mut get = HttpRequest::new(HttpMethod::Get, "/api/session.export");
    get.query = Some("sessionId=s1".to_owned());
    let mut response = handler.fetch(get).await;
    assert!(response.body.is_empty());
    let stream = response.body_stream.as_mut().expect("streaming GET body");
    assert_eq!(stream.next().await.unwrap().unwrap(), b"zip-");
    assert_eq!(stream.next().await.unwrap().unwrap(), b"stream");
    assert!(stream.next().await.is_none());

    let mut head = HttpRequest::new(HttpMethod::Other("HEAD".to_owned()), "/api/session.export");
    head.query = Some("sessionId=s2".to_owned());
    let response = handler.fetch(head).await;
    assert!(response.body.is_empty());
    assert!(response.body_stream.is_none());
    assert!(
        api.download_consumer_signal
            .lock()
            .as_ref()
            .is_some_and(AbortSignal::is_aborted)
    );
}

#[tokio::test]
async fn contracted_client_and_handler_cross_real_http_and_both_websockets() {
    let context = Context::new();
    let server = WebServer::install(
        &context,
        WebServerConfig {
            host: ListenHost::Loopback,
            port: 0,
        },
    )
    .await
    .unwrap();
    install_host(&context, ConnectionHostConfig::default(), None).unwrap();
    let api = Arc::new(ScriptedApi::default());
    let handler = ApiProxyHandler::new(api.clone());
    let _proxy = context
        .provide(HOST_API_PROXY, handler.connection_proxy())
        .unwrap();
    let client = new_web_api_client(Some(&format!("http://127.0.0.1:{}", server.port()))).unwrap();

    let unary = client
        .call_unary(
            "session.list",
            json!({ "ignored": true }),
            AbortSignal::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        unary.result,
        RpcResult::Success {
            value: Some(json!({ "items": [] }))
        }
    );

    let typed = ApiClient::from_transport(client.clone());
    let typed_list = typed
        .sessions
        .list(
            serde_json::from_value::<SessionListRequest>(json!({})).unwrap(),
            None,
        )
        .await
        .unwrap();
    let RpcResult::Success {
        value: Some(typed_list),
    } = typed_list.result
    else {
        panic!("typed list must succeed");
    };
    assert!(typed_list.items.is_empty());

    let mut mux = typed
        .events
        .mux(EmptyRequest {}, AbortSignal::default(), None);
    let mux = mux.next().await.unwrap().unwrap();
    assert!(matches!(
        mux.payload,
        MuxFrame::SessionSubscribed { last_seq: 0, .. }
    ));
    let mut host = typed
        .events
        .host(EmptyRequest {}, AbortSignal::default(), None);
    let host = host.next().await.unwrap().unwrap();
    assert!(matches!(host.payload, HostFrame::RemoteEvent { .. }));

    let receipt = typed
        .respond(
            ClientResponse::new(
                RpcId::new("answer-typed"),
                RpcResult::Success {
                    value: Some(json!({ "behavior": "allow" })),
                },
            ),
            None,
        )
        .await
        .unwrap();
    assert_eq!(receipt, RpcReceipt::Accepted);

    api.stream_download.store(true, Ordering::Release);
    let mut download = HttpRequest::new(HttpMethod::Get, "/api/session.export");
    download.query = Some("sessionId=s1&includeDescendants=true".to_owned());
    let download = client.fetch(download).await.unwrap();
    assert_eq!(download.status, 201);
    assert_eq!(download.body, b"zip-stream");
    assert_eq!(api.downloads.lock().len(), 1);

    context.fiber().dispose().await.unwrap();
}
