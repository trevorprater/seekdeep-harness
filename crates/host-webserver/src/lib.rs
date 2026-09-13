//! Dynamic HTTP and upgrade route registry for the `SeekDeep` Web composition.

mod invariant;

use std::{
    collections::HashMap,
    convert::Infallible,
    future::Future,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use bytes::Bytes;
use http_body_util::{BodyExt as _, Empty, Full, combinators::UnsyncBoxBody};
use hyper::{
    Request, Response, StatusCode, body::Body as _, body::Incoming, header, header::HeaderValue,
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use indexmap::IndexMap;
use parking_lot::{Mutex, RwLock};
use seekdeep_cordis::{Context, Plugin, ServiceKey, fiber::EffectHandle};
use seekdeep_llm::AbortSignal;
use socket2::SockRef;
use tokio::{net::TcpListener, task::JoinHandle};
use uuid::Uuid;

pub use invariant::{INVARIANT_NAME, register_invariant};

/// Typed Cordis slot corresponding to `ctx.webServer`.
pub const WEB_SERVER: ServiceKey<WebServer> = ServiceKey::new("webServer");
/// Stable Cordis plugin name.
pub const NAME: &str = "host-webserver";
/// Webserver plugin has no service prerequisites.
pub const INJECT: &[&str] = &[];

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct WebServerWireConfig {
    host: String,
    port: u16,
}

impl Default for WebServerWireConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_owned(),
            port: 3080,
        }
    }
}

/// Response body used by registered handlers.
pub type WebBody = UnsyncBoxBody<Bytes, std::io::Error>;
/// Hyper request delivered to one route owner.
pub type WebRequest = Request<Incoming>;
/// Complete response returned by one route owner.
pub type WebResponse = Response<WebBody>;
/// Asynchronous route result.
pub type WebHandlerFuture =
    Pin<Box<dyn Future<Output = anyhow::Result<WebResponse>> + Send + 'static>>;
/// HTTP or upgrade handler.
pub type WebHandler = Arc<dyn Fn(WebRequest) -> WebHandlerFuture + Send + Sync + 'static>;

#[derive(Debug)]
struct WebConnectionState {
    signal: AbortSignal,
    monitored: AtomicBool,
    notify: tokio::sync::Notify,
}

/// Per-connection signal aborted when the peer socket closes or the server shuts down.
#[derive(Clone, Debug)]
pub struct WebConnectionSignal(Arc<WebConnectionState>);

impl WebConnectionSignal {
    fn new() -> Self {
        Self(Arc::new(WebConnectionState {
            signal: AbortSignal::default(),
            monitored: AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
        }))
    }

    /// Begins EOF observation after a handler has consumed its request body.
    pub fn monitor_disconnect(&self) {
        if !self.0.monitored.swap(true, Ordering::AcqRel) {
            self.0.notify.notify_waiters();
        }
    }

    /// Shared cancellation signal for in-flight handler work.
    #[must_use]
    pub fn signal(&self) -> AbortSignal {
        self.0.signal.clone()
    }

    async fn monitored(&self) {
        loop {
            let notified = self.0.notify.notified();
            if self.0.monitored.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}

/// Route match kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebRouteKind {
    /// Pathname must match verbatim.
    Exact,
    /// Matches the pathname and descendants separated by `/`.
    Prefix,
}

/// One named route registration.
#[derive(Clone)]
pub struct WebRoute {
    /// Exact or prefix match mode.
    pub kind: WebRouteKind,
    /// Absolute pathname without a trailing slash.
    pub path: String,
    /// Owner of the complete response lifecycle.
    pub handler: WebHandler,
}

impl std::fmt::Debug for WebRoute {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebRoute")
            .field("kind", &self.kind)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// One exact-path HTTP upgrade registration.
#[derive(Clone)]
pub struct WebUpgradeRoute {
    /// Exact pathname.
    pub path: String,
    /// Handler that returns protocol negotiation response and owns upgraded work.
    pub handler: WebHandler,
}

impl std::fmt::Debug for WebUpgradeRoute {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebUpgradeRoute")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// Supported listen host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListenHost {
    /// Loopback only (`127.0.0.1`).
    Loopback,
    /// All IPv4 interfaces (`0.0.0.0`).
    AllInterfaces,
}

impl ListenHost {
    fn address(self) -> IpAddr {
        IpAddr::V4(match self {
            Self::Loopback => Ipv4Addr::LOCALHOST,
            Self::AllInterfaces => Ipv4Addr::UNSPECIFIED,
        })
    }

