//! Human-facing `/compact` command over the backend-independent compaction seam.

pub mod index;
pub mod invariant;

pub use index::{NAME, apply};
pub use invariant::register_invariant;
