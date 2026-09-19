//! Rust `DeepSeek`-backed search provider for the `SeekDeep` web capability seam.

/// Configuration resolution and Cordis composition.
pub mod index;
/// Package-owned invariant companion.
pub mod invariant;
/// Provider transport and response mapping.
pub mod provider;
/// Provider-private wire types.
pub mod types;

pub use index::{
    DeepSeekSearchConfig, NAME, config_schema, install, plugin, resolve_options,
    web_search_deepseek_settings_namespace,
};
pub use provider::{
    DEEPSEEK_DEFAULT_API_VERSION, DEEPSEEK_DEFAULT_BASE_URL, DEEPSEEK_DEFAULT_MAX_TOKENS,
    DEEPSEEK_DEFAULT_MAX_USES, DEEPSEEK_DEFAULT_MODEL, DEEPSEEK_PROVIDER_ID,
    DeepSeekSearchLlmRequest, DeepSeekSearchProvider, DeepSeekSearchProviderOptions,
    citation_snippets, map_anthropic_response,
};
pub use types::{
    AnthropicError, AnthropicResponse, CitationLocation, ContentBlock, WebSearchResultItem,
};