    /// Exact source configuration spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Loopback => "127.0.0.1",
            Self::AllInterfaces => "0.0.0.0",
        }
    }
}

/// Webserver listen configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WebServerConfig {
    /// Loopback or all-interfaces bind host.
    pub host: ListenHost,
    /// Port, where zero requests an OS-assigned value.
    pub port: u16,
}

struct RouteRecord<T> {
    id: Uuid,
    value: T,
}

type IndexTransform = Arc<dyn Fn(String) -> String + Send + Sync>;

#[derive(Default)]
struct Registry {
    exact: HashMap<String, RouteRecord<WebRoute>>,
    prefixes: HashMap<String, RouteRecord<WebRoute>>,
    upgrades: HashMap<String, RouteRecord<WebUpgradeRoute>>,
    fallback: Option<RouteRecord<WebHandler>>,
    index_taps: Vec<RouteRecord<IndexTransform>>,
}

/// Synchronous registration disposer that never removes a replacement.
pub struct WebRegistration {
    withdraw: Arc<dyn Fn() + Send + Sync>,
}

impl std::fmt::Debug for WebRegistration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebRegistration")
            .finish_non_exhaustive()
    }
}

impl WebRegistration {
    /// Withdraws this exact registration. Repeated disposal is harmless.
    pub fn dispose(&self) {
        (self.withdraw)();
    }

    /// Converts this registration into a fiber-owned effect.
    ///
    /// # Errors
    ///
    /// Returns inactive-owner failures from Cordis.
    pub fn own(self, context: &Context, label: impl Into<String>) -> anyhow::Result<EffectHandle> {
        let withdraw = Arc::clone(&self.withdraw);
        let effect = EffectHandle::synchronous(label, move || {
            withdraw();
            Ok(())
        });
        match context.own(effect) {
            Ok(effect) => Ok(effect),
            Err(error) => {
                // No owner will ever withdraw the route: withdraw it now rather than leaving
                // it registered on the server after the caller's activation failed.
                self.dispose();
                Err(error.into())
            }
        }
    }
}

/// Live HTTP server plus dynamic route registries.
pub struct WebServer {
    config: WebServerConfig,
    port: u16,
    registry: Arc<RwLock<Registry>>,
    shutdown: AbortSignal,
    accept_task: Mutex<Option<JoinHandle<()>>>,
    connections: Arc<Mutex<Vec<JoinHandle<()>>>>,
    encoded: Mutex<EncodedBodies>,
}

