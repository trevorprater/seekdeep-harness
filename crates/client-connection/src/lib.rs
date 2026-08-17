//! HTTP-up/WebSocket-down Connection transport shared by `SeekDeep` Host and Client.

mod controller;
mod host;
mod invariant;
mod rpc;
mod trust;
mod web_api_client;
mod websocket_downlink;

pub use controller::{
    ConnectionConfig, ConnectionController, ConnectionSinks, ConnectionState, EventFrame,
    HostDescription, StreamApi,
};
pub use host::{
    ConnectionApiProxy, ConnectionFallback, ConnectionFallbackFuture, ConnectionHostConfig,
    HOST_API_PROXY, install_host,
};
pub use invariant::{INVARIANT_NAME, install_invariant};
pub use rpc::{
    CLIENT_CONNECTION, ClientConnection, ClientConnectionFuture, ClientConnectionHandle,
    ClientRequest, ConnectionRpcAuthority, ConnectionStopHandle, EndpointMatcher, HOST_CONNECTION,
    HostConnectionService, HostDescriptionSubscription, HttpMethod, HttpRequest, HttpResponse,
    HttpResponseStream, HttpTransport, HttpTransportFuture, RpcError, RpcHandler, RpcHandlerFuture,
    RpcId, RpcResult, ServerResponse, SharedRpcRegistration, WebConnectionRpc, endpoint_from_path,
    result_of, transport_error, validate_rpc_target,
};
pub use trust::{assert_trusted_authority, is_loopback_hostname, is_trusted_api_request};
pub use web_api_client::{
    EnvelopeSubscription, UnaryTimeoutPolicy, WebApiClient, WebApiContract, WebApiDownlink,
    default_connection_config, install_client,
};
pub use websocket_downlink::{
    DownlinkApi, DownlinkClose, DownlinkKind, DownlinkStream, WebSocketDownlinks,
};

/// Stable Cordis plugin name.
pub const NAME: &str = "client-connection";
/// Route prefix owning every API request.
pub const API_PATH: &str = "/api";
/// Browser mux-frame WebSocket pathname.
pub const MUX_EVENTS_PATH: &str = "/api/events.mux";
/// Browser host-frame WebSocket pathname.
pub const HOST_EVENTS_PATH: &str = "/api/events.host";
/// Default maximum buffered request body: 160 MiB.
pub const DEFAULT_MAX_REQUEST_BODY_BYTES: usize = 160 * 1024 * 1024;

/// Headroom for RPC JSON fields around aggregate base64 image payloads.
pub const REQUEST_ENVELOPE_HEADROOM_BYTES: usize = 1024 * 1024;

/// Computes the minimum carrier body capacity for the configured aggregate image limit.
///
/// # Errors
///
/// Returns overflow when the configured limit cannot be represented.
pub fn required_image_body_bytes(max_message_image_bytes: usize) -> anyhow::Result<usize> {
    let encoded = max_message_image_bytes
        .checked_mul(4)
        .and_then(|value| value.checked_add(2))
        .map(|value| value / 3)
        .ok_or_else(|| anyhow::anyhow!("aggregate image limit overflows carrier capacity"))?;
    encoded
        .checked_add(REQUEST_ENVELOPE_HEADROOM_BYTES)
        .ok_or_else(|| anyhow::anyhow!("aggregate image limit overflows carrier capacity"))
}

/// Ensures the configured carrier cap can contain one maximum aggregate image batch.
///
/// # Errors
///
/// Returns the same load-time diagnostic as the source package when undersized.
pub fn assert_image_body_capacity(
    max_request_body_bytes: usize,
    max_message_image_bytes: usize,
) -> anyhow::Result<()> {
    let required = required_image_body_bytes(max_message_image_bytes)?;
    anyhow::ensure!(
        max_request_body_bytes >= required,
        "client-connection maxRequestBodyBytes ({max_request_body_bytes}) must be at least \
         {required} for the configured aggregate image limit"
    );
    Ok(())
}
