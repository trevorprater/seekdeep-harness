//! Host-side WebSocket carrier for the two server-to-browser event streams.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

use futures::{SinkExt as _, StreamExt as _, stream::BoxStream};
use http_body_util::{BodyExt as _, Empty};
use hyper::{Response, StatusCode, upgrade::OnUpgrade};
use hyper_util::rt::TokioIo;
use parking_lot::Mutex;
use seekdeep_host_webserver::{WebHandler, WebHandlerFuture, WebRequest, WebResponse};
use seekdeep_llm::AbortSignal;
use serde::Serialize;
use serde_json::{Map, Value};
use tokio::task::JoinHandle;
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        handshake::derive_accept_key,
        protocol::{CloseFrame, frame::coding::CloseCode},
    },
};
use uuid::Uuid;

use crate::{EventFrame, RpcError, RpcId, is_trusted_api_request};

/// Typed source stream behind one Host WebSocket downlink.
pub type DownlinkStream = BoxStream<'static, anyhow::Result<EventFrame>>;

/// Host API surface supplying the mux and Host event streams.
pub trait DownlinkApi: Send + Sync + 'static {
    /// Opens the mux stream with a signal aborted when its socket is lost.
    fn mux(&self, signal: AbortSignal) -> DownlinkStream;

    /// Opens the Host stream with a signal aborted when its socket is lost.
    fn host(&self, signal: AbortSignal) -> DownlinkStream;
}

/// One of the two independent downstream channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownlinkKind {
    /// Session multiplexing events.
    Mux,
    /// Host remote events.
    Host,
}

#[derive(Default)]
struct DownlinkState {
    closed: bool,
    pumps: Vec<JoinHandle<()>>,
}

/// In-progress downlink teardown returned by synchronous shutdown initiation.
pub struct DownlinkClose {
    pumps: Vec<JoinHandle<()>>,
}

impl DownlinkClose {
    /// Waits until every socket pump and source cleanup has stopped.
    pub async fn wait(self) {
        for pump in self.pumps {
            let _ = pump.await;
        }
    }
}

/// Owns WebSocket negotiation and frame pumping for both Host downlinks.
pub struct WebSocketDownlinks {
    api: Arc<dyn DownlinkApi>,
    shutdown: AbortSignal,
    state: Mutex<DownlinkState>,
}

impl std::fmt::Debug for WebSocketDownlinks {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock();
        formatter
            .debug_struct("WebSocketDownlinks")
            .field("closed", &state.closed)
            .field("owned_pumps", &state.pumps.len())
            .finish_non_exhaustive()
    }
}

impl WebSocketDownlinks {
    /// Creates a live no-listener WebSocket acceptor.
    #[must_use]
    pub fn new(api: Arc<dyn DownlinkApi>) -> Arc<Self> {
        Arc::new(Self {
            api,
            shutdown: AbortSignal::default(),
            state: Mutex::new(DownlinkState::default()),
        })
    }

    /// Builds an exact-upgrade handler for one downlink and trust policy.
    #[must_use]
    pub fn handler(self: &Arc<Self>, kind: DownlinkKind, trusted_hosts: Vec<String>) -> WebHandler {
        let downlinks = self.clone();
        Arc::new(move |request| {
            let downlinks = downlinks.clone();
            let trusted_hosts = trusted_hosts.clone();
            Box::pin(async move { downlinks.handle_upgrade(request, kind, &trusted_hosts) })
                as WebHandlerFuture
        })
    }

    /// Terminates owned sockets and waits for every event source to clean up.
    ///
    /// # Errors
    ///
    /// Like the source `ws` acceptor, a second close reports that the acceptor
    /// is no longer running.
    pub async fn close(&self) -> anyhow::Result<()> {
        self.begin_close()?.wait().await;
        Ok(())
    }