impl std::fmt::Debug for WebServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebServer")
            .field("host", &self.config.host.as_str())
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl WebServer {
    /// Binds, begins accepting, and provides a live server.
    ///
    /// # Errors
    ///
    /// Returns bind, duplicate-service, or inactive-owner failures. A taken
    /// port therefore fails activation before the service becomes visible.
    pub async fn install(context: &Context, config: WebServerConfig) -> anyhow::Result<Arc<Self>> {
        let listener =
            TcpListener::bind(SocketAddr::new(config.host.address(), config.port)).await?;
        let port = listener.local_addr()?.port();
        let server = Arc::new(Self {
            config,
            port,
            registry: Arc::new(RwLock::new(Registry::default())),
            shutdown: AbortSignal::default(),
            accept_task: Mutex::new(None),
            connections: Arc::new(Mutex::new(Vec::new())),
            encoded: Mutex::new(EncodedBodies::default()),
        });
        let run_server = server.clone();
        *server.accept_task.lock() = Some(tokio::spawn(async move {
            run_server.accept(listener).await;
        }));
        if let Err(error) = context.provide(WEB_SERVER, server.clone()) {
            server.release_listener().await;
            return Err(error.into());
        }
        let cleanup = server.clone();
        if let Err(error) = context.own(EffectHandle::new("webServer.listen", move || {
            Box::pin(async move { cleanup.close().await })
        })) {
            // The owner went inactive between the publication and this registration:
            // nothing will ever run the cleanup, so release the listener now instead of
            // leaving the accept task holding the port.
            server.release_listener().await;
            return Err(error.into());
        }
        Ok(server)
    }

    /// Stops accepting and waits for the accept task to drop the listener, so a failed
    /// installation has released its port by the time its error is reported.
    async fn release_listener(&self) {
        self.shutdown.abort();
        let accept_task = self.accept_task.lock().take();
        if let Some(task) = accept_task {
            let _ = task.await;
        }
    }

    /// Actual listening port, including OS assignment for configured port zero.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Configured bind host.
    #[must_use]
    pub fn host(&self) -> ListenHost {
        self.config.host
    }

    /// Registers one exact or prefix route.
    ///
    /// # Errors
    ///
    /// Rejects a duplicate `(kind, path)` registration.
    pub fn register(&self, route: WebRoute) -> anyhow::Result<WebRegistration> {
        let id = Uuid::now_v7();
        let path = route.path.clone();
        let kind = route.kind;
        {
            let mut registry = self.registry.write();
            let table = match kind {
                WebRouteKind::Exact => &mut registry.exact,
                WebRouteKind::Prefix => &mut registry.prefixes,
            };
            anyhow::ensure!(
                !table.contains_key(&path),
                "webserver: duplicate {} route {path:?}",
                match kind {
                    WebRouteKind::Exact => "exact",
                    WebRouteKind::Prefix => "prefix",
                }
            );
            table.insert(path.clone(), RouteRecord { id, value: route });
        }
        let registry = self.registry.clone();
        Ok(WebRegistration {
            withdraw: Arc::new(move || {
                let mut registry = registry.write();
                let table = match kind {
                    WebRouteKind::Exact => &mut registry.exact,
                    WebRouteKind::Prefix => &mut registry.prefixes,
                };
                if table.get(&path).is_some_and(|record| record.id == id) {
                    table.remove(&path);
                }
            }),
        })
    }

    /// Registers one exact-path upgrade route.
    ///
    /// # Errors
    ///
    /// Rejects duplicate socket ownership.
    pub fn register_upgrade(&self, route: WebUpgradeRoute) -> anyhow::Result<WebRegistration> {
        let id = Uuid::now_v7();
        let path = route.path.clone();
        {
            let mut registry = self.registry.write();
            anyhow::ensure!(
                !registry.upgrades.contains_key(&path),
                "webserver: duplicate upgrade route {path:?}"
            );
            registry
                .upgrades
                .insert(path.clone(), RouteRecord { id, value: route });
        }
        let registry = self.registry.clone();
        Ok(WebRegistration {
            withdraw: Arc::new(move || {
                let mut registry = registry.write();
                if registry
                    .upgrades
                    .get(&path)
                    .is_some_and(|record| record.id == id)
                {
                    registry.upgrades.remove(&path);
                }
            }),
        })
    }

    /// Claims the singleton fallback seat.
    ///
    /// # Errors
    ///
    /// Rejects a second fallback owner.
    pub fn register_fallback(&self, handler: WebHandler) -> anyhow::Result<WebRegistration> {
        let id = Uuid::now_v7();
        {
            let mut registry = self.registry.write();
            anyhow::ensure!(
                registry.fallback.is_none(),
                "webserver: fallback already registered"
            );
            registry.fallback = Some(RouteRecord { id, value: handler });
        }
        let registry = self.registry.clone();
        Ok(WebRegistration {
            withdraw: Arc::new(move || {
                let mut registry = registry.write();
                if registry
                    .fallback
                    .as_ref()
                    .is_some_and(|record| record.id == id)
                {
                    registry.fallback = None;
                }
            }),
        })
    }

    /// Registers one index transform in insertion order.
    pub fn tap_index(&self, transform: IndexTransform) -> WebRegistration {
        let id = Uuid::now_v7();
        self.registry.write().index_taps.push(RouteRecord {
            id,
            value: transform,
        });
        let registry = self.registry.clone();
        WebRegistration {
            withdraw: Arc::new(move || {
                registry.write().index_taps.retain(|record| record.id != id);
            }),
        }
    }

    /// Applies current index transforms in registration order.
    #[must_use]
    pub fn apply_index_taps(&self, html: impl Into<String>) -> String {
        let taps = self
            .registry
            .read()
            .index_taps
            .iter()
            .map(|record| record.value.clone())
            .collect::<Vec<_>>();
        taps.into_iter().fold(html.into(), |body, tap| tap(body))
    }

    async fn accept(self: Arc<Self>, listener: TcpListener) {
        loop {
            let accepted = tokio::select! {
                () = self.shutdown.cancelled() => return,
                accepted = listener.accept() => accepted,
            };
            let Ok((stream, _peer)) = accepted else {
                if !self.shutdown.is_aborted() {
                    tracing::error!("webserver: listener accept failed");
                }
                continue;
            };
            let watcher = match SockRef::from(&stream).try_clone() {
                Ok(socket) => {
                    let stream: std::net::TcpStream = socket.into();
                    if let Err(error) = stream.set_nonblocking(true) {
                        tracing::warn!(%error, "webserver: disconnect watcher setup failed");
                        continue;
                    }
                    match tokio::net::TcpStream::from_std(stream) {
                        Ok(stream) => stream,
                        Err(error) => {
                            tracing::warn!(%error, "webserver: disconnect watcher setup failed");
                            continue;
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "webserver: disconnect watcher clone failed");
                    continue;
                }
            };
            let server = self.clone();
            let shutdown = self.shutdown.clone();
            let task = tokio::spawn(async move {
                let disconnected = WebConnectionSignal::new();
                let watcher_signal = disconnected.clone();
                let watcher = tokio::spawn(async move {
                    watch_disconnect(watcher, shutdown, watcher_signal).await;
                });
                let io = TokioIo::new(stream);
                let service = service_fn(move |request| {
                    let server = server.clone();
                    let disconnected = disconnected.clone();
                    async move {
                        let mut request = request;
                        request.extensions_mut().insert(disconnected);
                        Ok::<_, Infallible>(server.dispatch(request).await)
                    }
                });
                if let Err(error) = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service)
                    .with_upgrades()
                    .await
                {
                    tracing::warn!(%error, "webserver: connection failed");
                }
                watcher.abort();
                let _ = watcher.await;
            });
            let mut connections = self.connections.lock();
            connections.retain(|connection| !connection.is_finished());
            connections.push(task);
        }
    }

    async fn dispatch(&self, request: WebRequest) -> WebResponse {
        let path = request.uri().path().to_owned();
        let target = request.uri().to_string();
        let gzip = accepts_encoding(
            request
                .headers()
                .get(header::ACCEPT_ENCODING)
                .and_then(|value| value.to_str().ok()),
            "gzip",
        );
        let is_upgrade = request
            .headers()
            .get(hyper::header::CONNECTION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
            });
        let handler = {
            let registry = self.registry.read();
            if is_upgrade {
                registry
                    .upgrades
                    .get(&path)
                    .map(|record| record.value.handler.clone())
            } else {
                match_route(&registry, &path)
                    .map(|route| route.handler.clone())
                    .or_else(|| {
                        registry
                            .fallback
                            .as_ref()
                            .map(|record| record.value.clone())
                    })
            }
        };
        let Some(handler) = handler else {
            return response(StatusCode::NOT_FOUND, Bytes::new());
        };
        match handler(request).await {
            Ok(response) if gzip => {
                let key = replay_key(&target, &response);
                compress(&self.encoded, key, response).await
            }
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(%error, "webserver: request handler failed");
                response(StatusCode::BAD_REQUEST, Bytes::new())
            }
        }
    }

    async fn close(&self) -> anyhow::Result<()> {
        self.shutdown.abort();
        let accept_task = self.accept_task.lock().take();
        if let Some(task) = accept_task {
            let _ = task.await;
        }
        let connections = std::mem::take(&mut *self.connections.lock());
        for connection in &connections {
            connection.abort();
        }
        for connection in connections {
            let _ = connection.await;
        }
        Ok(())
    }
}

