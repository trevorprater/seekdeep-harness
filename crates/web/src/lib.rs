//! Service definition for the web access capability seam.

pub mod index;
pub mod invariant;
pub mod types;

pub use index::{INJECT, NAME, WEB, WebRuntime, WebRuntimeConfig, config_schema, plugin};
pub use types::{
    WEB_ERROR_NAME, WebFetchBody, WebFetchProvider, WebFetchRequest, WebFetchResult,
    WebSearchProvider, WebSearchRequest, WebSearchResult, WebSearchSource, web_error,
};
