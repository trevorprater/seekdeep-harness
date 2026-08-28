//! Portable models and compiled browser primitives shared by the `SeekDeep` UI.

mod ansi;
#[cfg(target_arch = "wasm32")]
mod browser_atoms;
#[cfg(target_arch = "wasm32")]
mod browser_blocks;
#[cfg(target_arch = "wasm32")]
mod browser_code_block;
#[cfg(target_arch = "wasm32")]
mod browser_dialogs;
#[cfg(target_arch = "wasm32")]
mod browser_highlight;
#[cfg(target_arch = "wasm32")]
mod browser_hover_card;
#[cfg(target_arch = "wasm32")]
mod browser_icons;
#[cfg(target_arch = "wasm32")]
mod browser_json_tree;
#[cfg(target_arch = "wasm32")]
mod browser_markdown_atoms;
#[cfg(target_arch = "wasm32")]
mod browser_menu;
#[cfg(target_arch = "wasm32")]
mod browser_read_block;
#[cfg(target_arch = "wasm32")]
mod browser_tooltip;
#[cfg(target_arch = "wasm32")]
mod browser_util;
#[cfg(target_arch = "wasm32")]
mod browser_web;
mod head_tail_cap;
#[allow(clippy::needless_raw_string_hashes, clippy::unreadable_literal)]
mod icon_data;
mod markdown;

pub use ansi::*;
#[cfg(target_arch = "wasm32")]
pub use browser_atoms::*;
#[cfg(target_arch = "wasm32")]
pub use browser_blocks::*;
#[cfg(target_arch = "wasm32")]
pub use browser_code_block::*;
#[cfg(target_arch = "wasm32")]
pub use browser_dialogs::*;
#[cfg(target_arch = "wasm32")]
pub use browser_highlight::*;
#[cfg(target_arch = "wasm32")]
pub use browser_hover_card::*;
#[cfg(target_arch = "wasm32")]
pub use browser_icons::*;
#[cfg(target_arch = "wasm32")]
pub use browser_json_tree::*;
#[cfg(target_arch = "wasm32")]
pub use browser_markdown_atoms::*;
#[cfg(target_arch = "wasm32")]
pub use browser_menu::*;
#[cfg(target_arch = "wasm32")]
pub use browser_read_block::*;
#[cfg(target_arch = "wasm32")]
pub use browser_tooltip::*;
#[cfg(target_arch = "wasm32")]
pub use browser_util::*;
#[cfg(target_arch = "wasm32")]
pub use browser_web::*;
pub use head_tail_cap::*;
pub use icon_data::*;
pub use markdown::*;