    /// Synchronously rejects new upgrades and aborts every active source.
    ///
    /// The returned cleanup must be awaited to preserve source-finalizer parity.
    ///
    /// # Errors
    ///
    /// Returns the stopped-acceptor diagnostic after the first call.
    pub fn begin_close(&self) -> anyhow::Result<DownlinkClose> {
        let mut state = self.state.lock();
        anyhow::ensure!(!state.closed, "websocket downlink acceptor is not running");
        state.closed = true;
        self.shutdown.abort();
        Ok(DownlinkClose {
            pumps: std::mem::take(&mut state.pumps),
        })
    }

    fn handle_upgrade(
        self: Arc<Self>,
        mut request: WebRequest,
        kind: DownlinkKind,
        trusted_hosts: &[String],
    ) -> anyhow::Result<WebResponse> {
        if !is_trusted_api_request(&copy_headers(request.headers()), trusted_hosts) {
            return Ok(text_response(StatusCode::FORBIDDEN, "forbidden"));
        }
        let response = handshake_response(&request)?;
        let upgrade = hyper::upgrade::on(&mut request);
        let downlinks = self.clone();
        let pump = tokio::spawn(async move {
            downlinks.accept(upgrade, kind).await;
        });
        let mut state = self.state.lock();
        if state.closed {
            pump.abort();
            return Err(anyhow::anyhow!(
                "websocket downlink acceptor is not running"
            ));
        }
        state.pumps.retain(|pump| !pump.is_finished());
        state.pumps.push(pump);
        Ok(response)
    }

    async fn accept(self: Arc<Self>, upgrade: OnUpgrade, kind: DownlinkKind) {
        let upgraded = tokio::select! {
            () = self.shutdown.cancelled() => return,
            upgraded = upgrade => match upgraded {
                Ok(upgraded) => upgraded,
                Err(error) => {
                    tracing::warn!(%error, "client-connection: WebSocket upgrade failed");
                    return;
                }
            },
        };
        let socket = WebSocketStream::from_raw_socket(
            TokioIo::new(upgraded),
            tokio_tungstenite::tungstenite::protocol::Role::Server,
            None,
        )
        .await;
        let signal = AbortSignal::default();
        let frames = catch_unwind(AssertUnwindSafe(|| match kind {
            DownlinkKind::Mux => self.api.mux(signal.clone()),
            DownlinkKind::Host => self.api.host(signal.clone()),
        }))
        .unwrap_or_else(|panic| {
            futures::stream::once(async move { Err(anyhow::anyhow!("{}", panic_message(&*panic))) })
                .boxed()
        });
        pump(socket, frames, signal, self.shutdown.clone()).await;
    }
}

#[derive(Serialize)]
struct ServerRequest<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(rename = "rpcId")]
    rpc_id: &'a RpcId,
    method: &'a str,
    payload: &'a Value,
}

async fn pump<S>(
    mut socket: WebSocketStream<S>,
    mut frames: DownlinkStream,
    signal: AbortSignal,
    shutdown: AbortSignal,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut connected = true;
    loop {
        if !connected {
            match frames.next().await {
                Some(Ok(_) | Err(_)) | None => break,
            }
        }
        tokio::select! {
            () = shutdown.cancelled() => {
                signal.abort();
                let _ = socket.send(Message::Close(None)).await;
                connected = false;
            }
            incoming = socket.next() => {
                match incoming {
                    Some(Ok(Message::Text(_) | Message::Binary(_))) => {
                        let _ = socket.send(Message::Close(Some(CloseFrame {
                            code: CloseCode::Policy,
                            reason: "downlink only".into(),
                        }))).await;
                        signal.abort();
                        connected = false;
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if socket.send(Message::Pong(payload)).await.is_err() {
                            signal.abort();
                            connected = false;
                        }
                    }
                    Some(Ok(Message::Pong(_) | Message::Frame(_))) => {}
                    Some(Ok(Message::Close(_))) => {
                        let _ = socket.flush().await;
                        signal.abort();
                        connected = false;
                    }
                    Some(Err(_)) | None => {
                        signal.abort();
                        connected = false;
                    }
                }
            }
            frame = frames.next() => {
                match frame {
                    Some(Ok(frame)) => {
                        if let Err(error) = send_frame(&mut socket, &frame).await {
                            if !signal.is_aborted() {
                                let failure = failure_frame(format!("Error: {error}"));
                                let _ = send_frame(&mut socket, &failure).await;
                            }
                            break;
                        }
                    }
                    Some(Err(error)) => {
                        if !signal.is_aborted() {
                            let failure = failure_frame(format!("Error: {error}"));
                            let _ = send_frame(&mut socket, &failure).await;
                        }
                        break;
                    }
                    None => break,
                }
            }
        }
    }
    signal.abort();
    if connected {
        let _ = socket.send(Message::Close(None)).await;
    }
}

