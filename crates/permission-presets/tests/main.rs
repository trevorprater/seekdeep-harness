//! One test binary per crate: every top-level test file is a module here.

mod invariant_parity;
mod permission_presets_parity;
mod projection_parity;

#[path = "support/mod.rs"]
mod support;
