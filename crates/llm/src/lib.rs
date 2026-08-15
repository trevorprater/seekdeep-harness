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
/// Provider retry configuration.
pub mod retry_policy;
/// Adapter registry and normalized streaming runtime.
pub mod runtime;
/// Content, stream, and request types.
pub mod types;

pub use adapter_failure::normalize_llm_failure;
pub use assembler::BlockAssembler;
pub use attribution::{APP_IDENTITY, AppIdentity, attribution_headers, user_agent};
pub use brand::{CallId, MessageId, ProviderRequestId, ReasoningEffortId};
pub use call_config::call_config_equals;
pub use content::content_has_image;
pub use error::{
    ApiKeyCheck, CONTEXT_WINDOW_EXCEEDED_CODE, EMPTY_RESPONSE_CODE, HarnessError,
    INVALID_CREDENTIAL_CODE, LlmError, LlmFailure, QUOTA_EXCEEDED_CODE, assert_usable_api_key,
    error_chain, is_context_window_exceeded_error, is_quota_exceeded_error, normalize_api_key,
};
pub use message::{
    CONTEXT_SUMMARY_MAX_CHARS, Message, MessageRole, MessageSource, bound_context_summary,
    chunk_is_token_delta,
};
pub use retry_policy::{MAX_TIMER_DELAY_MS, ResolvedRetryPolicy, resolve_retry_policy};
pub use runtime::{
    AdapterRegistrationHandle, AdapterStream, DirectoryRegistrationHandle, LLM, LlmAdapter,
    LlmRuntime, LlmStream, LlmStreamMiddleware, LlmStreamNext, ModelDiscoveryHandle,
    PreparedLlmCall,
};
pub use types::{
    AbortSignal, ContentBlock, FinishReason, GenerateOptions, LlmCallConfig,
    LlmCallConfigAdapterDefaults, LlmConfigurableProvider, LlmDiscoveredModel, LlmModelContext,
    LlmModelDiscoveryRequest, LlmModelInfo, LlmModelReasoningInfo, LlmProviderAuthentication,
    LlmProviderInfo, LlmReasoningEffortInfo, LlmRequestPurpose, LlmResolvedModelInfo,
    ModelModality, StreamChunk, TokenUsage, ToolSchema, is_token_delta,
};
