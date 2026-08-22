//! Native bundle stat polling and `/plugins/events` SSE broadcasting.

use std::{
    collections::BTreeMap,
    convert::Infallible,
    fs, io,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};

use bytes::Bytes;
use futures::stream;
use http_body_util::{BodyExt as _, Full, StreamBody};
use hyper::{Method, Response, StatusCode, body::Frame, header};
use parking_lot::Mutex;
use seekdeep_client_modules::{CLIENT_MODULES, ClientModuleHost, HostSubscription};
use seekdeep_cordis::{Plugin, ServiceKey, fiber::EffectHandle};
use seekdeep_host_webserver::{
    WEB_SERVER, WebBody, WebHandler, WebRegistration, WebRoute, WebRouteKind, WebServer,
};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{EVENTS_ENDPOINT, sse_data};

/// Stable plugin and service name.
pub const CLIENT_HMR_NAME: &str = "client-hmr";
/// Typed native HMR service.
pub const CLIENT_HMR: ServiceKey<ClientHmrHostService> = ServiceKey::new(CLIENT_HMR_NAME);
/// Invariant companion name.
pub const CLIENT_HMR_INVARIANT_NAME: &str = "client-hmr-invariant";
const PACKAGE_NAME: &str = "@seekdeep-ai/seekdeep-client-hmr";

/// Native watcher configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientHmrConfig {
    /// Bundle stat-poll interval in milliseconds.
    pub poll_interval_ms: u64,
}

impl Default for ClientHmrConfig {
    fn default() -> Self {
        Self {
            poll_interval_ms: 500,
        }
    }
}

#[derive(Clone, Debug)]
struct WatchedBundle {
    path: PathBuf,
    modified: SystemTime,
    size: u64,
    dirty: bool,
}

struct HostLifecycle {
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
    graph: Option<HostSubscription>,
    rebuilt: Option<HostSubscription>,
    route: Option<WebRegistration>,
}

/// Live Host bundle watcher and SSE connection registry.
pub struct ClientHmrHostService {
    modules: Arc<ClientModuleHost>,
    watched: Mutex<BTreeMap<String, WatchedBundle>>,
    connections: Mutex<Vec<tokio::sync::mpsc::UnboundedSender<Bytes>>>,
    lifecycle: Mutex<HostLifecycle>,
}

impl std::fmt::Debug for ClientHmrHostService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClientHmrHostService")
            .field("watched", &self.watched.lock().len())
            .field("connections", &self.connections.lock().len())
            .finish_non_exhaustive()
    }
}