async fn send_frame<S>(socket: &mut WebSocketStream<S>, frame: &EventFrame) -> anyhow::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let method = frame
        .payload
        .as_object()
        .and_then(|payload| payload.get("type"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("downlink frame has no string type"))?;
    let message = ServerRequest {
        kind: "server-request",
        rpc_id: &frame.rpc_id,
        method,
        payload: &frame.payload,
    };
    socket
        .send(Message::Text(serde_json::to_string(&message)?.into()))
        .await?;
    Ok(())
}

fn failure_frame(message: String) -> EventFrame {
    EventFrame {
        rpc_id: RpcId::new(Uuid::new_v4().to_string()),
        payload: serde_json::json!({
            "type": "stream/error",
            "error": RpcError {
                code: "internal".to_owned(),
                message,
                details: Map::new(),
            },
        }),
    }
}

fn handshake_response(request: &WebRequest) -> anyhow::Result<WebResponse> {
    anyhow::ensure!(
        request.method() == hyper::Method::GET,
        "WebSocket upgrade requires GET"
    );
    anyhow::ensure!(
        request
            .headers()
            .get(hyper::header::UPGRADE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("websocket")),
        "WebSocket upgrade header is missing"
    );
    anyhow::ensure!(
        request
            .headers()
            .get(hyper::header::CONNECTION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))),
        "WebSocket connection upgrade token is missing"
    );
    anyhow::ensure!(
        request
            .headers()
            .get(hyper::header::SEC_WEBSOCKET_VERSION)
            .and_then(|value| value.to_str().ok())
            == Some("13"),
        "unsupported WebSocket version"
    );
    let key = request
        .headers()
        .get(hyper::header::SEC_WEBSOCKET_KEY)
        .ok_or_else(|| anyhow::anyhow!("WebSocket key is missing"))?;
    let accept = derive_accept_key(key.as_bytes());
    let body = Empty::new().map_err(|never| match never {}).boxed_unsync();
    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    response.headers_mut().insert(
        hyper::header::CONNECTION,
        hyper::header::HeaderValue::from_static("Upgrade"),
    );
    response.headers_mut().insert(
        hyper::header::UPGRADE,
        hyper::header::HeaderValue::from_static("websocket"),
    );
    response.headers_mut().insert(
        hyper::header::SEC_WEBSOCKET_ACCEPT,
        hyper::header::HeaderValue::try_from(accept)?,
    );
    Ok(response)
}

fn text_response(status: StatusCode, body: &'static str) -> WebResponse {
    let body = http_body_util::Full::new(body.into())
        .map_err(|never| match never {})
        .boxed_unsync();
    let mut response = Response::new(body);
    *response.status_mut() = status;
    response
}

fn copy_headers(headers: &hyper::HeaderMap) -> std::collections::HashMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
        })
        .collect()
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    panic.downcast_ref::<&str>().map_or_else(
        || {
            panic
                .downcast_ref::<String>()
                .map_or_else(|| "downlink source panicked".to_owned(), Clone::clone)
        },
        |message| (*message).to_owned(),
    )
}
