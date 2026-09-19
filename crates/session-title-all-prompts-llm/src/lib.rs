//! All-human-messages model provider for the session-title service.

pub mod index;
pub mod invariant;

pub use index::{INJECT, NAME, apply, config_schema, plugin};
