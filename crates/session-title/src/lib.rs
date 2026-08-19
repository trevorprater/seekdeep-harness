//! Session-title domain: title normalization and the durable title-source
//! invariant. The model/provider service lives in the deferred service module.

pub mod client;
pub mod invariant;
pub mod model;
pub mod normalize;
pub mod types;

pub use model::{
    Config as SessionTitleConfig, SessionTitleAutomaticMode, SessionTitleEventData,
    SessionTitleModelProvenance, SessionTitleProviderId, SessionTitleSnapshot, SessionTitleSource,
    SessionTitleUserMessage, fold_session_title,
};
pub use normalize::{fallback_session_title, normalize_session_title, truncate_title_utf8};
pub use types::TitleProjection;

/// Cordis plugin name used by loader diagnostics.
pub const NAME: &str = "session-title";
