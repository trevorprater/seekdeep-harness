//! One test binary per crate: every top-level test file is a module here.

mod deepseek_e2e;
mod deepseek_spec;
mod redirect_spec;
mod settings_spec;

#[path = "support/mod.rs"]
mod support;
