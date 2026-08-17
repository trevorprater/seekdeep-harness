//! Real WebSocket executable specification for the two Host downlinks.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use futures::{SinkExt as _, StreamExt as _, stream};
use parking_lot::Mutex;
use seekdeep_client_connection::{
    ConnectionApiProxy, ConnectionFallback, ConnectionHostConfig, DownlinkApi, DownlinkKind,
    DownlinkStream, EventFrame, HOST_API_PROXY, HOST_EVENTS_PATH, HttpResponse, MUX_EVENTS_PATH,
    RpcId, WebSocketDownlinks, install_host,
};
use seekdeep_cordis::Context;
use seekdeep_host_webserver::{ListenHost, WebServer, WebServerConfig, WebUpgradeRoute};
use seekdeep_llm::AbortSignal;
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        Message,
        client::IntoClientRequest as _,
        http::{HeaderValue, header::HOST},
    },
};

type Source = Arc<dyn Fn(AbortSignal) -> DownlinkStream + Send + Sync>;

struct TestApi {
    mux: Source,
    host: Source,
}

impl DownlinkApi for TestApi {
    fn mux(&self, signal: AbortSignal) -> DownlinkStream {
        (self.mux)(signal)
    }

    fn host(&self, signal: AbortSignal) -> DownlinkStream {
        (self.host)(signal)
    }
}

fn source(callback: impl Fn(AbortSignal) -> DownlinkStream + Send + Sync + 'static) -> Source {
    Arc::new(callback)
}

fn idle(signal: AbortSignal) -> DownlinkStream {
    stream::unfold(signal, |signal| async move {
        signal.cancelled().await;
        None::<(anyhow::Result<EventFrame>, AbortSignal)>
    })
    .boxed()
}

fn api(mux: Source, host: Source) -> Arc<dyn DownlinkApi> {
    Arc::new(TestApi { mux, host })
}

async fn serve(downlinks: &Arc<WebSocketDownlinks>) -> (Context, Arc<WebServer>, String) {
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
    server
        .register_upgrade(WebUpgradeRoute {
            path: MUX_EVENTS_PATH.to_owned(),
            handler: downlinks.handler(DownlinkKind::Mux, Vec::new()),
        })
        .unwrap();
    server
        .register_upgrade(WebUpgradeRoute {
            path: HOST_EVENTS_PATH.to_owned(),
            handler: downlinks.handler(DownlinkKind::Host, Vec::new()),
        })
        .unwrap();
    let origin = format!("ws://127.0.0.1:{}", server.port());
    (context, server, origin)
}

