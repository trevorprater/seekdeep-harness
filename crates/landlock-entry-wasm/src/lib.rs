//! Node entry bindings for the compiled Rust Landlock package contract.

#[cfg(target_arch = "wasm32")]
mod wasm;

#[cfg(target_arch = "wasm32")]
pub use wasm::{
    configure_bindings, grant_args, launcher_bin, launcher_failure_exit, launcher_path, probe,
};
