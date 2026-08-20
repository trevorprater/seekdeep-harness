//! Host BFF entry and Loader shell for the Remote contribution assembly.

pub mod agent_lookup;
pub mod invariant;
pub mod remote_events;
pub mod types;

pub use agent_lookup::{
    ApiRemoteAgentOptions, ApiRemoteAgentResult, ApiRemoteLookupError, ApiRemoteSessionNotFound,
    ApiRemoteSubagentSessionOwnership, api_remote_subagent_ownership_error,
    create_api_remote_agent_resolver, has_api_remote_subagent_owner, inspect_api_remote_session,
};
pub use remote_events::API_REMOTE_FORWARDED_EVENTS;
pub use types::ApiRemoteForwardedEvent;

/// Host plugin body; the selected contributions mount only in Client environments.
pub fn apply() {}
