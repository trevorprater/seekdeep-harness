//! Host Webserver integration and bounded HTTP bridge for `/api`.

use std::{collections::HashMap, sync::Arc};

use bytes::Bytes;
use futures::{StreamExt as _, future::BoxFuture};
use http_body_util::{BodyExt as _, Full, StreamBody};
use hyper::{
    Response, StatusCode,
    body::{Frame, Incoming},
};
use seekdeep_cordis::{Context, ServiceKey, fiber::EffectHandle};
use seekdeep_host_webserver::{
    WEB_SERVER, WebConnectionSignal, WebHandler, WebHandlerFuture, WebRegistration, WebResponse,
    WebRoute, WebRouteKind, WebServer, WebUpgradeRoute,
};
use seekdeep_llm::AbortSignal;

use crate::{
    API_PATH, DEFAULT_MAX_REQUEST_BODY_BYTES, DownlinkApi, DownlinkKind, HOST_EVENTS_PATH,
    HostConnectionService, HttpMethod, HttpRequest, HttpResponse, MUX_EVENTS_PATH,
    WebSocketDownlinks, assert_image_body_capacity, is_trusted_api_request,
};

/// Future returned by Connection's optional API Proxy fallback.
pub type ConnectionFallbackFuture = BoxFuture<'static, HttpResponse>;
/// Transport-independent fallback for `/api` endpoints not claimed by an interceptor.
pub type ConnectionFallback =
    Arc<dyn Fn(HttpRequest) -> ConnectionFallbackFuture + Send + Sync + 'static>;

/// Typed optional Host API Proxy seat consumed by Connection.
pub const HOST_API_PROXY: ServiceKey<ConnectionApiProxy> = ServiceKey::new("apiProxy");

/// Transport-facing subset of the Host API Proxy.
pub struct ConnectionApiProxy {
    fallback: ConnectionFallback,
    downlinks: Arc<dyn DownlinkApi>,
}

impl std::fmt::Debug for ConnectionApiProxy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConnectionApiProxy")
            .finish_non_exhaustive()
    }
}

impl ConnectionApiProxy {
    /// Creates the fetch fallback plus its two event-stream sources.
    #[must_use]
    pub fn new(fallback: ConnectionFallback, downlinks: Arc<dyn DownlinkApi>) -> Arc<Self> {
        Arc::new(Self {
            fallback,
            downlinks,
        })
    }

    async fn fetch(&self, request: HttpRequest) -> HttpResponse {
        (self.fallback)(request).await
    }
}

struct MountedDownlinks {
    proxy: Arc<ConnectionApiProxy>,
    downlinks: Arc<WebSocketDownlinks>,
    mux: WebRegistration,
    host: WebRegistration,
}

#[derive(Default)]
struct DownlinkMountState {
    mounted: Option<MountedDownlinks>,
    cleanups: Vec<tokio::task::JoinHandle<()>>,
}

struct DownlinkMounts {
    context: Context,
    webserver: Arc<WebServer>,
    trusted_hosts: Vec<String>,
    state: parking_lot::Mutex<DownlinkMountState>,
}

impl DownlinkMounts {
    fn new(context: Context, webserver: Arc<WebServer>, trusted_hosts: Vec<String>) -> Arc<Self> {
        Arc::new(Self {
            context,
            webserver,
            trusted_hosts,
            state: parking_lot::Mutex::new(DownlinkMountState::default()),
        })
    }

    fn reconcile(&self) -> anyhow::Result<()> {
        let desired = self.context.get(HOST_API_PROXY);
        let mut state = self.state.lock();
        state.cleanups.retain(|cleanup| !cleanup.is_finished());
        if state
            .mounted
            .as_ref()
            .zip(desired.as_ref())
            .is_some_and(|(mounted, desired)| Arc::ptr_eq(&mounted.proxy, desired))
        {
            return Ok(());
        }
        if let Some(mounted) = state.mounted.take() {
            Self::withdraw(&mut state, &mounted);
        }
        let Some(proxy) = desired else {
            return Ok(());
        };
        let downlinks = WebSocketDownlinks::new(proxy.downlinks.clone());
        let mux = self.webserver.register_upgrade(WebUpgradeRoute {
            path: MUX_EVENTS_PATH.to_owned(),
            handler: downlinks.handler(DownlinkKind::Mux, self.trusted_hosts.clone()),
        })?;
        let host = match self.webserver.register_upgrade(WebUpgradeRoute {
            path: HOST_EVENTS_PATH.to_owned(),
            handler: downlinks.handler(DownlinkKind::Host, self.trusted_hosts.clone()),
        }) {
            Ok(host) => host,
            Err(error) => {
                mux.dispose();
                if let Ok(close) = downlinks.begin_close() {
                    state.cleanups.push(tokio::spawn(close.wait()));
                }
                return Err(error);
            }
        };
        state.mounted = Some(MountedDownlinks {
            proxy,
            downlinks,
            mux,
            host,
        });
        Ok(())
    }

