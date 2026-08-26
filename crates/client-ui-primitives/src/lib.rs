//! Portable models and compiled browser primitives shared by the `SeekDeep` UI.

mod ansi;
#[cfg(target_arch = "wasm32")]
mod browser_atoms;
#[cfg(target_arch = "wasm32")]
mod browser_util;
mod head_tail_cap;
mod markdown;

pub use ansi::*;
#[cfg(target_arch = "wasm32")]
pub use browser_atoms::*;
#[cfg(target_arch = "wasm32")]
pub use browser_util::*;
pub use head_tail_cap::*;
pub use markdown::*;
