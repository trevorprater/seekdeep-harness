//! Narrow native Win32 adapter for the safe Windows ACL state machines.
//!
//! # Safety boundary
//!
//! `windows-sys` exposes raw pointers and every relevant system call as
//! `unsafe`. This crate is the sole owner of those calls: it validates slice
//! bounds, NUL-terminates UTF-16 inputs, converts handles through typed safe
//! newtypes, and copies kernel-owned ACL/SID data into bounded Rust buffers
//! before returning to the safe orchestration crate. No other workspace crate
//! needs or permits unsafe code.

#![allow(unsafe_code)]
#![deny(missing_docs, unreachable_pub)]
#![deny(clippy::all, clippy::pedantic)]
#![allow(clippy::module_name_repetitions, clippy::must_use_candidate)]

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::WindowsBindings;

/// Returns whether this build contains the native Win32 adapter.
#[must_use]
pub const fn is_supported_build() -> bool {
    cfg!(windows)
}
