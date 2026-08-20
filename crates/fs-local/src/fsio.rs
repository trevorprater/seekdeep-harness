//! Cordis-free local filesystem mechanics.

use std::path::{Path, PathBuf};

use seekdeep_fs::types::{FsError, FsErrorCode, FsKind, FsTargetKey, FsVersion};
use seekdeep_llm::AbortSignal;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

const BINARY_SAMPLE_BYTES: usize = 8192;

/// A resolved local path: the absolute path shown to callers and its realpath identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalTarget {
    /// Absolute path (symlinks not resolved) used for display.
    pub display_path: String,
    /// Realpath identity used as the stable target key and the I/O path.
    pub target_key: FsTargetKey,
}

/// Result of probing a path: null when it does not exist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathInfo {
    /// Opaque freshness token.
    pub version: FsVersion,
    /// POSIX permission bits (0o777 mask).
    pub mode: u32,
    /// Regular file, directory, or other.
    pub kind: FsKind,
    /// Byte size.
    pub size: u64,
}

/// One local directory child with a resolved target and cheap metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalDirEntry {
    /// Basename.
    pub name: String,
    /// Regular file, directory, or other.
    pub kind: FsKind,
    /// Resolved child target.
    pub target: LocalTarget,
    /// Optional freshness token.
    pub version: Option<FsVersion>,
    /// Optional byte size.
    pub size: Option<u64>,
}

/// Line-ending style detected before LF normalization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEndings {
    /// Unix LF.
    Lf,
    /// Windows CRLF.
    Crlf,
}