/// Builds the Loader-compatible Host Webserver plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let config: WebServerWireConfig = serde_json::from_value(config)?;
            let host = match config.host.as_str() {
                "127.0.0.1" => ListenHost::Loopback,
                "0.0.0.0" => ListenHost::AllInterfaces,
                other => anyhow::bail!("webserver: unsupported host {other:?}"),
            };
            WebServer::install(
                &context,
                WebServerConfig {
                    host,
                    port: config.port,
                },
            )
            .await?;
            Ok(())
        })
    })
}

async fn watch_disconnect(
    stream: tokio::net::TcpStream,
    shutdown: AbortSignal,
    disconnected: WebConnectionSignal,
) {
    tokio::select! {
        () = shutdown.cancelled() => {
            disconnected.0.signal.abort();
            return;
        }
        () = disconnected.monitored() => {}
    }
    // The socket is shared with the connection that consumes it, so a peek observes either
    // unread request bytes or the close. Wait for readiness instead of polling: an idle
    // long-lived stream costs no wakeups, and pending bytes the connection has not drained
    // yet are re-checked at a bounded pace rather than every millisecond.
    let mut probe = [0_u8; 1];
    loop {
        let ready = tokio::select! {
            () = shutdown.cancelled() => {
                disconnected.0.signal.abort();
                return;
            }
            ready = stream.readable() => ready,
        };
        if ready.is_err() {
            disconnected.0.signal.abort();
            return;
        }
        match stream.peek(&mut probe).await {
            Ok(0) | Err(_) => {
                disconnected.0.signal.abort();
                return;
            }
            Ok(_) => tokio::time::sleep(DISCONNECT_RECHECK).await,
        }
    }
}

