//! Local filesystem backend for the seekdeep-fs seam.

pub mod fsio;
pub mod index;
pub mod invariant;
pub mod win32;

pub use index::{Config, LocalFileSystem, config_schema, plugin};
pub use invariant::register_invariant;