impl ClientHmrHostService {
    /// Starts watch reconciliation, polling, and the SSE route.
    ///
    /// # Errors
    ///
    /// Rejects a zero poll interval, duplicate route, or initial wiring failure.
    pub fn start(
        modules: Arc<ClientModuleHost>,
        web_server: &WebServer,
        config: &ClientHmrConfig,
    ) -> anyhow::Result<Arc<Self>> {
        anyhow::ensure!(
            config.poll_interval_ms >= 1,
            "client-hmr pollIntervalMs must be at least 1"
        );
        let service = Arc::new(Self {
            modules,
            watched: Mutex::new(BTreeMap::new()),
            connections: Mutex::new(Vec::new()),
            lifecycle: Mutex::new(HostLifecycle {
                cancel: None,
                task: None,
                graph: None,
                rebuilt: None,
                route: None,
            }),
        });
        service.sync_watches();
        let graph = {
            let weak = Arc::downgrade(&service);
            service.modules.on_graph_changed(Arc::new(move || {
                if let Some(service) = weak.upgrade() {
                    service.sync_watches();
                }
            }))
        };
        let rebuilt = {
            let weak = Arc::downgrade(&service);
            service.modules.on_rebuilt(Arc::new(move |id, rev| {
                if let Some(service) = weak.upgrade() {
                    service.broadcast(&sse_data(&json!({
                        "type": "rebuilt",
                        "id": id,
                        "rev": rev,
                    })));
                }
            }))
        };
        let route_service = service.clone();
        let route: WebHandler = Arc::new(move |request| {
            let service = route_service.clone();
            Box::pin(async move { Ok(service.connect(request.method())) })
        });
        let route = match web_server.register(WebRoute {
            kind: WebRouteKind::Exact,
            path: EVENTS_ENDPOINT.to_owned(),
            handler: route,
        }) {
            Ok(route) => route,
            Err(error) => {
                graph.dispose();
                rebuilt.dispose();
                return Err(error);
            }
        };
        let (cancel, mut cancelled) = tokio::sync::oneshot::channel();
        let poll_service = service.clone();
        let poll_interval_ms = config.poll_interval_ms;
        let task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(poll_interval_ms));
            interval.tick().await;
            loop {
                tokio::select! {
                    _ = &mut cancelled => break,
                    _ = interval.tick() => poll_service.poll_watches(),
                }
            }
        });
        *service.lifecycle.lock() = HostLifecycle {
            cancel: Some(cancel),
            task: Some(task),
            graph: Some(graph),
            rebuilt: Some(rebuilt),
            route: Some(route),
        };
        Ok(service)
    }

    /// Stops the poller, removes subscriptions/route, and closes every stream.
    ///
    /// # Errors
    ///
    /// Returns a poll-task panic.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        let lifecycle = {
            let mut lifecycle = self.lifecycle.lock();
            HostLifecycle {
                cancel: lifecycle.cancel.take(),
                task: lifecycle.task.take(),
                graph: lifecycle.graph.take(),
                rebuilt: lifecycle.rebuilt.take(),
                route: lifecycle.route.take(),
            }
        };
        if let Some(cancel) = lifecycle.cancel {
            let _ = cancel.send(());
        }
        if let Some(task) = lifecycle.task {
            task.await?;
        }
        if let Some(graph) = lifecycle.graph {
            graph.dispose();
        }
        if let Some(rebuilt) = lifecycle.rebuilt {
            rebuilt.dispose();
        }
        if let Some(route) = lifecycle.route {
            route.dispose();
        }
        self.connections.lock().clear();
        self.watched.lock().clear();
        Ok(())
    }

    /// Runs one deterministic stat-poll pass.
    pub fn poll_now(&self) {
        self.poll_watches();
    }

    /// Current watched bundle count.
    #[must_use]
    pub fn watched_count(&self) -> usize {
        self.watched.lock().len()
    }

    fn sync_watches(&self) {
        let rows = self
            .modules
            .graph()
            .entries
            .iter()
            .filter_map(|row| {
                self.modules
                    .client_path(&row.id)
                    .map(|path| (row.id.as_str().to_owned(), path))
            })
            .collect::<BTreeMap<_, _>>();
        self.watched
            .lock()
            .retain(|id, watch| rows.get(id) == Some(&watch.path));
        for (id, path) in rows {
            if self.watched.lock().contains_key(&id) {
                continue;
            }
            self.watch_row(id, path);
        }
    }

    fn watch_row(&self, id: String, path: PathBuf) {
        let metadata = fs::metadata(&path);
        let mut watch = match metadata {
            Ok(metadata) => WatchedBundle {
                path,
                modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                size: metadata.len(),
                dirty: false,
            },
            Err(error) => {
                if error.kind() != io::ErrorKind::NotFound {
                    tracing::warn!(%error, "Client HMR baseline failed");
                }
                WatchedBundle {
                    path,
                    modified: SystemTime::UNIX_EPOCH,
                    size: 0,
                    dirty: true,
                }
            }
        };
        let current = (watch.modified, watch.size);
        self.rehash(&id, &mut watch, current);
        self.watched.lock().insert(id, watch);
    }

    fn poll_watches(&self) {
        let ids = self.watched.lock().keys().cloned().collect::<Vec<_>>();
        for id in ids {
            let mut watch = {
                let Some(watch) = self.watched.lock().remove(&id) else {
                    continue;
                };
                watch
            };
            match fs::metadata(&watch.path) {
                Ok(metadata) => {
                    let current = (
                        metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                        metadata.len(),
                    );
                    if watch.dirty || current != (watch.modified, watch.size) {
                        self.rehash(&id, &mut watch, current);
                    }
                }
                Err(error) => {
                    watch.dirty = true;
                    if error.kind() != io::ErrorKind::NotFound {
                        tracing::warn!(%error, "Client HMR poll failed");
                    }
                }
            }
            self.watched.lock().insert(id, watch);
        }
    }

    fn rehash(&self, id: &str, watch: &mut WatchedBundle, current: (SystemTime, u64)) {
        match self
            .modules
            .rebuilt(&seekdeep_client_modules::ClientModuleId::new(id))
        {
            Ok(_) => {
                watch.modified = current.0;
                watch.size = current.1;
                watch.dirty = false;
            }
            Err(error) if is_not_found(&error) => {
                watch.dirty = true;
            }
            Err(error) => {
                tracing::warn!(%error, "Client HMR rehash failed");
                watch.modified = current.0;
                watch.size = current.1;
                watch.dirty = false;
            }
        }
    }

    fn connect(&self, method: &Method) -> seekdeep_host_webserver::WebResponse {
        if method != Method::GET && method != Method::HEAD {
            return empty_response(StatusCode::METHOD_NOT_ALLOWED);
        }
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let _ = sender.send(Bytes::from_static(b": connected\n\n"));
        let graph = serde_json::to_value(&*self.modules.graph()).expect("graph serializes");
        let _ = sender.send(Bytes::from(sse_data(&json!({
            "type": "graph",
            "graph": graph,
        }))));
        self.connections.lock().push(sender);
        let body = StreamBody::new(stream::unfold(receiver, |mut receiver| async move {
            receiver
                .recv()
                .await
                .map(|bytes| (Ok::<_, io::Error>(Frame::data(bytes)), receiver))
        }))
        .boxed_unsync();
        let mut response = Response::new(body);
        *response.status_mut() = StatusCode::OK;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("text/event-stream"),
        );
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-cache"),
        );
        response.headers_mut().insert(
            header::CONNECTION,
            header::HeaderValue::from_static("keep-alive"),
        );
        response
    }

    fn broadcast(&self, frame: &str) {
        let bytes = Bytes::copy_from_slice(frame.as_bytes());
        self.connections
            .lock()
            .retain(|sender| sender.send(bytes.clone()).is_ok());
    }
}

