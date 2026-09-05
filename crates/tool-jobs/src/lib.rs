//! Model-facing background job control tools over the `ctx.jobs` registry.

pub mod index;
pub mod invariant;

pub use index::{CompletionDelivery, Config, NAME, apply, config_schema, status_line};
pub use invariant::register_invariant;
