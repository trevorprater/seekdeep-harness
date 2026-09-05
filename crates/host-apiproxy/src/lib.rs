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
    PathCapabilityProbe, PathOpener, SaveDefaultModelSelection, WorkspaceRuntime,
    WorkspaceRuntimeError, WorkspaceSnapshot,
};
pub use session_export::{
    AttachmentStoreExportAdapter, DEFAULT_SESSION_LOG_COMPRESSION_LEVEL,
    SESSION_LOG_PUSH_CHUNK_BYTES, SessionLineageNode, SessionLogAttachments,
    SessionLogCompressionLevel, SessionLogExportDeps, SessionLogLineageQuery,
    SessionLogLiveSessions, SessionLogPersistence, SessionLogZipEntry,
    SessionPersistenceExportAdapter, SessionQueryExportAdapter, SessionStoreExportAdapter,
    flush_live_session_log, prepare_session_log_response, session_log_zip_filename,
};
pub use session_runtime::{
    ColdArtifactMetadata, DEFAULT_COLD_BLANK_PROBE_MAX_BYTES, SessionApiProxyOptions,
    SessionApiProxyRuntime, SessionApiProxyServices, SessionProjectionReads,
};

/// Loader plugin identity.
pub const PLUGIN_NAME: &str = "host-apiproxy";
/// Complete Host API dependencies required before gateway composition.
pub const PLUGIN_INJECT: &[&str] = &[
    "agentDefaultModel",
    "agents",
    "attachments",
    "directoryPicker",
    "llm",
    "sessions",
    "subagents",
    "sessionQuery",
    "tools",
    "userQuestions",
    "workspaceRegistry",
];

/// Host API gateway composition configuration.
#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Explicit native path-opening capability override.
    pub native_open: Option<bool>,
    /// Session-log ZIP DEFLATE level.
    pub session_export_compression_level: Option<u8>,
    /// Maximum cold artifact size eligible for a blankness probe.
    pub cold_blank_probe_max_bytes: Option<u64>,
}

#[derive(Debug)]
struct UnhandledDomains;

impl ApiProxyRuntime for UnhandledDomains {
    fn unary(
        &self,
        method: RpcMethod,
        _request: RpcRequest<serde_json::Value>,
        _signal: seekdeep_llm::AbortSignal,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<RpcResponse<serde_json::Value>>> {
        Box::pin(async move { anyhow::bail!("API method {method:?} has no composed domain") })
    }

    fn respond(
        &self,
        _message: ClientResponse,
        _signal: seekdeep_llm::AbortSignal,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<RpcReceipt>> {
        Box::pin(async { anyhow::bail!("API respond has no composed domain") })
    }

    fn mux(
        &self,
        _request: RpcRequest<serde_json::Value>,
        _signal: seekdeep_llm::AbortSignal,
    ) -> ApiDownlinkStream<api::events::MuxFrame> {
        use futures::StreamExt as _;
        futures::stream::empty().boxed()
    }

    fn host(
        &self,
        _request: RpcRequest<serde_json::Value>,
        _signal: seekdeep_llm::AbortSignal,
    ) -> ApiDownlinkStream<api::events::HostFrame> {
        use futures::StreamExt as _;
        futures::stream::empty().boxed()
    }

    fn session_log(
        &self,
        _query: api::downloads::SessionLogQuery,
        _signal: seekdeep_llm::AbortSignal,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<seekdeep_client_connection::HttpResponse>>
    {
        Box::pin(async { anyhow::bail!("API session export has no composed domain") })
    }
}

/// Builds and publishes the compiled transport-independent Host API gateway.
///
/// # Errors
///
/// Returns configuration, missing-service, gateway-composition, or publication failures.
pub fn install(
    context: &seekdeep_cordis::Context,
    config: Config,
) -> anyhow::Result<std::sync::Arc<ApiProxyService>> {
    if let Some(level) = config.session_export_compression_level {
        anyhow::ensure!(
            level <= 9,
            "sessionExportCompressionLevel must be at most 9"
        );
    }
    let defaults_service = context
        .get(seekdeep_agent_default_model::AGENT_DEFAULT_MODEL)
        .ok_or_else(|| anyhow::anyhow!("host-apiproxy requires agentDefaultModel"))?;
    let current_defaults = defaults_service.clone();
    let save_defaults = defaults_service;
    let defaults = ApiProxyDefaults {
        default_model_selection: std::sync::Arc::new(move || {
            let selection = current_defaults.current_selection();
            ModelSelection {
                provider: selection.provider.to_string(),
                model: selection.model.to_string(),
                reasoning_effort: selection
                    .reasoning_effort
                    .map(|effort| effort.as_str().to_owned()),
            }
        }),
        save_default_model_selection: Some(std::sync::Arc::new(move |selection| {
            let save_defaults = save_defaults.clone();
            Box::pin(async move {
                save_defaults
                    .save_selection(&seekdeep_agent::ModelSelection {
                        provider: seekdeep_llm::ProviderId::new(selection.provider),
                        model: seekdeep_llm::ModelId::new(selection.model),
                        reasoning_effort: selection
                            .reasoning_effort
                            .map(seekdeep_llm::ReasoningEffortId::new),
                    })
                    .await
            })
        })),
        cwd: std::env::current_dir()?.to_string_lossy().into_owned(),
        open_path: None,
        open_text_file: None,
        can_open_path: config
            .native_open
            .map(|available| std::sync::Arc::new(move || available) as PathCapabilityProbe),
        native_path_opener: PathOpenerInternals::default(),
        cold_blank_probe_max_bytes: config.cold_blank_probe_max_bytes,
        session_export_compression_level: SessionLogCompressionLevel::new(
            config
                .session_export_compression_level
                .unwrap_or(DEFAULT_SESSION_LOG_COMPRESSION_LEVEL),
        )?,
    };
    let agents = context
        .get(seekdeep_agent::AGENTS)
        .ok_or_else(|| anyhow::anyhow!("host-apiproxy requires agents"))?;
    let attached: AttachedSessionCount = std::sync::Arc::new(move || agents.list().len());
    let domains: std::sync::Arc<dyn ApiProxyRuntime> = std::sync::Arc::new(UnhandledDomains);
    let service = ApiProxyService::from_context(context, defaults, attached, domains)?;
    let handler = ApiProxyHandler::new(service.clone());
    context.provide(
        seekdeep_client_connection::HOST_API_PROXY,
        handler.connection_proxy(),
    )?;
    Ok(service)
}

/// Builds the Loader-compatible Host API gateway plugin.
#[must_use]
pub fn plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(
        PLUGIN_NAME,
        PLUGIN_INJECT.iter().copied(),
        |context, config| {
            Box::pin(async move {
                install(&context, serde_json::from_value(config)?)?;
                Ok(())
            })
        },
    )
}
