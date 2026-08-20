//! Model-facing foreground Ralph loop over the workflow and subagent seams.

pub mod index;
pub mod invariant;

pub use index::{Config, INJECT, NAME, apply};
pub use invariant::register_invariant;