/// Pause between disconnect re-checks while unread bytes sit on a monitored connection.
const DISCONNECT_RECHECK: std::time::Duration = std::time::Duration::from_millis(50);

fn match_route(registry: &Registry, path: &str) -> Option<WebRoute> {
    if let Some(exact) = registry.exact.get(path) {
        return Some(exact.value.clone());
    }
    registry
        .prefixes
        .iter()
        .filter(|(prefix, _)| path == *prefix || path.starts_with(&format!("{prefix}/")))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, route)| route.value.clone())
}

/// Creates a buffered response.
#[must_use]
pub fn response(status: StatusCode, body: impl Into<Bytes>) -> WebResponse {
    let body = Full::new(body.into())
        .map_err(|never| match never {})
        .boxed_unsync();
    let mut response = Response::new(body);
    *response.status_mut() = status;
    response
}

/// Smallest buffered body worth compressing; below this the framing outweighs the saving.
const COMPRESS_MIN_BYTES: u64 = 1024;

/// Whether a media type's browser representation compresses well enough to be worth the work.
fn compressible_media_type(content_type: &str) -> bool {
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    media_type.starts_with("text/")
        || media_type.ends_with("javascript")
        || media_type.ends_with("+json")
        || media_type.ends_with("+xml")
        || matches!(
            media_type.as_str(),
            "application/json" | "application/wasm" | "application/xml" | "image/svg+xml"
        )
}

/// Whether an `Accept-Encoding` field lists `coding` with a non-zero quality.
fn accepts_encoding(header: Option<&str>, coding: &str) -> bool {
    let Some(header) = header else {
        return false;
    };
    header.split(',').any(|entry| {
        let mut parameters = entry.split(';');
        let token = parameters.next().unwrap_or_default().trim();
        token.eq_ignore_ascii_case(coding)
            && !parameters.any(|parameter| {
                parameter
                    .trim()
                    .strip_prefix("q=")
                    .and_then(|quality| quality.trim().parse::<f32>().ok())
                    .is_some_and(|quality| quality <= 0.0)
            })
    })
}

fn gzip(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    use std::io::Write as _;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    encoder.write_all(bytes)?;
    encoder.finish()
}

/// Total encoded bytes one server replays before evicting the least recently used entry.
///
/// A revision-addressed target always carries the same bytes, so encoding it again on the next
/// load spends CPU to reproduce a result the server already has.
const ENCODED_BYTES_BUDGET: usize = 48 * 1024 * 1024;

/// Bounded, least-recently-used store of already-encoded response bodies.
#[derive(Default)]
struct EncodedBodies {
    entries: Mutex<IndexMap<String, Bytes>>,
    bytes: usize,
}

impl EncodedBodies {
    fn get(&mut self, key: &str) -> Option<Bytes> {
        let entries = self.entries.get_mut();
        let index = entries.get_index_of(key)?;
        entries.move_index(index, entries.len().saturating_sub(1));
        entries.get(key).cloned()
    }

