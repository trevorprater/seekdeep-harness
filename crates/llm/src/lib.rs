//! Provider-neutral messages, streaming chunks, assembly, failures, and retry policy.

/// Adapter-failure normalization.
pub mod adapter_failure;
/// Stream-to-message assembly.
pub mod assembler;
/// Static provider-request attribution.
pub mod attribution;
/// Stable cross-boundary identifiers.
pub mod brand;
/// Conversation call-configuration helpers.
pub mod call_config;
/// Content-block structural helpers.
pub mod content;
/// Errors and provider-neutral failure classification.
pub mod error;
/// Incremental LLM stream-grammar invariants.
pub mod invariant;
/// Immutable provider-neutral messages.
pub mod message;
/// Closed-union escape diagnostics.
pub mod never;
/// Provider retry configuration.
pub mod retry_policy;
/// Adapter registry and normalized streaming runtime.
pub mod runtime;
/// Content, stream, and request types.
pub mod types;

pub use adapter_failure::{AdapterRejection, normalize_adapter_rejection, normalize_llm_failure};
pub use assembler::BlockAssembler;
pub use attribution::{
    APP_IDENTITY, AppIdentity, attribution_headers, attribution_headers_for, user_agent,
    user_agent_for,
};
pub use brand::{
    CallId, MessageId, ModelId, ProviderId, ProviderRequestId, ReasoningEffortId, SessionId,
};
pub use call_config::call_config_equals;
pub use content::{assistant_text, content_has_image};
pub use error::{
    ApiKeyCheck, CONTEXT_WINDOW_EXCEEDED_CODE, EMPTY_RESPONSE_CODE, ErrorChainGraph, HarnessError,
    INVALID_CREDENTIAL_CODE, LlmError, LlmFailure, QUOTA_EXCEEDED_CODE, assert_usable_api_key,
    error_chain, is_context_window_exceeded_error, is_harness_error, is_quota_exceeded_error,
    normalize_api_key,
};
pub use invariant::{INVARIANT_NAME, register_invariant};
pub use message::{
    CONTEXT_SUMMARY_MAX_CHARS, ContextSnapshotSection, Message, MessageRole, MessageSource,
    UserMessage, bound_context_summary, bound_context_summary_units, chunk_is_token_delta,
};
pub use never::assert_never;
pub use retry_policy::{
    MAX_TIMER_DELAY_MS, ResolvedRetryPolicy, RetryPolicyMode, resolve_retry_policy,
    retry_policy_schema,
};
pub use runtime::{
    AdapterCleanup, AdapterRegistrationHandle, AdapterStream, BoxLlmChunkStream,
    DirectoryRegistrationHandle, LLM, LlmAdapter, LlmDispatchRoute, LlmDispatchTrace, LlmRuntime,
    LlmStream, LlmStreamMiddleware, LlmStreamNext, ModelDiscoveryHandle, PreparedLlmCall,
};
pub use types::{
    AbortSignal, ContentBlock, FinishReason, GenerateOptions, LlmCallConfig,
    LlmCallConfigAdapterDefaults, LlmConfigurableProvider, LlmDiscoveredModel, LlmModelContext,
    LlmModelDiscoveryRequest, LlmModelInfo, LlmModelReasoningInfo, LlmProviderAuthentication,
    LlmProviderInfo, LlmReasoningEffortInfo, LlmRequestPurpose, LlmResolvedModelInfo,
    ModelModality, StreamChunk, TokenUsage, ToolSchema, is_agent_loop_request, is_token_delta,
    mark_agent_loop_request,
};