fn ensure_not_aborted(signal: Option<&AbortSignal>, verb: &str) -> Result<(), FsError> {
    if signal.is_some_and(AbortSignal::is_aborted) {
        Err(FsError::new(
            format!("{verb} aborted"),
            FsErrorCode::FsAborted,
        ))
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn version_of(meta: &std::fs::Metadata) -> FsVersion {
    use std::os::unix::fs::MetadataExt;
    FsVersion::new(format!(
        "{}:{}:{}:{}:{}",
        meta.dev(),
        meta.ino(),
        meta.size(),
        meta.mtime_nsec(),
        meta.ctime_nsec()
    ))
}

#[cfg(not(unix))]
fn version_of(meta: &std::fs::Metadata) -> FsVersion {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos());
    FsVersion::new(format!("{}:{mtime}", meta.len()))
}

#[cfg(unix)]
fn mode_of(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn mode_of(_meta: &std::fs::Metadata) -> u32 {
    0o644
}

/// Opaque version token from high-resolution identity and freshness metadata.
#[must_use]
pub fn version_of_meta(meta: &std::fs::Metadata) -> FsVersion {
    version_of(meta)
}

fn kind_of(meta: &std::fs::Metadata) -> FsKind {
    if meta.is_file() {
        FsKind::File
    } else if meta.is_dir() {
        FsKind::Directory
    } else {
        FsKind::Other
    }
}

fn dirname(path: &str) -> String {
    match Path::new(path).parent() {
        Some(parent) => {
            let parent = parent.to_string_lossy();
            if parent.is_empty() {
                ".".to_owned()
            } else {
                parent.into_owned()
            }
        }
        None => path.to_owned(),
    }
}

fn basename(path: &str) -> String {
    Path::new(path).file_name().map_or_else(
        || path.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn resolve_path(cwd: &str, path: &str) -> String {
    let path = Path::new(path);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        Path::new(cwd).join(path)
    };
    absolute
        .components()
        .collect::<PathBuf>()
        .to_string_lossy()
        .into_owned()
}

/// Resolves a path to its absolute display path and realpath identity.
///
/// # Errors
///
/// Returns a structured not-found or I/O failure.
pub async fn resolve_local_target(cwd: &str, path: &str) -> Result<LocalTarget, FsError> {
    if path.trim().is_empty() {
        return Err(FsError::new(
            "file_path must be a non-empty string",
            FsErrorCode::FsNotFound,
        ));
    }
    let display_path = resolve_path(cwd, path);
    match tokio::fs::canonicalize(&display_path).await {
        Ok(real) => Ok(LocalTarget {
            display_path: display_path.clone(),
            target_key: FsTargetKey::new(real.to_string_lossy().into_owned()),
        }),
        Err(error) => {
            if error.kind() == std::io::ErrorKind::NotADirectory {
                return Err(FsError::new(
                    format!(
                        "cannot resolve \"{display_path}\": a parent path segment is not a directory"
                    ),
                    FsErrorCode::FsNotFound,
                ));
            }
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(FsError::new(error.to_string(), FsErrorCode::FsIoError));
            }
            let mut missing = vec![basename(&display_path)];
            let mut ancestor = dirname(&display_path);
            loop {
                match tokio::fs::canonicalize(&ancestor).await {
                    Ok(real_ancestor) => {
                        let mut joined = real_ancestor;
                        for part in missing.iter().rev() {
                            joined.push(part);
                        }
                        return Ok(LocalTarget {
                            display_path: display_path.clone(),
                            target_key: FsTargetKey::new(joined.to_string_lossy().into_owned()),
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        let parent = dirname(&ancestor);
                        if parent == ancestor {
                            return Ok(LocalTarget {
                                display_path: display_path.clone(),
                                target_key: FsTargetKey::new(display_path),
                            });
                        }
                        missing.push(basename(&ancestor));
                        ancestor = parent;
                    }
                    Err(error) => {
                        return Err(FsError::new(error.to_string(), FsErrorCode::FsIoError));
                    }
                }
            }
        }
    }
}

/// Probes a path for its version, mode, kind, and size. None if absent.
///
/// # Errors
///
/// Returns a structured I/O failure for non-absence metadata errors.
pub async fn probe(path: &str) -> Result<Option<PathInfo>, FsError> {
    match tokio::fs::metadata(path).await {
        Ok(meta) => Ok(Some(PathInfo {
            version: version_of(&meta),
            mode: mode_of(&meta),
            kind: kind_of(&meta),
            size: meta.len(),
        })),
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || error.kind() == std::io::ErrorKind::NotADirectory =>
        {
            Ok(None)
        }
        Err(error) => Err(FsError::new(error.to_string(), FsErrorCode::FsIoError)),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn listing_io_error(display_path: &str, error: std::io::Error) -> FsError {
    match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory => FsError::new(
            format!("cannot list \"{display_path}\": not found"),
            FsErrorCode::FsNotFound,
        ),
        std::io::ErrorKind::PermissionDenied => FsError::new(
            format!("cannot list \"{display_path}\": permission denied"),
            FsErrorCode::FsPermissionDenied,
        ),
        _ => FsError::new(
            format!("cannot list \"{display_path}\": {error}"),
            FsErrorCode::FsIoError,
        ),
    }
}

/// Lists direct children of a directory in stable name order.
///
/// # Errors
///
/// Returns not-found, not-directory, permission, or I/O failures.
pub async fn list_directory(
    target: &LocalTarget,
    signal: Option<&AbortSignal>,
) -> Result<Vec<LocalDirEntry>, FsError> {
    ensure_not_aborted(signal, "list")?;
    let info = probe(target.target_key.as_str()).await.map_err(|error| {
        if error.code == FsErrorCode::FsIoError {
            listing_io_error(
                &target.display_path,
                std::io::Error::other(error.message.clone()),
            )
        } else {
            error
        }
    })?;
    let Some(info) = info else {
        return Err(FsError::new(
            format!("cannot list \"{}\": not found", target.display_path),
            FsErrorCode::FsNotFound,
        ));
    };
    if info.kind != FsKind::Directory {
        return Err(FsError::new(
            format!("cannot list \"{}\": not a directory", target.display_path),
            FsErrorCode::FsNotDirectory,
        ));
    }
    let mut entries = match tokio::fs::read_dir(target.target_key.as_str()).await {
        Ok(entries) => entries,
        Err(error) => return Err(listing_io_error(&target.display_path, error)),
    };
    ensure_not_aborted(signal, "list")?;
    let mut result = Vec::new();
    let mut names = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| listing_io_error(&target.display_path, error))?
    {
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    for name in names {
        ensure_not_aborted(signal, "list")?;
        let child_target = resolve_local_target(target.target_key.as_str(), &name).await?;
        let child_info = probe(child_target.target_key.as_str())
            .await
            .map_err(|error| {
                if error.code == FsErrorCode::FsIoError {
                    listing_io_error(
                        &join(&target.display_path, &name),
                        std::io::Error::other(error.message.clone()),
                    )
                } else {
                    error
                }
            })?;
        let child_display = join(&target.display_path, &name);
        result.push(LocalDirEntry {
            name,
            kind: child_info.as_ref().map_or(FsKind::Other, |info| info.kind),
            target: LocalTarget {
                display_path: child_display,
                target_key: child_target.target_key,
            },
            version: child_info.as_ref().map(|info| info.version.clone()),
            size: child_info
                .as_ref()
                .filter(|info| info.kind == FsKind::File)
                .map(|info| info.size),
        });
    }
    Ok(result)
}

fn join(parent: &str, name: &str) -> String {
    Path::new(parent).join(name).to_string_lossy().into_owned()
}

fn not_text_error(verb: &str, display_path: &str) -> FsError {
    FsError::new(
        format!("cannot {verb} \"{display_path}\": invalid UTF-8 text"),
        FsErrorCode::FsNotText,
    )
}

async fn stat_regular_file(
    target: &LocalTarget,
    verb: &str,
    signal: Option<&AbortSignal>,
) -> Result<std::fs::Metadata, FsError> {
    ensure_not_aborted(signal, verb)?;
    match tokio::fs::metadata(target.target_key.as_str()).await {
        Ok(info) => {
            if !info.is_file() {
                return Err(FsError::new(
                    format!(
                        "cannot {verb} \"{}\": not a regular file",
                        target.display_path
                    ),
                    FsErrorCode::FsNotRegularFile,
                ));
            }
            Ok(info)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(FsError::new(
            format!("cannot {verb} \"{}\": not found", target.display_path),
            FsErrorCode::FsNotFound,
        )),
        Err(error) => Err(FsError::new(error.to_string(), FsErrorCode::FsIoError)),
    }
}

/// Reads a whole regular UTF-8 text file into a single decoded string.
///
/// # Errors
///
/// Returns not-found, not-regular-file, binary, or invalid-UTF-8 failures.
pub async fn read_whole_text(
    target: &LocalTarget,
    signal: Option<&AbortSignal>,
) -> Result<String, FsError> {
    stat_regular_file(target, "read", signal).await?;
    let raw = match tokio::fs::read(target.target_key.as_str()).await {
        Ok(raw) => raw,
        Err(error) => {
            if signal.is_some_and(AbortSignal::is_aborted) {
                return Err(FsError::new("read aborted", FsErrorCode::FsAborted));
            }
            return Err(FsError::new(error.to_string(), FsErrorCode::FsIoError));
        }
    };
    ensure_not_aborted(signal, "read")?;
    if raw.iter().take(BINARY_SAMPLE_BYTES).any(|byte| *byte == 0) {
        return Err(FsError::new(
            format!("cannot read \"{}\": binary file", target.display_path),
            FsErrorCode::FsNotText,
        ));
    }
    String::from_utf8(raw).map_err(|_| not_text_error("read", &target.display_path))
}

/// Reads a whole regular file as raw bytes bounded by `max_bytes`.
///
/// # Errors
///
/// Returns not-found, not-regular-file, or too-large failures.
pub async fn read_whole_bytes(
    target: &LocalTarget,
    signal: Option<&AbortSignal>,
    max_bytes: usize,
) -> Result<Vec<u8>, FsError> {
    let info = stat_regular_file(target, "read", signal).await?;
    if info.len() > max_bytes as u64 {
        return Err(FsError::new(
            format!(
                "cannot read \"{}\": {} bytes exceeds the {max_bytes}-byte limit",
                target.display_path,
                info.len()
            ),
            FsErrorCode::FsTooLarge,
        ));
    }
    let raw = tokio::fs::read(target.target_key.as_str())
        .await
        .map_err(|error| FsError::new(error.to_string(), FsErrorCode::FsIoError))?;
    ensure_not_aborted(signal, "read")?;
    if raw.len() > max_bytes {
        return Err(FsError::new(
            format!(
                "cannot read \"{}\": content exceeds the {max_bytes}-byte limit",
                target.display_path
            ),
            FsErrorCode::FsTooLarge,
        ));
    }
    Ok(raw)
}

/// Collapses CRLF to LF, the canonical in-memory edit/diff form.
#[must_use]
pub fn normalize_line_endings(content: &str) -> String {
    content.replace("\r\n", "\n")
}

/// Converts LF-normalized content back to the detected line-ending style.
#[must_use]
pub fn restore_line_endings(content: &str, line_endings: LineEndings) -> String {
    match line_endings {
        LineEndings::Lf => content.to_owned(),
        LineEndings::Crlf => normalize_line_endings(content).replace('\n', "\r\n"),
    }
}

fn detect_line_endings(raw: &str) -> LineEndings {
    let sample: String = raw.chars().take(4096).collect();
    let crlf_count = sample.matches("\r\n").count();
    let lf_count = sample.matches('\n').count() - crlf_count;
    if crlf_count > lf_count {
        LineEndings::Crlf
    } else {
        LineEndings::Lf
    }
}

fn count_occurrences(content: &str, needle: &str) -> usize {
    content.matches(needle).count()
}

/// Applies a literal replacement to LF-normalized content.
///
/// # Errors
///
/// Returns edit-not-found or ambiguous-edit failures.
pub fn apply_literal_edit(
    content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
    display_path: &str,
) -> Result<(String, usize), FsError> {
    let old_norm = normalize_line_endings(old_string);
    if old_norm.is_empty() {
        return Err(FsError::new(
            "old_string must be a non-empty string",
            FsErrorCode::FsEditNotFound,
        ));
    }
    let new_norm = normalize_line_endings(new_string);
    let replacements = count_occurrences(content, &old_norm);
    if replacements == 0 {
        return Err(FsError::new(
            format!("old_string was not found in \"{display_path}\""),
            FsErrorCode::FsEditNotFound,
        ));
    }
    if !replace_all && replacements > 1 {
        return Err(FsError::new(
            format!(
                "old_string matched {replacements} times in \"{display_path}\"; provide a more specific old_string or set replace_all to true"
            ),
            FsErrorCode::FsAmbiguousEdit,
        ));
    }
    Ok((content.replace(&old_norm, &new_norm), replacements))
}

/// Reads and decodes a file for editing, returning LF-normalized content plus style.
///
/// # Errors
///
/// Returns not-found, binary, or invalid-UTF-8 failures.
pub async fn read_for_edit(
    absolute_path: &str,
    display_path: &str,
    signal: Option<&AbortSignal>,
) -> Result<(String, LineEndings), FsError> {
    ensure_not_aborted(signal, "edit")?;
    let raw = tokio::fs::read(absolute_path)
        .await
        .map_err(|error| FsError::new(error.to_string(), FsErrorCode::FsIoError))?;
    ensure_not_aborted(signal, "edit")?;
    if raw.contains(&0) {
        return Err(FsError::new(
            format!("cannot edit \"{display_path}\": binary file"),
            FsErrorCode::FsNotText,
        ));
    }
    let decoded = String::from_utf8(raw).map_err(|_| not_text_error("edit", display_path))?;
    let endings = detect_line_endings(&decoded);
    Ok((normalize_line_endings(&decoded), endings))
}

/// Atomically replaces a file through a private, synced staging file.
///
/// # Errors
///
/// Returns structured I/O, permission, or abort failures.
pub async fn write_file_atomic(
    absolute_path: &str,
    content: &str,
    mode: Option<u32>,
    signal: Option<&AbortSignal>,
) -> Result<(), FsError> {
    ensure_not_aborted(signal, "write")?;
    let directory = dirname(absolute_path);
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|error| FsError::new(error.to_string(), FsErrorCode::FsIoError))?;
    ensure_not_aborted(signal, "write")?;
    let staging_dir_name = format!(
        ".{}.{}.{}.tmpdir",
        basename(absolute_path),
        std::process::id(),
        Uuid::new_v4()
    );
    let staging_dir = Path::new(&directory).join(&staging_dir_name);
    let temp_path = staging_dir.join(format!("{}.tmp", basename(absolute_path)));
    tokio::fs::create_dir(&staging_dir)
        .await
        .map_err(|error| FsError::new(error.to_string(), FsErrorCode::FsIoError))?;
    let result = async {
        let mut handle = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .await
            .map_err(|error| FsError::new(error.to_string(), FsErrorCode::FsIoError))?;
        handle
            .write_all(content.as_bytes())
            .await
            .map_err(|error| FsError::new(error.to_string(), FsErrorCode::FsIoError))?;
        handle
            .sync_all()
            .await
            .map_err(|error| FsError::new(error.to_string(), FsErrorCode::FsIoError))?;
        if let Some(mode) = mode {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                handle
                    .set_permissions(std::fs::Permissions::from_mode(mode))
                    .await
                    .map_err(|error| FsError::new(error.to_string(), FsErrorCode::FsIoError))?;
            }
        }
        drop(handle);
        ensure_not_aborted(signal, "write")?;
        tokio::fs::rename(&temp_path, absolute_path)
            .await
            .map_err(|error| FsError::new(error.to_string(), FsErrorCode::FsIoError))?;
        Ok::<(), FsError>(())
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&staging_dir).await;
    result
}
