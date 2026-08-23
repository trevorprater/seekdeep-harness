//! Unified Host API contracts, physical carrier adapters, and gateway service.
//!
//! The [`api`] module is the transport-independent contract layer. HTTP,
//! WebSocket, and in-process calls remain physical carriers around these
//! logical request and response messages.

pub mod api;
pub mod client;
pub mod configuration;
pub mod handler;
pub mod interactions;
pub mod invariant;
pub mod native_path_opener;
pub mod presets;
pub mod registry_workspace;
pub mod service;
pub mod session_export;
pub mod session_runtime;

pub use api::method::{
    ALL_RPC_METHODS, RpcMethod, UnknownRpcMethod, parse_unary_request, parse_unary_result,
    parse_unary_value,
};
pub use api::rpc::{
    ClientResponse, RpcMessage, RpcReceipt, RpcReceiptReason, RpcRequest, RpcResponse,
    ServerRequest, parse_client_request, parse_client_response, parse_rpc_error, parse_rpc_message,
    parse_rpc_receipt, parse_rpc_result, parse_server_request, parse_server_response,
};
pub use client::{
    AgentPresetsClient, ApiClient, ApiProxyContract, CredentialsClient, EnvelopeListener,
    EnvelopeSubscription, EventsClient, GoalsClient, HostClient, InProcessApiClient, LlmClient,
    SessionsClient, SettingsClient, SkillsClient, SubagentsClient, WorkspaceClient,
    new_web_api_client, new_web_api_client_with_timeout, parse_method_result,
};
pub use configuration::{ConfigurationApiProxyOptions, ConfigurationApiProxyRuntime};
pub use handler::{
    ApiDownlinkStream, ApiProxyHandler, ApiProxyRuntime, FetchBody, FetchBodyStream, FetchHandler,
    FetchResponse,
};
pub use interactions::InteractionApiProxyRuntime;
pub use native_path_opener::{
    NativePlatform, PathOpenerInternals, PathOpenerRunner, can_open_native_path, open_native_path,
    open_native_text_file,
};
pub use presets::{PresetApiProxyOptions, PresetApiProxyRuntime};
pub use registry_workspace::WorkspaceRegistryRuntime;
pub use seekdeep_client_connection::{
    ClientRequest, RpcError, RpcId, RpcResult, ServerResponse, result_of, transport_error,
};
pub use service::{
    ApiProxyDefaults, ApiProxyService, AttachedSessionCount, DefaultModelSelection, ModelSelection,
    PathCapabilityProbe, PathOpener, WorkspaceRuntime, WorkspaceRuntimeError, WorkspaceSnapshot,
};
pub use session_export::{
    AttachmentStoreExportAdapter, DEFAULT_SESSION_LOG_COMPRESSION_LEVEL,
    SESSION_LOG_PUSH_CHUNK_BYTES, SessionLineageNode, SessionLogAttachments,
    SessionLogCompressionLevel, SessionLogExportDeps, SessionLogLineageQuery,
    SessionLogLiveSessions, SessionLogPersistence, SessionLogZipEntry,
    SessionPersistenceExportAdapter, SessionStoreExportAdapter, flush_live_session_log,
    prepare_session_log_response, session_log_zip_filename,
};
pub use session_runtime::{
    ColdArtifactMetadata, DEFAULT_COLD_BLANK_PROBE_MAX_BYTES, SessionApiProxyOptions,
    SessionApiProxyRuntime, SessionApiProxyServices, SessionProjectionReads,
};
