//! Junction-safe recursive cleanup for repository test fixtures.

use std::{path::Path, time::Duration};

const MAX_RETRIES: usize = 50;
const RETRY_DELAY: Duration = Duration::from_millis(200);

/// Recursively unlinks symbolic links without traversing their targets.
///
/// Missing paths are accepted. Regular files are left for the owning recursive
/// removal operation.
///
/// # Errors
///
/// Returns metadata, directory-read, or link-removal failures other than a
/// concurrent disappearance.
pub fn unlink_fixture_links(path: &Path) -> std::io::Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() {
        return unlink_symbolic_link(path).or_else(ignore_missing);
    }
    if !metadata.is_dir() {
        return Ok(());
    }
    for child in std::fs::read_dir(path)? {
        unlink_fixture_links(&child?.path())?;
    }
    Ok(())
}

/// Removes one fixture tree after unlinking every nested symbolic link.
///
/// Removal retries use the source's 50-attempt, 200 ms linear handle-release
/// window. Missing paths are accepted.
///
/// # Errors
///
/// Returns link discovery/removal failures or the final recursive-removal
/// failure after the retry window.
pub fn remove_fixture_safely(path: &Path) -> std::io::Result<()> {
    unlink_fixture_links(path)?;
    for attempt in 0..=MAX_RETRIES {
        match remove_path(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) if attempt < MAX_RETRIES && retryable_removal_error(&error) => {
                let multiplier = u32::try_from(attempt + 1).unwrap_or(u32::MAX);
                std::thread::sleep(RETRY_DELAY.saturating_mul(multiplier));
            }
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::other(
        "fixture removal exhausted its retry loop without an outcome",
    ))
}

fn remove_path(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(error) => Err(error),
    }
}

fn retryable_removal_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied
            | std::io::ErrorKind::DirectoryNotEmpty
            | std::io::ErrorKind::ResourceBusy
    ) || retryable_raw_error(error.raw_os_error())
}

#[cfg(unix)]
fn retryable_raw_error(error: Option<i32>) -> bool {
    matches!(error, Some(23 | 24))
}

#[cfg(windows)]
fn retryable_raw_error(error: Option<i32>) -> bool {
    matches!(error, Some(4 | 5 | 32 | 33 | 145))
}

#[cfg(not(any(unix, windows)))]
fn retryable_raw_error(_error: Option<i32>) -> bool {
    false
}

#[cfg(unix)]
fn unlink_symbolic_link(path: &Path) -> std::io::Result<()> {
    std::fs::remove_file(path)
}

#[cfg(windows)]
fn unlink_symbolic_link(path: &Path) -> std::io::Result<()> {
    std::fs::remove_file(path)
        .or_else(|file_error| std::fs::remove_dir(path).map_err(|_| file_error))
}

#[cfg(not(any(unix, windows)))]
fn unlink_symbolic_link(path: &Path) -> std::io::Result<()> {
    std::fs::remove_file(path)
}

fn ignore_missing(error: std::io::Error) -> std::io::Result<()> {
    if error.kind() == std::io::ErrorKind::NotFound {
        Ok(())
    } else {
        Err(error)
    }
}
