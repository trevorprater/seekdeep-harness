//! One test binary per crate: every top-level test file is a module here.

mod claude_code_parity;
mod loader_composition;
mod real_deepseek_e2e;
mod real_product;

#[cfg(unix)]
#[path = "support/mod.rs"]
mod support;
