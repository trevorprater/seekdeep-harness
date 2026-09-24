//! One test binary per crate: every top-level test file is a module here.

mod build_parity;
mod entry_wasm_parity;
mod packed_install_parity;
mod process_capture_parity;
mod publish_parity;
mod release_parity;

#[path = "release_support/mod.rs"]
mod release_support;
