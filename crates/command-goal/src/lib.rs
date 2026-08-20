//! Human-facing `/goal` command over the persisted same-session goal domain.

pub mod index;
pub mod invariant;

pub use index::{NAME, apply};
pub use invariant::register_invariant;
