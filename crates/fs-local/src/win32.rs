//! Windows security-descriptor helpers for atomic local-file replacement.

#[cfg(windows)]
mod imp {
    use std::ffi::OsStr;
    use std::io;
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "advapi32")]
    extern "system" {
        fn GetFileSecurityW(
            path: *const u16,
            requested: u32,
            descriptor: *mut u8,
            length: u32,
            needed: *mut u32,
        ) -> i32;
        fn SetFileSecurityW(path: *const u16, information: u32, descriptor: *const u8) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn ReplaceFileW(
            replaced: *const u16,
            replacement: *const u16,
            backup: *const u16,
            flags: u32,
            exclude: *mut std::ffi::c_void,
            reserved: *mut std::ffi::c_void,
        ) -> i32;
        fn GetLastError() -> u32;
    }

    const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
    const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;
    const ERROR_FILE_NOT_FOUND: u32 = 2;
    const ERROR_PATH_NOT_FOUND: u32 = 3;
    const ERROR_ACCESS_DENIED: u32 = 5;

    fn wide(path: &str) -> Vec<u16> {
        OsStr::new(path).encode_wide().chain(Some(0)).collect()
    }

    fn last_error() -> io::Error {
        io::Error::from_raw_os_error(unsafe { GetLastError() } as i32)
    }

    /// Reads a file's self-relative DACL security descriptor.
    pub fn read_file_dacl_win32(path: &str) -> io::Result<Vec<u8>> {
        let native = wide(path);
        let mut needed: u32 = 0;
        let probe = unsafe {
            GetFileSecurityW(
                native.as_ptr(),
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                0,
                &mut needed,
            )
        };
        if probe != 0 || needed == 0 {
            return Err(last_error());
        }
        let mut descriptor = vec![0u8; needed as usize];
        let result = unsafe {
            GetFileSecurityW(
                native.as_ptr(),
                DACL_SECURITY_INFORMATION,
                descriptor.as_mut_ptr(),
                descriptor.len() as u32,
                &mut needed,
            )
        };
        if result == 0 {
            return Err(last_error());
        }
        descriptor.truncate(needed as usize);
        Ok(descriptor)
    }

    /// Copies an existing file's DACL onto another file and protects it from
    /// staging-parent inheritance.
    pub fn copy_file_dacl_win32(source: &str, destination: &str) -> io::Result<()> {
        let descriptor = read_file_dacl_win32(source)?;
        let information = DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;
        let result = unsafe {
            SetFileSecurityW(wide(destination).as_ptr(), information, descriptor.as_ptr())
        };
        if result == 0 {
            Err(last_error())
        } else {
            Ok(())
        }
    }

    /// Replaces a Windows file while preserving the replaced file's ACL and other
    /// replace metadata.
    pub fn replace_file_win32(replaced: &str, replacement: &str) -> io::Result<()> {
        let result = unsafe {
            ReplaceFileW(
                wide(replaced).as_ptr(),
                wide(replacement).as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if result == 0 {
            Err(last_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(windows)]
pub use imp::{copy_file_dacl_win32, read_file_dacl_win32, replace_file_win32};

/// Reads a file's self-relative DACL security descriptor (Windows-only no-op).
///
/// # Errors
///
/// Always returns an unsupported error on non-Windows.
#[cfg(not(windows))]
pub fn read_file_dacl_win32(_path: &str) -> std::io::Result<Vec<u8>> {
    Err(std::io::Error::other(
        "win32 helpers unavailable on non-Windows",
    ))
}

/// Copies an existing file's DACL onto another file (Windows-only no-op).
///
/// # Errors
///
/// Always returns an unsupported error on non-Windows.
#[cfg(not(windows))]
pub fn copy_file_dacl_win32(_source: &str, _destination: &str) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "win32 helpers unavailable on non-Windows",
    ))
}

/// Replaces a Windows file preserving its ACL (Windows-only no-op).
///
/// # Errors
///
/// Always returns an unsupported error on non-Windows.
#[cfg(not(windows))]
pub fn replace_file_win32(_replaced: &str, _replacement: &str) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "win32 helpers unavailable on non-Windows",
    ))
}
