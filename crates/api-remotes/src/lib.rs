//! Host BFF entry and Loader shell for the Remote contribution assembly.

pub mod agent_lookup;
pub mod invariant;
pub mod remote_events;
pub mod types;

pub use remote_events::API_REMOTE_FORWARDED_EVENTS;
pub use types::ApiRemoteForwardedEvent;

/// Host plugin body; the selected contributions mount only in Client environments.
pub fn apply() {}
