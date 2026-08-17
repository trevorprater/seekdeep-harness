//! Real HTTP/WebSocket checks for the Rust Client web carrier.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::{SinkExt as _, StreamExt as _, stream};
use parking_lot::Mutex;
use seekdeep_client_connection::{
    CLIENT_CONNECTION, ClientRequest, ConnectionApiProxy, ConnectionConfig, ConnectionFallback,
    ConnectionHostConfig, ConnectionSinks, DownlinkApi, DownlinkStream, EventFrame, HOST_API_PROXY,
    HttpResponse, MUX_EVENTS_PATH, RpcId, RpcResult, ServerResponse, StreamApi, UnaryTimeoutPolicy,
    WebApiClient, WebApiContract, WebApiDownlink, install_host,
};
use seekdeep_cordis::Context;
use seekdeep_host_webserver::{ListenHost, WebServer, WebServerConfig};
use seekdeep_llm::AbortSignal;
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::oneshot};
use tokio_tungstenite::{accept_async, tungstenite::Message};

struct TestDownlinks {
    mux: Arc<dyn Fn(AbortSignal) -> DownlinkStream + Send + Sync>,
    host: Arc<dyn Fn(AbortSignal) -> DownlinkStream + Send + Sync>,
}

#[derive(Default)]
struct CountingContract {
    responses: AtomicUsize,
    successes: AtomicUsize,
    downlinks: AtomicUsize,
}

impl WebApiContract for CountingContract {
    fn parse_server_response(&self, value: &Value) -> anyhow::Result<ServerResponse> {
        self.responses.fetch_add(1, Ordering::Relaxed);
        let response: ServerResponse = serde_json::from_value(value.clone())?;
        anyhow::ensure!(response.kind == "server-response");
        Ok(response)
    }

    fn parse_unary_success_value(
        &self,
        _method: &str,
        value: Option<&Value>,
    ) -> anyhow::Result<Option<Value>> {
        self.successes.fetch_add(1, Ordering::Relaxed);
        Ok(value.cloned())
    }

    fn parse_downlink_payload(
        &self,
        _downlink: WebApiDownlink,
        payload: &Value,
    ) -> anyhow::Result<Value> {
        self.downlinks.fetch_add(1, Ordering::Relaxed);
        Ok(payload.clone())
    }
}

impl DownlinkApi for TestDownlinks {
    fn mux(&self, signal: AbortSignal) -> DownlinkStream {
        (self.mux)(signal)
    }

    fn host(&self, signal: AbortSignal) -> DownlinkStream {
        (self.host)(signal)
    }
}

fn idle(signal: AbortSignal) -> DownlinkStream {
    stream::unfold(signal, |signal| async move {
        signal.cancelled().await;
        None::<(anyhow::Result<EventFrame>, AbortSignal)>
    })
    .boxed()
}

