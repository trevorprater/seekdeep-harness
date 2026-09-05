//! Atomic file replacement and cross-process writer coordination.

use std::{
    error::Error,
    fmt,
    future::Future,
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use tokio::io::AsyncWriteExt;
use uuid::Uuid;

/// Filesystem permissions for an atomic replacement and newly created parents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteFileAtomicOptions {
    /// Permission bits for the fresh replacement inode.
    pub mode: u32,
    /// Optional permission bits for parent directories created by this call.
    pub dir_mode: Option<u32>,
}

/// Replaces a file with complete bytes in one same-directory rename.
///
/// Parent directories are created first. The temporary sibling uses exclusive
/// create, so it cannot follow a planted symlink, and the final rename replaces
/// the target entry itself. Crash durability through `fsync` is intentionally
/// outside this package's source contract.
///
/// # Errors
///
/// Returns a parent creation, temporary creation/write, rename, or cleanup
/// error. If cleanup itself fails, that cleanup error takes precedence just as
/// the source `catch` block does.
pub async fn write_file_atomic(
    filename: impl AsRef<Path>,
    content: &[u8],
    options: WriteFileAtomicOptions,
) -> io::Result<()> {
    let filename = filename.as_ref();
    let parent = nonempty_parent(filename);
    create_parent_directories(parent, options.dir_mode).await?;
    let temp = temporary_sibling(filename);

    let operation = async {
        let mut open = tokio::fs::OpenOptions::new();
        open.write(true).create_new(true);
        set_open_mode(&mut open, options.mode);
        let mut file = open.open(&temp).await?;
        file.write_all(content).await?;
        file.shutdown().await?;
        persist_temporary(file, &temp, filename).await
    }
    .await;

    if let Err(operation_error) = operation {
        match tokio::fs::remove_file(&temp).await {
            Ok(()) => return Err(operation_error),
            Err(cleanup) if cleanup.kind() == io::ErrorKind::NotFound => {
                return Err(operation_error);
            }
            Err(cleanup) => return Err(cleanup),
        }
    }
    Ok(())
}

fn nonempty_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

async fn create_parent_directories(path: &Path, mode: Option<u32>) -> io::Result<()> {
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true);
    set_directory_mode(&mut builder, mode);
    builder.create(path).await
}

fn temporary_sibling(filename: &Path) -> PathBuf {
    let random = Uuid::new_v4().simple().to_string();
    let suffix = &random[..12];
    let mut value = filename.as_os_str().to_os_string();
    value.push(format!(".{suffix}.tmp"));
    PathBuf::from(value)
}

#[cfg(unix)]
fn set_open_mode(options: &mut tokio::fs::OpenOptions, mode: u32) {
    options.mode(mode);
}

#[cfg(not(unix))]
fn set_open_mode(_options: &mut tokio::fs::OpenOptions, _mode: u32) {}

#[cfg(unix)]
fn set_directory_mode(builder: &mut tokio::fs::DirBuilder, mode: Option<u32>) {
    if let Some(mode) = mode {
        builder.mode(mode);
    }
}

#[cfg(not(unix))]
fn set_directory_mode(_builder: &mut tokio::fs::DirBuilder, _mode: Option<u32>) {}

async fn persist_temporary(file: tokio::fs::File, source: &Path, target: &Path) -> io::Result<()> {
    let file = file.into_std().await;
    let source = tempfile::TempPath::try_from_path(source.to_path_buf())?;
    let target = target.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let temporary = tempfile::NamedTempFile::from_parts(file, source);
        match temporary.persist(target) {
            Ok(_) => Ok(()),
            Err(error) => {
                let operation_error = error.error;
                match error.file.close() {
                    Ok(()) => Err(operation_error),
                    Err(cleanup_error) => Err(cleanup_error),
                }
            }
        }
    })
    .await
    .map_err(|error| io::Error::other(format!("atomic rename task failed: {error}")))?
}

const LOCK_RETRY_INITIAL: Duration = Duration::from_millis(20);
const LOCK_RETRY_MAX: Duration = Duration::from_millis(200);
const LOCK_TIMEOUT: Duration = Duration::from_millis(2_000);

