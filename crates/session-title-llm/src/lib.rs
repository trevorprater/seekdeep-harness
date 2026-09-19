//! Shared model-backed session-title provider policy.

pub mod index;
pub mod invariant;

pub use index::{
    SESSION_TITLE_TIMEOUT_CODE, SessionTitleLlmConfig, SessionTitleLlmMessageSelector,
    config_schema, generate_session_title_with_llm, register_session_title_llm_provider,
    resolve_session_title_llm_config,
};
