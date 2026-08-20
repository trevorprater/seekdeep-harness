//! Rust anonymous public HTTP(S) fetch provider for the SeekDeep web capability seam.

/// Configuration resolution and Cordis composition.
pub mod index;
/// Package-owned invariant companion.
pub mod invariant;
/// URL validation and content-type classification.
pub mod policy;
/// Provider transport and response decoding.
pub mod provider;

pub use index::{
    DEFAULT_USER_AGENT, HttpFetchConfig, NAME, config_schema, install, plugin, resolve_limits,
};
pub use policy::{
    FetchableKind, classify_content_type, decoder_for_charset, is_same_origin, parse_charset,
    validate_fetch_url,
};
pub use provider::{HttpFetchLimits, HttpFetchProvider, LOCAL_FETCH_PROVIDER_ID};