    fn withdraw(state: &mut DownlinkMountState, mounted: &MountedDownlinks) {
        mounted.host.dispose();
        mounted.mux.dispose();
        if let Ok(close) = mounted.downlinks.begin_close() {
            state.cleanups.push(tokio::spawn(close.wait()));
        }
    }

    fn begin_close(&self) -> Vec<tokio::task::JoinHandle<()>> {
        let mut state = self.state.lock();
        if let Some(mounted) = state.mounted.take() {
            Self::withdraw(&mut state, &mounted);
        }
        std::mem::take(&mut state.cleanups)
    }

    async fn close(&self) {
        let cleanups = self.begin_close();
        for cleanup in cleanups {
            let _ = cleanup.await;
        }
    }
}

/// Resolved Host plugin configuration.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionHostConfig {
    /// Non-loopback deployment authorities accepted by the browser trust fence.
    pub trusted_hosts: Vec<String>,
    /// Maximum buffered JSON request body.
    pub max_request_body_bytes: usize,
    /// Aggregate image limit when an attachment/API Proxy plane is mounted.
    pub max_message_image_bytes: Option<usize>,
}

impl Default for ConnectionHostConfig {
    fn default() -> Self {
        Self {
            trusted_hosts: Vec::new(),
            max_request_body_bytes: DEFAULT_MAX_REQUEST_BODY_BYTES,
            max_message_image_bytes: None,
        }
    }
}

const PRIVILEGED_METHODS: &[&str] = &[
    "agentPreset.read",
    "agentPreset.copy",
    "agentPreset.openDocument",
    "agentPreset.remove",
    "host.pickDirectory",
    "host.openPath",
    "settings.describe",
    "settings.openDocument",
    "settings.update",
    "settings.replace",
    "settings.mutate",
    "credentials.describe",
    "credentials.set",
    "credentials.unset",
    "llm.discoverModels",
];

/// Installs Host Connection, its `/api` route, and optional API fallback.
///
/// # Errors
///
/// Rejects missing Webserver, malformed trusted authorities, an undersized
/// image carrier, duplicate service/route ownership, or an inactive fiber.
pub fn install_host(
    context: &Context,
    config: ConnectionHostConfig,
    fallback: Option<ConnectionFallback>,
) -> anyhow::Result<Arc<HostConnectionService>> {
    let webserver = context
        .get(WEB_SERVER)
        .ok_or_else(|| anyhow::anyhow!("client-connection requires webServer"))?;
    let connection = HostConnectionService::new(config.trusted_hosts.clone())?;
    connection.attach_webserver(webserver.clone())?;
    if let Some(max_message_image_bytes) = config.max_message_image_bytes {
        assert_image_body_capacity(config.max_request_body_bytes, max_message_image_bytes)?;
    }
    let route_connection = connection.clone();
    let trusted_hosts = config.trusted_hosts;
    let downlink_trusted_hosts = trusted_hosts.clone();
    let max_request_body_bytes = config.max_request_body_bytes;
    let route_context = context.clone();
    let route: WebHandler = Arc::new(move |request| {
        let connection = route_connection.clone();
        let trusted_hosts = trusted_hosts.clone();
        let fallback = fallback.clone();
        let route_context = route_context.clone();
        Box::pin(async move {
            dispatch_api(
                connection,
                request,
                &trusted_hosts,
                max_request_body_bytes,
                route_context,
                fallback,
            )
            .await
        }) as WebHandlerFuture
    });
    let route_registration = webserver.register(WebRoute {
        kind: WebRouteKind::Prefix,
        path: API_PATH.to_owned(),
        handler: route,
    })?;
    let downlink_mounts = DownlinkMounts::new(context.clone(), webserver, downlink_trusted_hosts);
    if let Err(error) = downlink_mounts.reconcile() {
        route_registration.dispose();
        let _ = downlink_mounts.begin_close();
        return Err(error);
    }
    if let Err(error) = connection.provide(context) {
        route_registration.dispose();
        let _ = downlink_mounts.begin_close();
        return Err(error);
    }
    route_registration.own(context, "client-connection: /api route")?;
    let cleanup_mounts = downlink_mounts.clone();
    context.own(EffectHandle::new(
        "client-connection: WebSocket downlinks",
        move || {
            Box::pin(async move {
                cleanup_mounts.close().await;
                Ok(())
            })
        },
    ))?;
    let reconcile_mounts = downlink_mounts;
    context.on_service_change(move || {
        if let Err(error) = reconcile_mounts.reconcile() {
            tracing::error!(%error, "client-connection: downlink dependency reconciliation failed");
        }
    })?;
    Ok(connection)
}

