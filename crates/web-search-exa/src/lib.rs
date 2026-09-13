//! Rust `Exa`-backed search provider for the `SeekDeep` web capability seam.

/// Configuration resolution and Cordis composition.
pub mod index;
/// Package-owned invariant companion.
pub mod invariant;
/// Provider transport and response mapping.
pub mod provider;
/// Provider-private wire types.
pub mod types;

pub use index::{ExaConfig, NAME, config_schema, install, plugin, resolve_options};
pub use provider::{
    EXA_DEFAULT_BASE_URL, EXA_DEFAULT_HIGHLIGHTS_PER_RESULT, EXA_DEFAULT_SEARCH_TYPE,
    EXA_PROVIDER_ID, ExaSearchProvider, ExaSearchProviderOptions, SearchType, map_exa_response,
    map_exa_result,
};
pub use types::{ExaError, ExaResult, ExaSearchResponse};
