//! File identity and change timestamps absent from stable Rust's Windows metadata API.

use std::{io, path::Path};

/// Opaque file version carrying Node's device, inode, size, mtime, and ctime fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileVersion(String);

impl FileVersion {
    #[cfg(any(windows, test))]
    fn from_fields(
        device: u32,
        inode: u64,
        size: u64,
        modification_ticks: i64,
        change_ticks: i64,
    ) -> Self {
        let unix_nanos = |ticks: i64| (i128::from(ticks) - 116_444_736_000_000_000) * 100;
        Self(format!(
            "{device}:{inode}:{size}:{}:{}",
            unix_nanos(modification_ticks),
            unix_nanos(change_ticks),
        ))
    }

    /// Source-compatible colon-delimited decimal representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Reads one file's identity and nanosecond timestamps through an owned Windows handle.
///
/// `follow_symlinks` selects `stat` or `lstat` identity. Directories are accepted.
///
/// # Errors
/// Returns open failures or the exact Win32 error from a failed metadata query.
#[cfg(windows)]
pub fn file_version(path: &Path, follow_symlinks: bool) -> io::Result<FileVersion> {
    use std::os::windows::{fs::OpenOptionsExt as _, io::AsRawHandle as _};
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FileBasicInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
    };

    let mut flags = FILE_FLAG_BACKUP_SEMANTICS;
    if !follow_symlinks {
        flags |= FILE_FLAG_OPEN_REPARSE_POINT;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(flags)
        .open(path)?;
    let mut identity = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the owned file keeps its handle alive, and the output points to a
    // live, correctly sized and aligned BY_HANDLE_FILE_INFORMATION value.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut identity) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut basic = FILE_BASIC_INFO::default();
    let size = u32::try_from(std::mem::size_of::<FILE_BASIC_INFO>()).map_err(io::Error::other)?;
    // SAFETY: FileBasicInfo selects exactly FILE_BASIC_INFO; the writable buffer
    // has that size and alignment, and the owned file is live for the call.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileBasicInfo,
            (&raw mut basic).cast(),
            size,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let inode = (u64::from(identity.nFileIndexHigh) << 32) | u64::from(identity.nFileIndexLow);
    let length = (u64::from(identity.nFileSizeHigh) << 32) | u64::from(identity.nFileSizeLow);
    Ok(FileVersion::from_fields(
        identity.dwVolumeSerialNumber,
        inode,
        length,
        basic.LastWriteTime,
        basic.ChangeTime,
    ))
}

/// Non-Windows builds cannot query Win32 file identity.
///
/// # Errors
/// Always returns [`io::ErrorKind::Unsupported`].
#[cfg(not(windows))]
pub fn file_version(_path: &Path, _follow_symlinks: bool) -> io::Result<FileVersion> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Win32 file metadata is available only on Windows",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_conversion_preserves_pre_epoch_dates_and_full_width_identifiers() {
        let version = FileVersion::from_fields(
            u32::MAX,
            u64::MAX,
            4_294_967_297,
            116_444_735_999_999_999,
            116_444_736_000_000_001,
        );
        assert_eq!(
            version.as_str(),
            "4294967295:18446744073709551615:4294967297:-100:100"
        );
        for (ticks, nanos) in [
            (i64::MIN, "-933981677285477580800"),
            (0, "-11644473600000000000"),
            (116_444_736_000_000_000, "0"),
            (i64::MAX, "910692730085477580700"),
        ] {
            assert_eq!(
                FileVersion::from_fields(0, 0, 0, ticks, ticks).as_str(),
                format!("0:0:0:{nanos}:{nanos}")
            );
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn unsupported_hosts_reject_both_stat_and_lstat() {
        for follow_symlinks in [false, true] {
            let error = file_version(Path::new("unopened"), follow_symlinks).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
            assert_eq!(
                error.to_string(),
                "Win32 file metadata is available only on Windows"
            );
        }
    }
}
