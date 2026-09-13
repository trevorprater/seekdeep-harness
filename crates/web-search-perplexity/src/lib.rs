//! Rust `Perplexity`-backed search provider for the `SeekDeep` web capability seam.

/// Configuration resolution and Cordis composition.
pub mod index;
/// Package-owned invariant companion.
pub mod invariant;
/// Provider transport and response mapping.
pub mod provider;
/// Provider-private wire types.
pub mod types;

pub use index::{NAME, PerplexityConfig, config_schema, install, plugin, resolve_options};
pub use provider::{
    PERPLEXITY_DEFAULT_BASE_URL, PERPLEXITY_DEFAULT_MAX_TOKENS, PERPLEXITY_DEFAULT_MODEL,
    PERPLEXITY_PROVIDER_ID, PerplexityRecency, PerplexitySearchProvider,
    PerplexitySearchProviderOptions, map_perplexity_response, map_perplexity_result,
};
pub use types::{
    PerplexityChoice, PerplexityError, PerplexityErrorDetail, PerplexityMessage,
    PerplexityResponse, PerplexitySearchResult,
};
