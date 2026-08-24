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

const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;

/// Safe host boundary used by the checked Win32 call orchestration.
pub trait Win32FileApi {
    /// Probes or fills one self-relative security descriptor.
    fn get_file_security(
        &self,
        path: &str,
        requested: u32,
        descriptor: Option<&mut [u8]>,
        needed: &mut u32,
    ) -> bool;

    /// Applies one self-relative security descriptor.
    fn set_file_security(&self, path: &str, information: u32, descriptor: &[u8]) -> bool;

    /// Replaces one file with another on the same volume.
    fn replace_file(&self, replaced: &str, replacement: &str) -> bool;

    /// Returns the thread-local error from the immediately preceding call.
    fn last_error(&self) -> u32;
}

/// Converts a Windows path to the extended-length spelling passed to Win32.
#[must_use]
pub fn namespaced_path(path: &Path) -> String {
    let rendered = path.as_os_str().to_string_lossy();
    if rendered.starts_with(r"\\?\") {
        return rendered.into_owned();
    }
    if let Some(unc) = rendered.strip_prefix(r"\\") {
        return format!(r"\\?\UNC\{unc}");
    }
    format!(r"\\?\{rendered}")
}

fn last_error(api: &dyn Win32FileApi) -> io::Error {
    io::Error::from_raw_os_error(api.last_error().cast_signed())
}

/// Reads a DACL through an injected Win32 host boundary.
///
/// # Errors
///
/// Returns the boundary's exact last-error code.
pub fn read_file_dacl_with(api: &dyn Win32FileApi, path: &Path) -> io::Result<Vec<u8>> {
    let path = namespaced_path(path);
    let mut needed = 0_u32;
    let _ = api.get_file_security(&path, DACL_SECURITY_INFORMATION, None, &mut needed);
    if needed == 0 {
        return Err(last_error(api));
    }
    let capacity = usize::try_from(needed).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Win32 security descriptor length exceeded usize",
        )
    })?;
    let mut descriptor = vec![0_u8; capacity];
    if !api.get_file_security(
        &path,
        DACL_SECURITY_INFORMATION,
        Some(&mut descriptor),
        &mut needed,
    ) {
        return Err(last_error(api));
    }
    let length = usize::try_from(needed).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Win32 security descriptor length exceeded usize",
        )
    })?;
    if length > descriptor.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Win32 security descriptor grew beyond the probed length",
        ));
    }
    descriptor.truncate(length);
    Ok(descriptor)
}

/// Copies and protects a DACL through an injected Win32 host boundary.
///
/// # Errors
///
/// Returns descriptor-read or installation failures.
pub fn copy_file_dacl_with(
    api: &dyn Win32FileApi,
    source: &Path,
    destination: &Path,
) -> io::Result<()> {
    let descriptor = read_file_dacl_with(api, source)?;
    if api.set_file_security(
        &namespaced_path(destination),
        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
        &descriptor,
    ) {
        Ok(())
    } else {
        Err(last_error(api))
    }
}

/// Replaces a file through an injected Win32 host boundary.
///
/// # Errors
///
/// Returns the boundary's exact last-error code.
pub fn replace_file_with(
    api: &dyn Win32FileApi,
    replaced: &Path,
    replacement: &Path,
) -> io::Result<()> {
    if api.replace_file(&namespaced_path(replaced), &namespaced_path(replacement)) {
        Ok(())
    } else {
        Err(last_error(api))
    }
}

#[cfg(windows)]
#[derive(Debug)]
struct SystemApi;

#[cfg(windows)]
fn wide(path: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt as _;
    std::ffi::OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(windows)]
impl Win32FileApi for SystemApi {
    fn get_file_security(
        &self,
        path: &str,
        requested: u32,
        descriptor: Option<&mut [u8]>,
        needed: &mut u32,
    ) -> bool {
        use windows_sys::Win32::Security::GetFileSecurityW;

        let path = wide(path);
        let (descriptor, length) = descriptor.map_or((std::ptr::null_mut(), 0), |descriptor| {
            let length = u32::try_from(descriptor.len()).unwrap_or(u32::MAX);
            (descriptor.as_mut_ptr().cast(), length)
        });
        // SAFETY: path is NUL-terminated and live; descriptor is either null
        // for the size probe or points to a live writable slice of length.
        unsafe { GetFileSecurityW(path.as_ptr(), requested, descriptor, length, needed) != 0 }
    }

    fn set_file_security(&self, path: &str, information: u32, descriptor: &[u8]) -> bool {
        use windows_sys::Win32::Security::SetFileSecurityW;

        let path = wide(path);
        // SAFETY: path is NUL-terminated and live, and descriptor remains live
        // for the complete call after being returned by GetFileSecurityW.
        unsafe {
            SetFileSecurityW(
                path.as_ptr(),
                information,
                descriptor.as_ptr().cast_mut().cast(),
            ) != 0
        }
    }

    fn replace_file(&self, replaced: &str, replacement: &str) -> bool {
        use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;

        let replaced = wide(replaced);
        let replacement = wide(replacement);
        // SAFETY: both buffers are NUL-terminated and live; all optional
        // pointers are null and flags are zero.
        unsafe {
            ReplaceFileW(
                replaced.as_ptr(),
                replacement.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                std::ptr::null(),
            ) != 0
        }
    }

    fn last_error(&self) -> u32 {
        use windows_sys::Win32::Foundation::GetLastError;

        // SAFETY: GetLastError has no preconditions; callers invoke this
        // immediately after the failing thread-local Win32 call.
        unsafe { GetLastError() }
    }
}

/// Reads an existing file's self-relative DACL security descriptor.
///
/// # Errors
///
/// Returns the exact Win32 last-error code through [`io::Error`].
#[cfg(windows)]
pub fn read_file_dacl(path: &Path) -> io::Result<Vec<u8>> {
    read_file_dacl_with(&SystemApi, path)
}

/// Copies and protects an existing file's DACL onto an empty staging file.
///
/// # Errors
///
/// Returns descriptor-read or Win32 installation failures.
#[cfg(windows)]
pub fn copy_file_dacl(source: &Path, destination: &Path) -> io::Result<()> {
    copy_file_dacl_with(&SystemApi, source, destination)
}

/// Replaces an existing file while preserving its security and replace metadata.
///
/// # Errors
///
/// Returns the exact Win32 last-error code through [`io::Error`].
#[cfg(windows)]
pub fn replace_file(replaced: &Path, replacement: &Path) -> io::Result<()> {
    replace_file_with(&SystemApi, replaced, replacement)
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