impl Drop for ClientHmrHostService {
    fn drop(&mut self) {
        let lifecycle = self.lifecycle.get_mut();
        if let Some(cancel) = lifecycle.cancel.take() {
            let _ = cancel.send(());
        }
        if let Some(task) = lifecycle.task.take() {
            task.abort();
        }
        if let Some(graph) = lifecycle.graph.take() {
            graph.dispose();
        }
        if let Some(rebuilt) = lifecycle.rebuilt.take() {
            rebuilt.dispose();
        }
        if let Some(route) = lifecycle.route.take() {
            route.dispose();
        }
    }
}

/// Builds the Host Cordis plugin with strict config validation.
#[must_use]
pub fn client_hmr_host_plugin() -> Plugin {
    Plugin::new(
        CLIENT_HMR_NAME,
        ["clientModules", "webServer"],
        |context, config| {
            Box::pin(async move {
                let config: ClientHmrConfig = serde_json::from_value(config)?;
                anyhow::ensure!(
                    config.poll_interval_ms >= 1,
                    "client-hmr pollIntervalMs must be at least 1"
                );
                let modules = context
                    .get(CLIENT_MODULES)
                    .ok_or_else(|| anyhow::anyhow!("client-hmr requires clientModules"))?;
                let web_server = context
                    .get(WEB_SERVER)
                    .ok_or_else(|| anyhow::anyhow!("client-hmr requires webServer"))?;
                let service = ClientHmrHostService::start(modules, &web_server, &config)?;
                context.provide(CLIENT_HMR, service.clone())?;
                context.own(EffectHandle::new("client-hmr", move || {
                    Box::pin(async move { service.dispose().await })
                }))?;
                Ok(())
            })
        },
    )
    .with_config_validator(|value| {
        let config: ClientHmrConfig = serde_json::from_value(value.clone())?;
        anyhow::ensure!(
            config.poll_interval_ms >= 1,
            "client-hmr pollIntervalMs must be at least 1"
        );
        Ok(serde_json::to_value(config)?)
    })
}

/// Reserves the stat-poller teardown relation, owned by one joined Rust task.
///
/// # Errors
///
/// Returns ordinary invariant registry failures.
pub fn register_client_hmr_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<io::Error>())
        .is_some_and(|error| error.kind() == io::ErrorKind::NotFound)
}

fn empty_response(status: StatusCode) -> seekdeep_host_webserver::WebResponse {
    let body: WebBody = Full::new(Bytes::new())
        .map_err(|never: Infallible| match never {})
        .boxed_unsync();
    let mut response = Response::new(body);
    *response.status_mut() = status;
    response
}