fn success_response(request: &ClientRequest, value: Value) -> HttpResponse {
    HttpResponse {
        status: 200,
        headers: [("content-type".to_owned(), "application/json".to_owned())]
            .into_iter()
            .collect(),
        body: serde_json::to_vec(&ServerResponse::new(
            request.rpc_id.clone(),
            RpcResult::Success { value: Some(value) },
        ))
        .unwrap(),
        body_stream: None,
    }
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("condition must become true");
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn concrete_client_drives_unary_handshake_both_downlinks_and_generic_rpc() {
    let host_context = Context::new();
    let server = WebServer::install(
        &host_context,
        WebServerConfig {
            host: ListenHost::Loopback,
            port: 0,
        },
    )
    .await
    .unwrap();
    install_host(&host_context, ConnectionHostConfig::default(), None).unwrap();
    let fallback: ConnectionFallback = Arc::new(|request| {
        Box::pin(async move {
            let message: ClientRequest = serde_json::from_slice(&request.body).unwrap();
            let value = if message.method == "host.describe" {
                json!({
                    "version": "rust",
                    "cwd": "/seekdeep",
                    "attachedSessions": 0,
                    "canOpenPath": true,
                })
            } else {
                json!({ "ref": "goal-rust" })
            };
            success_response(&message, value)
        })
    });
    let mux_aborted = Arc::new(AtomicBool::new(false));
    let downlinks: Arc<dyn DownlinkApi> = Arc::new(TestDownlinks {
        mux: Arc::new({
            let aborted = mux_aborted.clone();
            move |signal| {
                let first = stream::once(async {
                    Ok(EventFrame {
                        rpc_id: RpcId::new("mux-native"),
                        payload: json!({ "type": "session/subscribed", "lastSeq": 4 }),
                    })
                });
                let tail =
                    stream::unfold((signal, aborted.clone()), |(signal, aborted)| async move {
                        signal.cancelled().await;
                        aborted.store(true, Ordering::Release);
                        None::<(anyhow::Result<EventFrame>, (AbortSignal, Arc<AtomicBool>))>
                    });
                first.chain(tail).boxed()
            }
        }),
        host: Arc::new(|signal| {
            stream::once(async {
                Ok(EventFrame {
                    rpc_id: RpcId::new("host-native"),
                    payload: json!({
                        "type": "host/remote-event",
                        "event": "commands/change",
                        "args": [],
                    }),
                })
            })
            .chain(idle(signal))
            .boxed()
        }),
    });
    host_context
        .provide(HOST_API_PROXY, ConnectionApiProxy::new(fallback, downlinks))
        .unwrap();

    let contract = Arc::new(CountingContract::default());
    let client = WebApiClient::with_contract(
        Some(&format!("http://127.0.0.1:{}", server.port())),
        contract.clone(),
    )
    .unwrap();
    assert!(client.is_loopback());
    let observed = Arc::new(Mutex::new(Vec::<Value>::new()));
    let _subscription = client.subscribe_envelopes({
        let observed = observed.clone();
        Arc::new(move |batch| observed.lock().extend_from_slice(batch))
    });
    let client_context = Context::new();
    client.provide(&client_context).unwrap();
    let handle = client_context.get(CLIENT_CONNECTION).unwrap();
    let mux = Arc::new(Mutex::new(Vec::new()));
    let host = Arc::new(Mutex::new(Vec::new()));
    let connected = Arc::new(Mutex::new(Vec::new()));
    let stop = handle
        .start(
            ConnectionSinks {
                on_mux_envelope: Some({
                    let mux = mux.clone();
                    Arc::new(move |frame| mux.lock().push(frame))
                }),
                on_host_envelope: Some({
                    let host = host.clone();
                    Arc::new(move |frame| host.lock().push(frame))
                }),
                on_connected: Some({
                    let connected = connected.clone();
                    Arc::new(move |description| connected.lock().push(description))
                }),
                ..ConnectionSinks::default()
            },
            ConnectionConfig {
                backoff_base_ms: 5.0,
                backoff_factor: 1.0,
                backoff_max_ms: 5.0,
                stream_open_timeout_ms: 500.0,
            },
        )
        .unwrap();
    wait_until(|| !connected.lock().is_empty()).await;
    wait_until(|| !mux.lock().is_empty() && !host.lock().is_empty()).await;
    assert_eq!(connected.lock()[0]["version"], "rust");
    assert_eq!(mux.lock()[0].rpc_id.as_str(), "mux-native");
    assert_eq!(host.lock()[0].rpc_id.as_str(), "host-native");

    let result = handle
        .call(
            "/api",
            "goals/create",
            json!({ "args": { "agentId": "a" } }),
            AbortSignal::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        result,
        RpcResult::Success {
            value: Some(json!({ "ref": "goal-rust" })),
        }
    );
    wait_until(|| observed.lock().len() >= 4).await;
    let methods = observed
        .lock()
        .iter()
        .filter_map(|envelope| {
            envelope
                .get("method")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    assert!(methods.iter().any(|method| method == "host.describe"));
    assert!(methods.iter().any(|method| method == "session/subscribed"));
    assert!(methods.iter().any(|method| method == "host/remote-event"));
    assert!(contract.responses.load(Ordering::Relaxed) >= 1);
    assert!(contract.successes.load(Ordering::Relaxed) >= 1);
    assert!(contract.downlinks.load(Ordering::Relaxed) >= 2);
    stop.stop();
    wait_until(|| mux_aborted.load(Ordering::Acquire)).await;
    client_context.fiber().dispose().await.unwrap();
    host_context.fiber().dispose().await.unwrap();
}

#[test]
fn maps_http_and_https_bases_to_ws_and_wss_and_reports_loopback() {
    let local = WebApiClient::new(Some("http://127.8.9.10:3080/path?ignored=1")).unwrap();
    assert_eq!(
        local.downlink_url(MUX_EVENTS_PATH).unwrap().as_str(),
        "ws://127.8.9.10:3080/api/events.mux"
    );
    assert!(local.is_loopback());
    let remote = WebApiClient::new(Some("https://harness.example/root")).unwrap();
    assert_eq!(
        remote.downlink_url(MUX_EVENTS_PATH).unwrap().as_str(),
        "wss://harness.example/api/events.mux"
    );
    assert!(!remote.is_loopback());
}

#[tokio::test]
async fn already_aborted_downlink_never_dials() {
    let client = WebApiClient::new(Some("http://127.0.0.1:1")).unwrap();
    let signal = AbortSignal::default();
    signal.abort();
    let mut stream = client.mux(signal, Arc::new(|| panic!("must not open")));
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn malformed_text_and_binary_frames_are_dropped_without_killing_the_stream() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        socket
            .send(Message::Binary(vec![1, 2, 3].into()))
            .await
            .unwrap();
        socket.send(Message::Text("not json".into())).await.unwrap();
        socket
            .send(Message::Text(
                json!({
                    "type": "server-request",
                    "rpcId": "valid",
                    "method": "session/subscribed",
                    "payload": { "type": "session/subscribed", "lastSeq": 9 },
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        socket.send(Message::Close(None)).await.unwrap();
    });
    let client = WebApiClient::new(Some(&format!("http://127.0.0.1:{port}"))).unwrap();
    let (opened_tx, opened_rx) = oneshot::channel();
    let opened_tx = Arc::new(Mutex::new(Some(opened_tx)));
    let mut frames = client.mux(
        AbortSignal::default(),
        Arc::new(move || {
            if let Some(opened) = opened_tx.lock().take() {
                let _ = opened.send(());
            }
        }),
    );
    let (opened, frame) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(opened_rx, frames.next())
    })
    .await
    .expect("downlink and valid frame must arrive");
    opened.unwrap();
    let frame = frame.unwrap().unwrap();
    assert_eq!(frame.rpc_id.as_str(), "valid");
    assert_eq!(frame.payload["lastSeq"], 9);
    assert!(
        tokio::time::timeout(Duration::from_secs(2), frames.next())
            .await
            .expect("close must end the stream")
            .is_none()
    );
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("server must finish")
        .unwrap();
}

#[tokio::test]
async fn bounded_unary_timeout_aborts_a_pending_http_request() {
    let host_context = Context::new();
    let server = WebServer::install(
        &host_context,
        WebServerConfig {
            host: ListenHost::Loopback,
            port: 0,
        },
    )
    .await
    .unwrap();
    install_host(
        &host_context,
        ConnectionHostConfig::default(),
        Some(Arc::new(|_| Box::pin(std::future::pending()))),
    )
    .unwrap();
    let client = WebApiClient::with_timeout(
        Some(&format!("http://127.0.0.1:{}", server.port())),
        Duration::from_millis(10),
    )
    .unwrap();
    let caller_signal = AbortSignal::default();
    let error = client
        .call_unary("host.describe", json!({}), caller_signal.clone())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("timed out"));
    assert!(!caller_signal.is_aborted());

    let user_paced = client.call_unary_with_policy(
        "host.pickDirectory",
        json!({}),
        AbortSignal::default(),
        UnaryTimeoutPolicy::CallerSignalOnly,
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), user_paced)
            .await
            .is_err(),
        "caller-signal-only operation must outlive the configured 10 ms deadline"
    );
    host_context.fiber().dispose().await.unwrap();
}
