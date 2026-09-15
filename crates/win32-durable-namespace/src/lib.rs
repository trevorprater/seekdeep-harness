//! Narrow Win32 durable namespace publication.
//!
//! This crate is the workspace's single unsafe exception for `MoveFileExW`.
//! The API receives owned Rust paths, converts them to NUL-terminated UTF-16,
//! keeps those buffers alive for the complete call, passes only
//! `MOVEFILE_WRITE_THROUGH` (never replace-existing or cross-volume copy), and
//! reads `GetLastError` immediately when the call reports failure.

use std::{io, path::Path};

/// Publishes a new path with write-through namespace semantics.
///
/// # Errors
///
/// Returns the exact Win32 last-error code through [`io::Error`].
#[cfg(windows)]
pub fn move_new_write_through(existing: &Path, replacement: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::{
        Foundation::GetLastError,
        Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW},
    };

    let existing = namespaced(existing)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let replacement = namespaced(replacement)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: both pointers reference live NUL-terminated UTF-16 buffers for
    // the duration of the call. Flags request write-through only, so the API
    // cannot replace an existing target or fall back to a cross-volume copy.
    let moved = unsafe {
        MoveFileExW(
            existing.as_ptr(),
            replacement.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved != 0 {
        return Ok(());
    }
    // SAFETY: GetLastError has no preconditions and is read immediately after
    // the failing thread-local Win32 call above.
    let code = unsafe { GetLastError() };
    Err(io::Error::from_raw_os_error(code.cast_signed()))
}

#[cfg(windows)]
fn namespaced(path: &Path) -> std::ffi::OsString {
    let rendered = path.as_os_str().to_string_lossy();
    if rendered.starts_with(r"\\?\") {
        return path.as_os_str().to_owned();
    }
    if let Some(unc) = rendered.strip_prefix(r"\\") {
        return std::ffi::OsString::from(format!(r"\\?\UNC\{unc}"));
    }
    std::ffi::OsString::from(format!(r"\\?\{rendered}"))
}

/// Non-Windows builds never expose the native operation.
///
/// # Errors
///
/// Always returns [`io::ErrorKind::Unsupported`].
#[cfg(not(windows))]
pub fn move_new_write_through(_existing: &Path, _replacement: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "MoveFileExW is available only on Windows",
    ))
}
