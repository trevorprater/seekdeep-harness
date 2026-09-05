//! Durable provider-routed model-request recovery.

mod brand;
mod history;
/// Package-owned retry-event invariants.
pub mod invariant;
/// Agent request-error waterfall policy.
mod runtime;
/// Browser-safe durable event payloads.
pub mod types;

pub use brand::{RetryId, RetryPolicyKey};
pub use runtime::{
    NAME, RetryConfig, RetryInternals, install, install_with_internals, plugin,
    plugin_with_internals,
};
pub use types::{LlmRetryEventData, LlmRetryMode, LlmRetryStartedEventData};