    fn insert(&mut self, key: String, body: Bytes) {
        if body.len() > ENCODED_BYTES_BUDGET {
            return;
        }
        let entries: &mut IndexMap<String, Bytes> = self.entries.get_mut();
        if let Some(previous) = entries.shift_remove(&key) {
            self.bytes = self.bytes.saturating_sub(previous.len());
        }
        self.bytes += body.len();
        entries.insert(key, body);
        while self.bytes > ENCODED_BYTES_BUDGET {
            let Some((_, evicted)) = entries.shift_remove_index(0) else {
                self.bytes = 0;
                break;
            };
            self.bytes = self.bytes.saturating_sub(evicted.len());
        }
    }
}

/// The replay key for one response, present only when its bytes can never change.
///
/// Only a content-addressed target may be replayed: the key carries the request target and the
/// body length, and an immutable `cache-control` is the promise that both identify the bytes.
fn replay_key(target: &str, response: &WebResponse) -> Option<String> {
    let immutable = response
        .headers()
        .get(header::CACHE_CONTROL)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("immutable"));
    if !immutable {
        return None;
    }
    let size = response.body().size_hint().exact()?;
    Some(format!("{target}|{size}"))
}

/// Compresses a complete response body when the peer accepts gzip and every byte is already in
/// hand.
///
/// Streamed bodies (a `StreamBody` reports no exact length) and media types that do not benefit
/// pass through untouched, so event streams keep flowing frame by frame.
async fn compress(
    encoded: &Mutex<EncodedBodies>,
    key: Option<String>,
    response: WebResponse,
) -> WebResponse {
    if response.headers().contains_key(header::CONTENT_ENCODING) {
        return response;
    }
    let compressible = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(compressible_media_type);
    if !compressible {
        return response;
    }
    if response
        .body()
        .size_hint()
        .exact()
        .is_none_or(|size| size < COMPRESS_MIN_BYTES)
    {
        return response;
    }
    if let Some(key) = &key
        && let Some(cached) = encoded.lock().get(key)
    {
        let (parts, _) = response.into_parts();
        return encoded_response(parts, cached);
    }
    let (parts, body) = response.into_parts();
    let Ok(collected) = body.collect().await else {
        tracing::warn!("webserver: response body failed to buffer for compression");
        return Response::from_parts(
            parts,
            Full::new(Bytes::new())
                .map_err(|never| match never {})
                .boxed_unsync(),
        );
    };
    let original = collected.to_bytes();
    let source = original.clone();
    match tokio::task::spawn_blocking(move || gzip(&source)).await {
        Ok(Ok(compressed)) => {
            let compressed = Bytes::from(compressed);
            if let Some(key) = key {
                encoded.lock().insert(key, compressed.clone());
            }
            encoded_response(parts, compressed)
        }
        Ok(Err(error)) => {
            tracing::warn!(%error, "webserver: response compression failed");
            Response::from_parts(
                parts,
                Full::new(original)
                    .map_err(|never| match never {})
                    .boxed_unsync(),
            )
        }
        Err(error) => {
            tracing::warn!(%error, "webserver: response compression task failed");
            Response::from_parts(
                parts,
                Full::new(original)
                    .map_err(|never| match never {})
                    .boxed_unsync(),
            )
        }
    }
}

/// Rebuilds a response around one encoded body, keeping its status and other headers.
fn encoded_response(parts: hyper::http::response::Parts, compressed: Bytes) -> WebResponse {
    let mut response = Response::from_parts(
        parts,
        Full::new(compressed)
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    response
        .headers_mut()
        .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    response
        .headers_mut()
        .append(header::VARY, HeaderValue::from_static("accept-encoding"));
    response.headers_mut().remove(header::CONTENT_LENGTH);
    response
}

/// Creates an empty successful upgrade negotiation response.
///
/// # Errors
///
/// Rejects a protocol token that is not a valid HTTP header value.
pub fn switching_protocols(protocol: &str) -> anyhow::Result<WebResponse> {
    let body = Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed_unsync();
    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    response.headers_mut().insert(
        hyper::header::CONNECTION,
        hyper::header::HeaderValue::from_static("Upgrade"),
    );
    response.headers_mut().insert(
        hyper::header::UPGRADE,
        hyper::header::HeaderValue::from_str(protocol)?,
    );
    Ok(response)
}
