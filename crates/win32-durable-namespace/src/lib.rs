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
/// Returns path normalization failures or the exact Win32 last-error code through [`io::Error`].
#[cfg(windows)]
pub fn move_new_write_through(existing: &Path, replacement: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::{
        Foundation::GetLastError,
        Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW},
    };

    let existing = namespaced(existing)?
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let replacement = namespaced(replacement)?
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
fn namespaced(path: &Path) -> io::Result<std::ffi::OsString> {
    use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};

    // Verbatim paths disable Win32's slash and dot-segment normalization.
    let absolute = std::path::absolute(path)?;
    let units = absolute.as_os_str().encode_wide().collect::<Vec<_>>();
    if units.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains NUL",
        ));
    }
    if units.starts_with(&[92, 92, 63, 92]) {
        return Ok(absolute.into_os_string());
    }
    let namespaced = if units.starts_with(&[92, 92]) {
        let mut prefix = std::ffi::OsString::from(r"\\?\UNC\");
        prefix.push(std::ffi::OsString::from_wide(&units[2..]));
        prefix
    } else {
        let mut prefix = std::ffi::OsString::from(r"\\?\");
        prefix.push(absolute);
        prefix
    };
    Ok(namespaced)
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

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn publishes_mixed_separator_paths_without_replacing_an_existing_destination() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("nested");
        std::fs::create_dir(&parent).unwrap();
        let source = parent.join("first.txt");
        let destination = directory.path().join("published.txt");
        std::fs::write(&source, "first").unwrap();
        move_new_write_through(
            &parent.join("./first.txt"),
            &parent.join("../published.txt"),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&destination).unwrap(), "first");
        assert!(!source.exists());
        std::fs::write(&source, "second").unwrap();
        assert!(move_new_write_through(&source, &destination).is_err());
        assert_eq!(std::fs::read_to_string(source).unwrap(), "second");
        assert_eq!(std::fs::read_to_string(destination).unwrap(), "first");
    }

    #[test]
    fn namespace_conversion_preserves_utf16_and_rejects_nul() {
        use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};

        let mut raw = r"C:\root\".encode_utf16().collect::<Vec<_>>();
        raw.push(0xd800);
        let input = std::ffi::OsString::from_wide(&raw);
        let actual = namespaced(Path::new(&input)).unwrap();
        let expected = r"\\?\".encode_utf16().chain(raw).collect::<Vec<_>>();
        assert_eq!(actual.encode_wide().collect::<Vec<_>>(), expected);
        assert_eq!(
            namespaced(Path::new("C:\\root\\file\0suffix"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
