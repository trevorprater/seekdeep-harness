//! Narrow Win32 file-security and atomic-replacement primitives.
//!
//! # Safety boundary
//!
//! Win32 requires raw pointers for security descriptors and UTF-16 paths.
//! This crate NUL-terminates owned path buffers, keeps them alive across each
//! call, sizes the descriptor before allocation, and exposes only owned Rust
//! buffers and [`std::io::Result`] to the safe filesystem implementation.

#![allow(unsafe_code)]

use std::{io, path::Path};

#[cfg(windows)]
fn wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt as _;
    namespaced(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
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

/// Reads an existing file's self-relative DACL security descriptor.
///
/// # Errors
///
/// Returns the exact Win32 last-error code through [`io::Error`].
#[cfg(windows)]
pub fn read_file_dacl(path: &Path) -> io::Result<Vec<u8>> {
    use windows_sys::Win32::{
        Foundation::GetLastError,
        Security::{DACL_SECURITY_INFORMATION, GetFileSecurityW},
    };

    let path = wide(path);
    let mut needed = 0_u32;
    // SAFETY: path is a live NUL-terminated UTF-16 buffer; the null descriptor
    // and zero length are the documented size probe, and needed is writable.
    let _ = unsafe {
        GetFileSecurityW(
            path.as_ptr(),
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            0,
            &raw mut needed,
        )
    };
    if needed == 0 {
        // SAFETY: GetLastError has no preconditions and is read immediately
        // after the failing thread-local API call.
        let code = unsafe { GetLastError() };
        return Err(io::Error::from_raw_os_error(code.cast_signed()));
    }
    let mut descriptor = vec![0_u8; needed as usize];
    let descriptor_len = u32::try_from(descriptor.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Win32 security descriptor length exceeded u32",
        )
    })?;
    // SAFETY: descriptor owns at least needed writable bytes, path remains
    // live and terminated, and needed remains a valid output pointer.
    let read = unsafe {
        GetFileSecurityW(
            path.as_ptr(),
            DACL_SECURITY_INFORMATION,
            descriptor.as_mut_ptr().cast(),
            descriptor_len,
            &raw mut needed,
        )
    };
    if read == 0 {
        // SAFETY: read immediately after the failing call above.
        let code = unsafe { GetLastError() };
        return Err(io::Error::from_raw_os_error(code.cast_signed()));
    }
    descriptor.truncate(needed as usize);
    Ok(descriptor)
}

/// Copies and protects an existing file's DACL onto an empty staging file.
///
/// # Errors
///
/// Returns descriptor-read or Win32 installation failures.
#[cfg(windows)]
pub fn copy_file_dacl(source: &Path, destination: &Path) -> io::Result<()> {
    use windows_sys::Win32::{
        Foundation::GetLastError,
        Security::{
            DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, SetFileSecurityW,
        },
    };

    let descriptor = read_file_dacl(source)?;
    let destination = wide(destination);
    // SAFETY: destination is live and NUL-terminated; descriptor is a live
    // self-relative security descriptor returned by GetFileSecurityW.
    let installed = unsafe {
        SetFileSecurityW(
            destination.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor.as_ptr().cast_mut().cast(),
        )
    };
    if installed != 0 {
        return Ok(());
    }
    // SAFETY: read immediately after the failing call above.
    let code = unsafe { GetLastError() };
    Err(io::Error::from_raw_os_error(code.cast_signed()))
}

/// Replaces an existing file while preserving its security and replace metadata.
///
/// # Errors
///
/// Returns the exact Win32 last-error code through [`io::Error`].
#[cfg(windows)]
pub fn replace_file(replaced: &Path, replacement: &Path) -> io::Result<()> {
    use windows_sys::Win32::{Foundation::GetLastError, Storage::FileSystem::ReplaceFileW};

    let replaced = wide(replaced);
    let replacement = wide(replacement);
    // SAFETY: both paths are live NUL-terminated buffers. Optional pointers
    // are null and flags are zero, matching the documented metadata-preserving
    // replacement call.
    let replaced = unsafe {
        ReplaceFileW(
            replaced.as_ptr(),
            replacement.as_ptr(),
            std::ptr::null(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if replaced != 0 {
        return Ok(());
    }
    // SAFETY: read immediately after the failing call above.
    let code = unsafe { GetLastError() };
    Err(io::Error::from_raw_os_error(code.cast_signed()))
}

/// Non-Windows builds cannot read Win32 DACLs.
///
/// # Errors
///
/// Always returns [`io::ErrorKind::Unsupported`].
#[cfg(not(windows))]
pub fn read_file_dacl(_path: &Path) -> io::Result<Vec<u8>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Win32 DACL APIs are available only on Windows",
    ))
}

/// Non-Windows builds cannot copy Win32 DACLs.
///
/// # Errors
///
/// Always returns [`io::ErrorKind::Unsupported`].
#[cfg(not(windows))]
pub fn copy_file_dacl(_source: &Path, _destination: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Win32 DACL APIs are available only on Windows",
    ))
}

/// Non-Windows builds cannot call `ReplaceFileW`.
///
/// # Errors
///
/// Always returns [`io::ErrorKind::Unsupported`].
#[cfg(not(windows))]
pub fn replace_file(_replaced: &Path, _replacement: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "ReplaceFileW is available only on Windows",
    ))
}
