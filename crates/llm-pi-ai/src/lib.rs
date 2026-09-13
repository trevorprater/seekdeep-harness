//! Multi-provider adapter compatibility for the source `pi-ai` integration.

mod json;

/// Official Codex ChatGPT credential-file bridge.
pub mod codex_auth;

/// Lossless provider catalog materialization and profile overrides.
pub mod catalog;

/// Provider-profile parsing, validation, and detached route resolution.
pub mod config;

/// Catalog-first and bounded endpoint model discovery.
pub mod discovery;

/// Explained-empty package invariant companion.
pub mod invariant;

/// Provider construction, authentication inheritance, and protocol dispatch.
pub mod provider;

/// Snapshot-owned multi-provider LLM adapter over native protocol executors.
pub mod adapter;

/// OpenAI-compatible Chat Completions native protocol engine.
pub mod openai_completions;

/// OpenAI Responses native protocol engine.
pub mod openai_responses;

/// Native protocol dispatcher shared by the package adapter.
pub mod executor;

/// Anthropic Messages native protocol engine.
pub mod anthropic_messages;

/// Package plugin lifecycle, dynamic settings, credentials, directory, and discovery.
pub mod plugin;

/// Google Generative AI native protocol engine.
pub mod google_generative;

/// Amazon Bedrock Converse Stream native protocol engine.
pub mod bedrock;

/// Request-history conversion into native pi-ai context values.
pub mod context;

/// Provider-native assistant history and durable replay metadata.
pub mod replay;

/// Native pi-ai event translation into Harness stream chunks.
pub mod stream;

pub use adapter::{PiAiAdapter, PiAiAdapterOptions};
pub use catalog::{
    PiCompatProfile, PiModality, PiModelFields, PiModelProfile, PiReasoningEfforts,
    PiThinkingFormat,
};
pub use config::{
    PiCacheRetention, PiThinkingBudgets, PiTransport, ResolvedPiProviderProfile, config_schema,
};
pub use plugin::{INJECT, NAME, plugin};
pub use provider::supported_protocols;