/// Failure while acquiring, running, or releasing a writer-locked operation.
#[derive(Debug)]
pub enum FileLockError<E> {
    /// Exclusive lock creation failed for a reason other than contention.
    Acquire(io::Error),
    /// The existing lock remained present through the fixed deadline.
    Timeout {
        /// Lock sibling that remained occupied.
        path: PathBuf,
    },
    /// The protected operation failed.
    Operation(E),
    /// Removing this writer's lock failed; like JavaScript `finally`, this
    /// takes precedence over the operation's result.
    Release(io::Error),
}

impl<E: fmt::Display> fmt::Display for FileLockError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Acquire(error) | Self::Release(error) => error.fmt(formatter),
            Self::Timeout { path } => write!(
                formatter,
                "atomic-write: timed out waiting for the writer lock at {}",
                path.display()
            ),
            Self::Operation(error) => error.fmt(formatter),
        }
    }
}

impl<E: Error + 'static> Error for FileLockError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Acquire(error) | Self::Release(error) => Some(error),
            Self::Operation(error) => Some(error),
            Self::Timeout { .. } => None,
        }
    }
}

/// Runs one read-render-commit cycle under an exclusive `<filename>.lock`.
///
/// The contender never removes or steals an existing lock. Contention backs
/// off exponentially from 20 ms to 200 ms and fails after a 2 s deadline.
/// The parent directory must already exist.
///
/// # Errors
///
/// Returns acquisition, fixed-timeout, operation, or release failures. Release
/// failures take precedence over the protected result, matching `finally`.
pub async fn with_file_lock<T, E, F, Fut>(
    filename: impl AsRef<Path>,
    operation: F,
) -> Result<T, FileLockError<E>>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    with_file_lock_timing(
        filename.as_ref(),
        operation,
        LOCK_RETRY_INITIAL,
        LOCK_RETRY_MAX,
        LOCK_TIMEOUT,
    )
    .await
}

async fn with_file_lock_timing<T, E, F, Fut>(
    filename: &Path,
    operation: F,
    initial_delay: Duration,
    maximum_delay: Duration,
    timeout: Duration,
) -> Result<T, FileLockError<E>>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let lock_path = lock_path(filename);
    let deadline = Instant::now() + timeout;
    let mut delay = initial_delay;
    loop {
        match create_lock(&lock_path).await {
            Ok(()) => break,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(FileLockError::Acquire(error)),
        }
        if Instant::now() >= deadline {
            return Err(FileLockError::Timeout { path: lock_path });
        }
        tokio::time::sleep(delay).await;
        delay = delay.saturating_mul(2).min(maximum_delay);
    }

    let result = operation().await.map_err(FileLockError::Operation);
    match tokio::fs::remove_file(&lock_path).await {
        Ok(()) => result,
        Err(error) if error.kind() == io::ErrorKind::NotFound => result,
        Err(error) => Err(FileLockError::Release(error)),
    }
}

fn lock_path(filename: &Path) -> PathBuf {
    let mut path = filename.as_os_str().to_os_string();
    path.push(".lock");
    PathBuf::from(path)
}