async fn read_json<S>(socket: &mut tokio_tungstenite::WebSocketStream<S>) -> Value
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let message = socket.next().await.unwrap().unwrap();
    serde_json::from_str(message.to_text().unwrap()).unwrap()
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
async fn mux_and_host_use_independent_sockets_and_cancel_each_source() {
    let mux_aborted = Arc::new(AtomicBool::new(false));
    let host_aborted = Arc::new(AtomicBool::new(false));
    let downlinks = WebSocketDownlinks::new(api(
        source({
            let aborted = mux_aborted.clone();
            move |signal| {
                let first = stream::once(async {
                    Ok(EventFrame {
                        rpc_id: RpcId::new("mux-1"),
                        payload: json!({
                            "type": "session/subscribed",
                            "sessionId": "session-1",
                            "lastSeq": 4,
                        }),
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
        source({
            let aborted = host_aborted.clone();
            move |signal| {
                let first = stream::once(async {
                    Ok(EventFrame {
                        rpc_id: RpcId::new("host-1"),
                        payload: json!({
                            "type": "host/remote-event",
                            "event": "commands/change",
                            "args": [],
                        }),
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
    ));
    let (context, _server, origin) = serve(&downlinks).await;
    let (mut mux, _) = connect_async(format!("{origin}{MUX_EVENTS_PATH}"))
        .await
        .unwrap();
    let (mut host, _) = connect_async(format!("{origin}{HOST_EVENTS_PATH}"))
        .await
        .unwrap();
    assert_eq!(
        read_json(&mut mux).await,
        json!({
            "type": "server-request",
            "rpcId": "mux-1",
            "method": "session/subscribed",
            "payload": {
                "type": "session/subscribed",
                "sessionId": "session-1",
                "lastSeq": 4,
            },
        })
    );
    assert_eq!(
        read_json(&mut host).await,
        json!({
            "type": "server-request",
            "rpcId": "host-1",
            "method": "host/remote-event",
            "payload": {
                "type": "host/remote-event",
                "event": "commands/change",
                "args": [],
            },
        })
    );
    mux.close(None).await.unwrap();
    host.close(None).await.unwrap();
    wait_until(|| mux_aborted.load(Ordering::Acquire)).await;
    wait_until(|| host_aborted.load(Ordering::Acquire)).await;
    downlinks.close().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn client_messages_close_with_policy_violation_and_abort_the_source() {
    let aborted = Arc::new(AtomicBool::new(false));
    let downlinks = WebSocketDownlinks::new(api(
        source({
            let aborted = aborted.clone();
            move |signal| {
                let aborted = aborted.clone();
                stream::unfold(signal, move |signal| {
                    let aborted = aborted.clone();
                    async move {
                        signal.cancelled().await;
                        aborted.store(true, Ordering::Release);
                        None::<(anyhow::Result<EventFrame>, AbortSignal)>
                    }
                })
                .boxed()
            }
        }),
        source(idle),
    ));
    let (context, _server, origin) = serve(&downlinks).await;
    let (mut socket, _) = connect_async(format!("{origin}{MUX_EVENTS_PATH}"))
        .await
        .unwrap();
    socket
        .send(Message::Text("upstream payload".into()))
        .await
        .unwrap();
    let close = loop {
        if let Some(Ok(Message::Close(frame))) = socket.next().await {
            break frame.unwrap();
        }
    };
    assert_eq!(u16::from(close.code), 1008);
    assert_eq!(close.reason, "downlink only");
    wait_until(|| aborted.load(Ordering::Acquire)).await;
    downlinks.close().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn source_failure_sends_stream_error_before_normal_close() {
    let downlinks = WebSocketDownlinks::new(api(
        source(|_| stream::once(async { Err(anyhow::anyhow!("mux source failed")) }).boxed()),
        source(idle),
    ));
    let (context, _server, origin) = serve(&downlinks).await;
    let (mut socket, _) = connect_async(format!("{origin}{MUX_EVENTS_PATH}"))
        .await
        .unwrap();
    let failure = read_json(&mut socket).await;
    assert_eq!(
        failure["payload"],
        json!({
            "type": "stream/error",
            "error": {
                "code": "internal",
                "message": "Error: mux source failed",
                "details": {},
            },
        })
    );
    assert!(failure["rpcId"].as_str().is_some());
    assert_eq!(failure["method"], "stream/error");
    assert!(matches!(socket.next().await, Some(Ok(Message::Close(_)))));
    downlinks.close().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn transport_loss_aborts_source_and_late_frame_is_contained() {
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let release_rx = Arc::new(Mutex::new(Some(release_rx)));
    let carrier = Arc::new(Mutex::new(None));
    let finished = Arc::new(AtomicBool::new(false));
    let downlinks = WebSocketDownlinks::new(api(
        source({
            let carrier = carrier.clone();
            let finished = finished.clone();
            let release_rx = release_rx.clone();
            move |signal| {
                *carrier.lock() = Some(signal);
                let release = release_rx.lock().take().unwrap();
                let finished = finished.clone();
                stream::once(async move {
                    let _ = release.await;
                    finished.store(true, Ordering::Release);
                    Ok(EventFrame {
                        rpc_id: RpcId::new("late"),
                        payload: json!({
                            "type": "session/subscribed",
                            "sessionId": "session-late",
                            "lastSeq": 0,
                        }),
                    })
                })
                .boxed()
            }
        }),
        source(idle),
    ));
    let (context, _server, origin) = serve(&downlinks).await;
    let (socket, _) = connect_async(format!("{origin}{MUX_EVENTS_PATH}"))
        .await
        .unwrap();
    drop(socket);
    wait_until(|| carrier.lock().as_ref().is_some_and(AbortSignal::is_aborted)).await;
    release_tx.send(()).unwrap();
    wait_until(|| finished.load(Ordering::Acquire)).await;
    downlinks.close().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn malformed_source_frame_becomes_stream_error_and_does_not_escape() {
    let downlinks = WebSocketDownlinks::new(api(
        source(|_| {
            stream::once(async {
                Ok(EventFrame {
                    rpc_id: RpcId::new("bad"),
                    payload: json!({ "missing": "type" }),
                })
            })
            .boxed()
        }),
        source(idle),
    ));
    let (context, _server, origin) = serve(&downlinks).await;
    let (mut socket, _) = connect_async(format!("{origin}{MUX_EVENTS_PATH}"))
        .await
        .unwrap();
    let failure = read_json(&mut socket).await;
    assert_eq!(failure["method"], "stream/error");
    assert_eq!(
        failure["payload"]["error"]["message"],
        "Error: downlink frame has no string type"
    );
    assert!(matches!(socket.next().await, Some(Ok(Message::Close(_)))));
    downlinks.close().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn untrusted_upgrade_is_rejected_before_protocol_negotiation() {
    let downlinks = WebSocketDownlinks::new(api(source(idle), source(idle)));
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
    server
        .register_upgrade(WebUpgradeRoute {
            path: MUX_EVENTS_PATH.to_owned(),
            handler: downlinks.handler(DownlinkKind::Mux, vec!["example.test".to_owned()]),
        })
        .unwrap();
    let mut request = format!("ws://127.0.0.1:{}{MUX_EVENTS_PATH}", server.port())
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert(HOST, HeaderValue::from_static("attacker.test"));
    let error = connect_async(request).await.unwrap_err();
    assert!(error.to_string().contains("403"));
    downlinks.close().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn second_close_reports_stopped_acceptor() {
    let downlinks = WebSocketDownlinks::new(api(source(idle), source(idle)));
    downlinks.close().await.unwrap();
    assert!(
        downlinks
            .close()
            .await
            .unwrap_err()
            .to_string()
            .contains("not running")
    );
}

#[tokio::test]
async fn teardown_waits_for_source_cleanup() {
    let (cleanup_started_tx, cleanup_started_rx) = oneshot::channel();
    let cleanup_started_tx = Arc::new(Mutex::new(Some(cleanup_started_tx)));
    let (release_cleanup_tx, release_cleanup_rx) = oneshot::channel();
    let release_cleanup_rx = Arc::new(Mutex::new(Some(release_cleanup_rx)));
    let cleaned = Arc::new(AtomicBool::new(false));
    let downlinks = WebSocketDownlinks::new(api(
        source({
            let cleanup_started_tx = cleanup_started_tx.clone();
            let release_cleanup_rx = release_cleanup_rx.clone();
            let cleaned = cleaned.clone();
            move |signal| {
                let started = cleanup_started_tx.lock().take().unwrap();
                let release = release_cleanup_rx.lock().take().unwrap();
                let cleaned = cleaned.clone();
                stream::unfold(
                    Some((signal, started, release, cleaned)),
                    |state| async move {
                        let (signal, started, release, cleaned) = state?;
                        signal.cancelled().await;
                        let _ = started.send(());
                        let _ = release.await;
                        cleaned.store(true, Ordering::Release);
                        None::<(
                            anyhow::Result<EventFrame>,
                            Option<(
                                AbortSignal,
                                oneshot::Sender<()>,
                                oneshot::Receiver<()>,
                                Arc<AtomicBool>,
                            )>,
                        )>
                    },
                )
                .boxed()
            }
        }),
        source(idle),
    ));
    let (context, _server, origin) = serve(&downlinks).await;
    let (_socket, _) = connect_async(format!("{origin}{MUX_EVENTS_PATH}"))
        .await
        .unwrap();
    let closing = tokio::spawn({
        let downlinks = downlinks.clone();
        async move { downlinks.close().await }
    });
    cleanup_started_rx.await.unwrap();
    assert!(!closing.is_finished());
    release_cleanup_tx.send(()).unwrap();
    closing.await.unwrap().unwrap();
    assert!(cleaned.load(Ordering::Acquire));
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn host_plugin_tracks_optional_api_proxy_install_withdrawal_and_reprovision() {
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
    let url = format!("ws://127.0.0.1:{}{MUX_EVENTS_PATH}", server.port());
    assert!(
        connect_async(&url)
            .await
            .unwrap_err()
            .to_string()
            .contains("404")
    );

    let signals = Arc::new(Mutex::new(Vec::<AbortSignal>::new()));
    let stream_api = api(
        source({
            let signals = signals.clone();
            move |signal| {
                signals.lock().push(signal.clone());
                stream::once(async {
                    Ok(EventFrame {
                        rpc_id: RpcId::new("dynamic"),
                        payload: json!({ "type": "session/subscribed", "lastSeq": 0 }),
                    })
                })
                .chain(idle(signal))
                .boxed()
            }
        }),
        source(idle),
    );
    let fallback: ConnectionFallback =
        Arc::new(|_| Box::pin(async { HttpResponse::text(404, "proxy fallback") }));
    let proxy = ConnectionApiProxy::new(fallback, stream_api);
    let first_provision = context.provide(HOST_API_PROXY, proxy.clone()).unwrap();
    let (mut first, _) = connect_async(&url).await.unwrap();
    assert_eq!(read_json(&mut first).await["rpcId"], "dynamic");

    first_provision.dispose().await.unwrap();
    wait_until(|| signals.lock().first().is_some_and(AbortSignal::is_aborted)).await;
    assert!(
        connect_async(&url)
            .await
            .unwrap_err()
            .to_string()
            .contains("404")
    );

    let second_provision = context.provide(HOST_API_PROXY, proxy).unwrap();
    let (mut second, _) = connect_async(&url).await.unwrap();
    assert_eq!(read_json(&mut second).await["rpcId"], "dynamic");
    second_provision.dispose().await.unwrap();
    wait_until(|| signals.lock().get(1).is_some_and(AbortSignal::is_aborted)).await;
    context.fiber().dispose().await.unwrap();
}
