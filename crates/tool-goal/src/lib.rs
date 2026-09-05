//! Model-facing goal control tools over the persisted same-session goal domain.

pub mod authority;
pub mod index;
pub mod invariant;
pub mod wrapup;

pub use index::{Config, NAME, apply, config_schema};
pub use invariant::register_invariant;