async fn create_lock(path: &Path) -> io::Result<()> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    set_open_mode(&mut options, 0o600);
    let mut lock = options.open(path).await?;
    lock.write_all(format!("{}\n", std::process::id()).as_bytes())
        .await?;
    lock.shutdown().await
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use tempfile::tempdir;
    use thiserror::Error;

    use super::*;

    #[tokio::test]
    async fn creates_parents_content_and_exact_file_mode() {
        let temporary = tempdir().unwrap();
        let target = temporary.path().join("nested/deep/doc.yaml");
        write_file_atomic(
            &target,
            b"a: 1\n",
            WriteFileAtomicOptions {
                mode: 0o600,
                dir_mode: Some(0o700),
            },
        )
        .await
        .unwrap();
        assert_eq!(tokio::fs::read(&target).await.unwrap(), b"a: 1\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[tokio::test]
    async fn replacing_wider_file_narrows_mode_and_replaces_symlink_itself() {
        let temporary = tempdir().unwrap();
        let regular = temporary.path().join("regular.yaml");
        let mut wide = tokio::fs::OpenOptions::new();
        wide.write(true).create_new(true);
        set_open_mode(&mut wide, 0o644);
        let mut wide = wide.open(&regular).await.unwrap();
        wide.write_all(b"old").await.unwrap();
        wide.shutdown().await.unwrap();
        write_file_atomic(
            &regular,
            b"new",
            WriteFileAtomicOptions {
                mode: 0o600,
                dir_mode: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(tokio::fs::read(&regular).await.unwrap(), b"new");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&regular).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        let victim = temporary.path().join("victim");
        tokio::fs::write(&victim, b"victim-content").await.unwrap();
        let target = temporary.path().join("doc.yaml");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&victim, &target).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&victim, &target).unwrap();
        write_file_atomic(
            &target,
            b"replaced",
            WriteFileAtomicOptions {
                mode: 0o600,
                dir_mode: None,
            },
        )
        .await
        .unwrap();
        assert!(
            !std::fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(tokio::fs::read(&target).await.unwrap(), b"replaced");
        assert_eq!(tokio::fs::read(&victim).await.unwrap(), b"victim-content");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[tokio::test]
    async fn rename_failure_cleans_temporary_sibling() {
        let temporary = tempdir().unwrap();
        let target = temporary.path().join("occupied");
        tokio::fs::create_dir(&target).await.unwrap();
        assert!(
            write_file_atomic(
                &target,
                b"content",
                WriteFileAtomicOptions {
                    mode: 0o600,
                    dir_mode: None
                }
            )
            .await
            .is_err()
        );
        let entries = std::fs::read_dir(temporary.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert!(
            !entries
                .iter()
                .any(|entry| entry.to_string_lossy().contains(".tmp"))
        );
    }

    #[derive(Debug, Error)]
    #[error("operation failed")]
    struct OperationFailure;

    #[tokio::test]
    async fn lock_serializes_writers_and_releases_after_operation_error() {
        let temporary = tempdir().unwrap();
        let target = temporary.path().join("document");
        let first_entered = Arc::new(tokio::sync::Notify::new());
        let release_first = Arc::new(tokio::sync::Notify::new());
        let first = {
            let entered = first_entered.clone();
            let release = release_first.clone();
            let target = target.clone();
            tokio::spawn(async move {
                with_file_lock(&target, || async move {
                    entered.notify_one();
                    release.notified().await;
                    Ok::<_, OperationFailure>(1)
                })
                .await
            })
        };
        first_entered.notified().await;
        let second_entered = Arc::new(AtomicBool::new(false));
        let second = {
            let entered = second_entered.clone();
            let target = target.clone();
            tokio::spawn(async move {
                with_file_lock(&target, || async move {
                    entered.store(true, Ordering::Release);
                    Err::<(), _>(OperationFailure)
                })
                .await
            })
        };
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(!second_entered.load(Ordering::Acquire));
        release_first.notify_one();
        assert_eq!(first.await.unwrap().unwrap(), 1);
        assert!(matches!(
            second.await.unwrap(),
            Err(FileLockError::Operation(OperationFailure))
        ));
        assert!(!lock_path(&target).exists());
    }

    #[tokio::test]
    async fn lock_never_steals_existing_file_and_times_out() {
        let temporary = tempdir().unwrap();
        let target = temporary.path().join("document");
        let lock = lock_path(&target);
        tokio::fs::write(&lock, b"someone-else\n").await.unwrap();
        let called = AtomicBool::new(false);
        let result = with_file_lock_timing(
            &target,
            || async {
                called.store(true, Ordering::Release);
                Ok::<_, OperationFailure>(())
            },
            Duration::from_millis(1),
            Duration::from_millis(2),
            Duration::from_millis(5),
        )
        .await;
        assert!(matches!(result, Err(FileLockError::Timeout { .. })));
        assert!(!called.load(Ordering::Acquire));
        assert_eq!(tokio::fs::read(&lock).await.unwrap(), b"someone-else\n");
    }

    #[tokio::test]
    async fn invalid_parent_fails_before_operation() {
        let temporary = tempdir().unwrap();
        let parent = temporary.path().join("not-a-directory");
        tokio::fs::write(&parent, b"occupied").await.unwrap();
        let called = AtomicBool::new(false);
        let result = with_file_lock(parent.join("document"), || async {
            called.store(true, Ordering::Release);
            Ok::<_, OperationFailure>(())
        })
        .await;
        assert!(matches!(result, Err(FileLockError::Acquire(_))));
        assert!(!called.load(Ordering::Acquire));
    }
}
