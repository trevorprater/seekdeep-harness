//! One test binary per crate: every top-level test file is a module here.

mod exa_e2e;
mod exa_spec;

#[path = "support/mod.rs"]
mod support;
