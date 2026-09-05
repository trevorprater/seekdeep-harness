//! Rust `DeepSeek` chat-completions adapter.

/// HTTP transport and adapter-owned model metadata.
pub mod adapter;
/// Configuration resolution and Cordis composition.
pub mod config;
/// Package-owned invariant companion.
pub mod invariant;
/// Request serialization.
pub mod serialize;
/// Strict SSE framing.
pub mod sse;
/// Stream-chunk translation.
pub mod translate;
/// DeepSeek wire values.
pub mod types;

pub use adapter::{
    DEFAULT_CONTEXT_WINDOW, DEFAULT_MAX_TOKENS, DEFAULT_STREAM_IDLE_TIMEOUT_MS, DeepSeekAdapter,
    DeepSeekAdapterOptions, DeepSeekConnectionOptions, ResolvedDeepSeekCatalogModel,
};
pub use config::{
    DeepSeekCatalogModel, DeepSeekConfig, NAME, PUBLIC_BASE_URL, install, plugin,
    resolve_adapter_options,
};
pub use serialize::{ReasoningEffort, RequestDefaults};
