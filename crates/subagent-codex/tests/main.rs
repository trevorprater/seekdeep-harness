//! One test binary per crate: every top-level test file is a module here.

mod loader_composition;
mod real_deepseek_e2e;
mod real_product;
mod run_parity;
mod wire_parity;

#[cfg(unix)]
#[path = "support/mod.rs"]
mod support;
