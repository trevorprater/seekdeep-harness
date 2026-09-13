//! Safe Windows namespace error, race, and staging policy.

use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// `MOVEFILE_WRITE_THROUGH`, without replace or copy fallbacks.
pub const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;

/// Node-compatible error category derived from a Win32 last-error code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Win32Errno {
    /// Missing path.
    NoEntry,
    /// Access refused.
    Access,
    /// Cross-volume move refused.
    CrossDevice,
    /// Destination already exists.
    Exists,
    /// Invalid path name.
    Invalid,
    /// Unclassified I/O failure.
    Io,
}

impl Win32Errno {
    /// Node-style error code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoEntry => "ENOENT",
            Self::Access => "EACCES",
            Self::CrossDevice => "EXDEV",
            Self::Exists => "EEXIST",
            Self::Invalid => "EINVAL",
            Self::Io => "EIO",
        }
    }
}

/// Structured Win32 publication failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Win32NamespaceError {
    /// Node-style category.
    pub code: Win32Errno,
    /// Raw Win32 code.
    pub win32_code: u32,
    /// Source staging path.
    pub path: PathBuf,
    /// Destination path.
    pub dest: PathBuf,
}

impl fmt::Display for Win32NamespaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "MoveFileExW {} (Win32 {}): {} -> {}",
            self.code.as_str(),
            self.win32_code,
            self.path.display(),
            self.dest.display()
        )
    }
}

impl std::error::Error for Win32NamespaceError {}

/// Injectable `MoveFileExW` seam used by cross-platform unit tests.
pub trait Win32Move: Send + Sync {
    /// Moves one staging object to a new destination with write-through flags.
    ///
    /// # Errors
    ///
    /// Returns the exact structured last-error failure.
    fn move_new(
        &self,
        existing: &Path,
        replacement: &Path,
        flags: u32,
    ) -> Result<(), Win32NamespaceError>;
}

/// Maps one raw last-error code.
#[must_use]
pub const fn errno_code(win32_code: u32) -> Win32Errno {
    match win32_code {
        2 | 3 => Win32Errno::NoEntry,
        5 => Win32Errno::Access,
        17 => Win32Errno::CrossDevice,
        80 | 183 => Win32Errno::Exists,
        123 => Win32Errno::Invalid,
        _ => Win32Errno::Io,
    }
}

/// Constructs the complete publication error shape.
#[must_use]
pub fn win32_error(
    win32_code: u32,
    path: impl Into<PathBuf>,
    dest: impl Into<PathBuf>,
) -> Win32NamespaceError {
    Win32NamespaceError {
        code: errno_code(win32_code),
        win32_code,
        path: path.into(),
        dest: dest.into(),
    }
}

/// Publishes one staging object through an injected native seam.
///
/// # Errors
///
/// Returns the seam's exact structured failure.
pub fn publish_new_file_with(
    mover: &dyn Win32Move,
    existing: &Path,
    replacement: &Path,
) -> Result<(), Win32NamespaceError> {
    mover.move_new(existing, replacement, MOVEFILE_WRITE_THROUGH)
}

/// Creates every missing directory through short staging siblings.
///
/// # Errors
///
/// Returns non-directory ancestors, staging failures, or publication failures;
/// an already-existing winner is accepted only when it is a directory.
pub fn ensure_durable_directory_with(mover: &dyn Win32Move, target: &Path) -> anyhow::Result<()> {
    let absolute = if target.is_absolute() {
        target.to_path_buf()
    } else {
        std::env::current_dir()?.join(target)
    };
    let mut missing = Vec::new();
    let mut current = absolute.as_path();
    loop {
        if assert_directory(current)? {
            break;
        }
        missing.push(current.to_path_buf());
        current = current.parent().ok_or_else(|| {
            anyhow::anyhow!(
                "directory path has no existing root: {}",
                absolute.display()
            )
        })?;
    }
    for directory in missing.into_iter().rev() {
        let parent = directory.parent().ok_or_else(|| {
            anyhow::anyhow!("missing directory has no parent: {}", directory.display())
        })?;
        create_leaf_directory(mover, parent, &directory)?;
    }
    Ok(())
}

/// Production write-through publication.
///
/// # Errors
///
/// Returns the exact Win32 namespace failure. Non-Windows callers receive an
/// unsupported error and should never select this path.
pub fn publish_new_file_win32(existing: &Path, replacement: &Path) -> anyhow::Result<()> {
    publish_new_file_with(&SystemMove, existing, replacement)?;
    Ok(())
}

/// Production durable directory creation.
///
/// # Errors
///
/// Returns path, staging, or exact Win32 publication failures.
pub fn ensure_durable_directory_win32(target: &Path) -> anyhow::Result<()> {
    ensure_durable_directory_with(&SystemMove, target)
}

struct SystemMove;

impl Win32Move for SystemMove {
    fn move_new(
        &self,
        existing: &Path,
        replacement: &Path,
        flags: u32,
    ) -> Result<(), Win32NamespaceError> {
        debug_assert_eq!(flags, MOVEFILE_WRITE_THROUGH);
        #[cfg(windows)]
        {
            seekdeep_win32_durable_namespace::move_new_write_through(existing, replacement).map_err(
                |error| {
                    let code = error.raw_os_error().map_or(0, |code| code.cast_unsigned());
                    win32_error(code, existing, replacement)
                },
            )
        }
        #[cfg(not(windows))]
        {
            Err(win32_error(9999, existing, replacement))
        }
    }
}

fn assert_directory(path: &Path) -> io::Result<bool> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(true),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("path exists but is not a directory: {}", path.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn create_leaf_directory(
    mover: &dyn Win32Move,
    parent: &Path,
    target: &Path,
) -> anyhow::Result<()> {
    static NEXT_STAGING: AtomicU64 = AtomicU64::new(0);
    let staging = loop {
        let id = NEXT_STAGING.fetch_add(1, Ordering::Relaxed);
        let staging = parent.join(format!(".seekdeep-mkdir-{}-{id}", std::process::id()));
        match fs::create_dir(&staging) {
            Ok(()) => break staging,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    };
    match publish_new_file_with(mover, &staging, target) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            if error.code == Win32Errno::Exists && assert_directory(target)? {
                Ok(())
            } else {
                Err(error.into())
            }
        }
    }
}
