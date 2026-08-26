//! Portable models and compiled browser primitives shared by the `SeekDeep` UI.

mod ansi;
#[cfg(target_arch = "wasm32")]
mod browser_atoms;
#[cfg(target_arch = "wasm32")]
mod browser_dialogs;
#[cfg(target_arch = "wasm32")]
mod browser_icons;
#[cfg(target_arch = "wasm32")]
mod browser_tooltip;
#[cfg(target_arch = "wasm32")]
mod browser_util;
mod head_tail_cap;
#[allow(clippy::needless_raw_string_hashes, clippy::unreadable_literal)]
mod icon_data;
mod markdown;

pub use ansi::*;
#[cfg(target_arch = "wasm32")]
pub use browser_atoms::*;
#[cfg(target_arch = "wasm32")]
pub use browser_dialogs::*;
#[cfg(target_arch = "wasm32")]
pub use browser_icons::*;
#[cfg(target_arch = "wasm32")]
pub use browser_tooltip::*;
#[cfg(target_arch = "wasm32")]
pub use browser_util::*;
pub use head_tail_cap::*;
pub use icon_data::*;
pub use markdown::*;
