//! Human-facing `/goal` command over the persisted same-session goal domain.

pub mod index;
pub mod invariant;

pub use index::{INJECT, NAME, apply, plugin};
pub use invariant::register_invariant;