async fn dispatch_api(
    connection: Arc<HostConnectionService>,
    request: hyper::Request<Incoming>,
    trusted_hosts: &[String],
    max_request_body_bytes: usize,
    context: Context,
    fallback: Option<ConnectionFallback>,
) -> anyhow::Result<WebResponse> {
    let headers = copy_headers(request.headers());
    if !is_trusted_api_request(&headers, trusted_hosts) {
        return web_response(HttpResponse::text(403, "forbidden"));
    }
    let pathname = request.uri().path().to_owned();
    let method = pathname
        .strip_prefix(&format!("{API_PATH}/"))
        .filter(|method| !method.contains('/'));
    if method.is_some_and(|method| PRIVILEGED_METHODS.contains(&method))
        && !is_trusted_api_request(&headers, &[])
    {
        return web_response(HttpResponse::text(403, "forbidden"));
    }
    if request.method() == hyper::Method::GET
        && (pathname == MUX_EVENTS_PATH || pathname == HOST_EVENTS_PATH)
    {
        let mut response = HttpResponse::text(426, "upgrade required");
        response
            .headers
            .insert("connection".to_owned(), "Upgrade".to_owned());
        response
            .headers
            .insert("upgrade".to_owned(), "websocket".to_owned());
        return web_response(response);
    }
    let Some(request) = bridge_request(request, max_request_body_bytes).await? else {
        return Ok(payload_too_large());
    };
    let response = connection
        .dispatch_shared(API_PATH, request, move |request| async move {
            if let Some(proxy) = context.get(HOST_API_PROXY) {
                proxy.fetch(request).await
            } else if let Some(fallback) = fallback {
                fallback(request).await
            } else {
                HttpResponse::text(404, "not found")
            }
        })
        .await;
    web_response(response)
}

pub(crate) async fn dispatch_dedicated(
    connection: Arc<HostConnectionService>,
    channel: String,
    request: hyper::Request<Incoming>,
    authority: crate::ConnectionRpcAuthority,
) -> anyhow::Result<WebResponse> {
    if !connection.trusted_headers(&copy_headers(request.headers()), authority) {
        return web_response(HttpResponse::text(403, "forbidden"));
    }
    let Some(request) = bridge_request(request, DEFAULT_MAX_REQUEST_BODY_BYTES).await? else {
        return Ok(payload_too_large());
    };
    web_response(connection.dispatch(&channel, request).await)
}

async fn bridge_request(
    mut request: hyper::Request<Incoming>,
    max_request_body_bytes: usize,
) -> anyhow::Result<Option<HttpRequest>> {
    if request
        .headers()
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > max_request_body_bytes)
    {
        return Ok(None);
    }
    let connection_signal = request.extensions().get::<WebConnectionSignal>().cloned();
    let signal = connection_signal
        .as_ref()
        .map_or_else(AbortSignal::default, WebConnectionSignal::signal);
    let mut body = Vec::new();
    while let Some(frame) = request.body_mut().frame().await {
        let frame = frame?;
        if let Ok(data) = frame.into_data() {
            let Some(next_size) = body.len().checked_add(data.len()) else {
                return Ok(None);
            };
            if next_size > max_request_body_bytes {
                return Ok(None);
            }
            body.extend_from_slice(&data);
        }
    }
    if let Some(connection_signal) = connection_signal {
        connection_signal.monitor_disconnect();
    }
    let method = match *request.method() {
        hyper::Method::GET => HttpMethod::Get,
        hyper::Method::POST => HttpMethod::Post,
        _ => HttpMethod::Other(request.method().to_string()),
    };
    let bridged = HttpRequest {
        method,
        path: request.uri().path().to_owned(),
        query: request.uri().query().map(str::to_owned),
        headers: copy_headers(request.headers()),
        body,
        signal,
    };
    Ok(Some(bridged))
}

fn copy_headers(headers: &hyper::HeaderMap) -> HashMap<String, String> {
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

fn payload_too_large() -> WebResponse {
    let body = Full::new(Bytes::new())
        .map_err(|never| match never {})
        .boxed_unsync();
    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::PAYLOAD_TOO_LARGE;
    response.headers_mut().insert(
        hyper::header::CONNECTION,
        hyper::header::HeaderValue::from_static("close"),
    );
    response
}

fn web_response(mut source: HttpResponse) -> anyhow::Result<WebResponse> {
    let body = if let Some(stream) = source.body_stream.take() {
        StreamBody::new(stream.map(|chunk| {
            chunk
                .map(Bytes::from)
                .map(Frame::data)
                .map_err(|error| std::io::Error::other(error.to_string()))
        }))
        .boxed_unsync()
    } else {
        Full::new(Bytes::from(source.body))
            .map_err(|never| match never {})
            .boxed_unsync()
    };
    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::from_u16(source.status)?;
    for (name, value) in source.headers {
        response.headers_mut().insert(
            hyper::header::HeaderName::try_from(name)?,
            hyper::header::HeaderValue::try_from(value)?,
        );
    }
    Ok(response)
}
