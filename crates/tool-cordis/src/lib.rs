//! Model-facing dynamic Cordis tools and generated Host inspection catalogs.

/// Generated Host Service/Event catalog projections.
pub mod api_catalog;
/// Tool registration, execution, and explicit plugin-reference injection.
pub mod index;
/// Runtime fiber/service/tool inspection renderers.
pub mod inspect;
/// Replay-safe tool presentation.
pub mod present;
/// Model-facing dynamic Cordis guidance.
pub mod prompt;
/// First-party Host inspection providers.
pub mod providers;

pub use index::{INJECT, NAME, apply, plugin};
pub use prompt::cordis_system_prompt;
