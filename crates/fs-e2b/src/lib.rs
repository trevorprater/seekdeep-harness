//! Remote E2B filesystem provider over an object-safe sandbox client.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use base64::Engine as _;
use futures::{StreamExt as _, stream::BoxStream};
use parking_lot::Mutex;
use seekdeep_cordis::{Context, Plugin};
use seekdeep_e2b::E2B;
use seekdeep_fs::{
    FileSystem, FileSystemService, FsDirEntry, FsEditOutcome, FsEditRequest, FsError, FsErrorCode,
    FsInfo, FsKind, FsPathInfo, FsPathKind, FsTarget, FsTargetKey, FsVersion, FsWriteIntent,
    FsWriteOperation, FsWriteOutcome,
};
use seekdeep_llm::AbortSignal;
use seekdeep_sandbox::SandboxExecutionPolicy;
use sha2::{Digest as _, Sha256};

pub use seekdeep_e2b::{
    E2bByteStream, E2bCommandExit, E2bCommandResult, E2bCommands, E2bCreateOptions, E2bEntryInfo,
    E2bFileNotFound, E2bFileType, E2bFiles, E2bSandbox, E2bSandboxFactory, E2bSandboxFuture,
    E2bSandboxNotFound, E2bService,
};

const VERSION_METADATA_KEY: &str = "seekdeep-version";
const BINARY_SAMPLE_BYTES: usize = 8_192;

/// Cordis plugin name.
pub const NAME: &str = "fs-e2b";
/// Required services.
pub const INJECT: &[&str] = &["e2b"];

/// Remote filesystem backend sharing the sandbox owned by `ctx.e2b`.
pub struct E2bFileSystem {
    e2b: Arc<E2bService>,
    locks: Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>,
    next_staging: AtomicU64,
    next_version: AtomicU64,
}

impl std::fmt::Debug for E2bFileSystem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("E2bFileSystem")
            .field("cwd", &self.e2b.cwd())
            .finish_non_exhaustive()
    }
}

impl E2bFileSystem {
    /// Creates an unprovided backend.
    #[must_use]
    pub fn new(e2b: Arc<E2bService>) -> Arc<Self> {
        Arc::new(Self {
            e2b,
            locks: Mutex::new(HashMap::new()),
            next_staging: AtomicU64::new(1),
            next_version: AtomicU64::new(1),
        })
    }

    /// Installs the E2B backend on `ctx.fs`.
    ///
    /// # Errors
    ///
    /// Returns missing E2B, duplicate filesystem, or inactive-owner failures.
    pub fn install(context: &Context) -> anyhow::Result<Arc<Self>> {
        let e2b = context
            .get(E2B)
            .ok_or_else(|| anyhow::anyhow!("fs-e2b requires e2b"))?;
        let backend = Self::new(e2b);
        FileSystemService::new(backend.clone()).provide(context)?;
        Ok(backend)
    }

    async fn sandbox(&self) -> anyhow::Result<Arc<dyn E2bSandbox>> {
        self.e2b.get_sandbox().await
    }

    fn lock_for(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.locks.lock();
        if let Some(lock) = locks.get(key).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(key.to_owned(), Arc::downgrade(&lock));
        lock
    }

    async fn canonical_path(
        &self,
        sandbox: &dyn E2bSandbox,
        path: &str,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<String> {
        let command = format!(
            "set -o pipefail; realpath -mz -- {} | base64 -w0",
            quote_shell_arg(path)
        );
        let result = sandbox
            .commands()
            .run(&command, control_envs(), signal)
            .await
            .map_err(|error| {
                let message = error
                    .downcast_ref::<E2bCommandExit>()
                    .filter(|exit| !exit.stderr.is_empty())
                    .map_or_else(|| error.to_string(), |exit| exit.stderr.clone());
                anyhow::anyhow!(message)
            })?;
        decode_canonical_path(&result.stdout)
    }

    async fn probe(
        &self,
        path: &str,
        display_path: &str,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Option<E2bEntryInfo>> {
        assert_not_aborted(signal, "stat")?;
        match self.sandbox().await?.files().get_info(path, signal).await {
            Ok(entry) => {
                assert_not_aborted(signal, "stat")?;
                Ok(Some(entry))
            }
            Err(error) if error.downcast_ref::<E2bFileNotFound>().is_some() => Ok(None),
            Err(error) => Err(map_error(error, "stat", display_path, signal)),
        }
    }

    async fn require_regular(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<FsInfo> {
        let Some(info) = self.stat(target, signal).await? else {
            return Err(fs_error(
                format!("cannot read {:?}: not found", target.display_path),
                FsErrorCode::FsNotFound,
            ));
        };
        if info.kind != FsKind::File {
            return Err(fs_error(
                format!("cannot read {:?}: not a regular file", target.display_path),
                FsErrorCode::FsNotRegularFile,
            ));
        }
        Ok(info)
    }

    async fn read_for_diff(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Option<String>> {
        let result = async {
            let bytes = self
                .sandbox()
                .await?
                .files()
                .read_bytes(target.target_key.as_str(), signal)
                .await?;
            assert_not_aborted(signal, "read")?;
            decode_text(&bytes, &target.display_path, bytes.len())
        }
        .await;
        match result {
            Err(error)
                if error
                    .downcast_ref::<FsError>()
                    .is_some_and(|error| error.code == FsErrorCode::FsNotText) =>
            {
                Ok(None)
            }
            Ok(text) => Ok(Some(normalize_line_endings(&text))),
            Err(error) => Err(map_error(error, "read", &target.display_path, signal)),
        }
    }

    async fn read_for_edit(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<String> {
        let result = async {
            let bytes = self
                .sandbox()
                .await?
                .files()
                .read_bytes(target.target_key.as_str(), signal)
                .await?;
            assert_not_aborted(signal, "edit")?;
            decode_text(&bytes, &target.display_path, bytes.len())
        }
        .await;
        result.map_err(|error| map_error(error, "edit", &target.display_path, signal))
    }

    fn check_write_intent(
        existing: Option<&E2bEntryInfo>,
        expected: Option<&FsWriteIntent>,
        target: &FsTarget,
    ) -> anyhow::Result<()> {
        match expected {
            Some(FsWriteIntent::CreateIfAbsent) if existing.is_some() => Err(fs_error(
                format!(
                    "cannot overwrite existing {:?} without reading it first",
                    target.display_path
                ),
                FsErrorCode::FsNotObserved,
            )),
            Some(FsWriteIntent::ReplaceIfVersion { version })
                if existing.is_none_or(|entry| entry_version(entry) != *version) =>
            {
                Err(fs_error(
                    format!(
                        "cannot write {:?}: file changed since it was read",
                        target.display_path
                    ),
                    FsErrorCode::FsStaleVersion,
                ))
            }
            _ => Ok(()),
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the staging ledger keeps pre-commit cleanup and post-commit success in one owner"
    )]
    async fn write_atomic(
        &self,
        target: &FsTarget,
        content: &str,
        existing: Option<&E2bEntryInfo>,
        create_if_absent: bool,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<FsVersion> {
        assert_not_aborted(signal, "write")?;
        let sandbox = self.sandbox().await?;
        let target_path = target.target_key.as_str();
        let version_id = format!(
            "seekdeep-v{}",
            self.next_version.fetch_add(1, Ordering::Relaxed)
        );
        let staging_directory = posix_join(
            posix_dirname(target_path),
            &format!(
                ".seekdeep-e2b-{}.tmp",
                self.next_staging.fetch_add(1, Ordering::Relaxed)
            ),
        );
        let temporary = posix_join(&staging_directory, "content");
        let mut staging_created = false;
        let operation = async {
            anyhow::ensure!(
                sandbox.files().make_dir(&staging_directory, signal).await?,
                "private staging directory already exists"
            );
            staging_created = true;
            sandbox
                .commands()
                .run(
                    &format!("chmod 700 -- {}", quote_shell_arg(&staging_directory)),
                    control_envs(),
                    signal,
                )
                .await?;
            assert_not_aborted(signal, "write")?;
            sandbox
                .files()
                .write(
                    &temporary,
                    content,
                    BTreeMap::from([(VERSION_METADATA_KEY.to_owned(), version_id)]),
                    signal,
                )
                .await?;
            assert_not_aborted(signal, "write")?;
            let mode = existing.map_or(0o600, |entry| entry.mode & 0o777);
            sandbox
                .commands()
                .run(
                    &format!("chmod {mode:o} -- {}", quote_shell_arg(&temporary)),
                    control_envs(),
                    signal,
                )
                .await?;
            assert_not_aborted(signal, "write")?;
            let committed = if create_if_absent {
                let staged = sandbox.files().get_info(&temporary, signal).await?;
                assert_not_aborted(signal, "write")?;
                let target_arg = quote_shell_arg(target_path);
                let publication = sandbox
                    .commands()
                    .run(
                        &format!(
                            "if ln -T -- {} {target_arg}; then printf created; elif test -e {target_arg} || test -L {target_arg}; then printf exists; else exit 1; fi",
                            quote_shell_arg(&temporary)
                        ),
                        control_envs(),
                        None,
                    )
                    .await?;
                if publication.stdout == "exists" {
                    return Err(fs_error(
                        format!(
                            "cannot overwrite existing {:?} without reading it first",
                            target.display_path
                        ),
                        FsErrorCode::FsNotObserved,
                    ));
                }
                anyhow::ensure!(
                    publication.stdout == "created",
                    "guarded create returned an invalid publication result"
                );
                E2bEntryInfo {
                    name: posix_basename(target_path).to_owned(),
                    path: target_path.to_owned(),
                    ..staged
                }
            } else {
                sandbox
                    .files()
                    .rename(&temporary, target_path, signal)
                    .await?
            };
            let _ = sandbox.files().remove(&staging_directory).await;
            Ok(entry_version(&committed))
        }
        .await;
        match operation {
            Ok(version) => Ok(version),
            Err(error) => {
                if staging_created {
                    let _ = sandbox.files().remove(&staging_directory).await;
                }
                Err(map_error(error, "write", &target.display_path, signal))
            }
        }
    }
}

#[async_trait::async_trait]
impl FileSystem for E2bFileSystem {
    async fn resolve(
        &self,
        path: &str,
        cwd: Option<&str>,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<FsTarget> {
        assert_not_aborted(signal, "resolve")?;
        if path.trim().is_empty() {
            return Err(fs_error(
                "file_path must be a non-empty string",
                FsErrorCode::FsNotFound,
            ));
        }
        let display_path = posix_resolve(cwd.unwrap_or(self.e2b.cwd()), path);
        let result = async {
            let sandbox = self.sandbox().await?;
            let target_key = self
                .canonical_path(sandbox.as_ref(), &display_path, signal)
                .await?;
            assert_not_aborted(signal, "resolve")?;
            Ok(FsTarget {
                target_key: FsTargetKey::new(target_key),
                display_path: display_path.clone(),
            })
        }
        .await;
        result.map_err(|error| map_error(error, "resolve", &display_path, signal))
    }

    fn process_path(&self, target: &FsTarget) -> String {
        target.target_key.as_str().to_owned()
    }

    fn file_url(&self, target: &FsTarget) -> String {
        let path = self.process_path(target);
        assert!(
            path.starts_with('/'),
            "fs-e2b expected an absolute process path: {path:?}"
        );
        url::Url::from_file_path(&path)
            .expect("absolute POSIX paths have file URLs")
            .to_string()
    }

    fn contains(&self, parent: &FsTarget, child: &FsTarget) -> bool {
        let parent = parent.target_key.as_str().trim_end_matches('/');
        let child = child.target_key.as_str();
        child == parent
            || child
                .strip_prefix(parent)
                .is_some_and(|suffix| suffix.starts_with('/'))
    }

    async fn stat(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Option<FsInfo>> {
        assert_not_aborted(signal, "stat")?;
        Ok(self
            .probe(target.target_key.as_str(), &target.display_path, signal)
            .await?
            .map(|entry| FsInfo {
                version: entry_version(&entry),
                kind: entry_kind(&entry),
                size: (entry.kind == E2bFileType::File).then_some(entry.size),
            }))
    }

    async fn lstat(
        &self,
        path: &str,
        cwd: Option<&str>,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Option<FsPathInfo>> {
        assert_not_aborted(signal, "lstat")?;
        if path.trim().is_empty() {
            return Err(fs_error(
                "file_path must be a non-empty string",
                FsErrorCode::FsNotFound,
            ));
        }
        let display_path = posix_resolve(cwd.unwrap_or(self.e2b.cwd()), path);
        Ok(self
            .probe(&display_path, &display_path, signal)
            .await?
            .map(|entry| FsPathInfo {
                version: entry_version(&entry),
                kind: if entry.symlink_target.is_some() {
                    FsPathKind::Symlink
                } else {
                    match entry.kind {
                        E2bFileType::File => FsPathKind::File,
                        E2bFileType::Directory => FsPathKind::Directory,
                        E2bFileType::Other => FsPathKind::Other,
                    }
                },
                size: (entry.kind == E2bFileType::File).then_some(entry.size),
            }))
    }

    async fn read_text(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<String> {
        let sandbox = self.sandbox().await?;
        self.require_regular(target, signal).await?;
        let result = async {
            let bytes = sandbox
                .files()
                .read_bytes(target.target_key.as_str(), signal)
                .await?;
            assert_not_aborted(signal, "read")?;
            decode_text(&bytes, &target.display_path, BINARY_SAMPLE_BYTES)
        }
        .await;
        result.map_err(|error| map_error(error, "read", &target.display_path, signal))
    }

    #[allow(
        clippy::collapsible_if,
        reason = "async-stream expands 2024 let chains under its own older macro edition"
    )]
    async fn stream_text(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<BoxStream<'static, anyhow::Result<String>>> {
        let sandbox = self.sandbox().await?;
        self.require_regular(target, signal).await?;
        let remote = sandbox
            .files()
            .read_stream(target.target_key.as_str(), signal)
            .await
            .map_err(|error| map_error(error, "read", &target.display_path, signal))?;
        let display_path = target.display_path.clone();
        let signal = signal.cloned();
        let output = async_stream::try_stream! {
            let mut remote = remote;
            let mut cancellation = CancelOnDrop(Some(remote.cancel.clone()));
            let mut pending = Vec::new();
            let mut sampled = 0usize;
            while let Some(chunk) = remote.stream.next().await {
                assert_not_aborted(signal.as_ref(), "read")?;
                let chunk = chunk.map_err(|error| map_error(error, "read", &display_path, signal.as_ref()))?;
                if sampled < BINARY_SAMPLE_BYTES {
                    let count = (BINARY_SAMPLE_BYTES - sampled).min(chunk.len());
                    if chunk[..count].contains(&0) {
                        Err(fs_error(format!("cannot read {display_path:?}: binary file"), FsErrorCode::FsNotText))?;
                    }
                    sampled += count;
                }
                pending.extend_from_slice(&chunk);
                if let Some(text) = decode_utf8_prefix(&mut pending, false, &display_path)? {
                    if !text.is_empty() {
                        yield text;
                    }
                }
            }
            if let Some(text) = decode_utf8_prefix(&mut pending, true, &display_path)? {
                if !text.is_empty() {
                    yield text;
                }
            }
            cancellation.0 = None;
        };
        Ok(Box::pin(output))
    }

    async fn read_bytes(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
        max_bytes: usize,
    ) -> anyhow::Result<Vec<u8>> {
        let sandbox = self.sandbox().await?;
        let info = self.require_regular(target, signal).await?;
        if info.size.is_some_and(|size| size > max_bytes as u64) {
            return Err(fs_error(
                format!(
                    "cannot read {:?}: {} bytes exceeds the {max_bytes}-byte limit",
                    target.display_path,
                    info.size.unwrap_or(0)
                ),
                FsErrorCode::FsTooLarge,
            ));
        }
        let mut remote = sandbox
            .files()
            .read_stream(target.target_key.as_str(), signal)
            .await
            .map_err(|error| map_error(error, "read", &target.display_path, signal))?;
        let mut cancellation = CancelOnDrop(Some(remote.cancel.clone()));
        let mut output = Vec::new();
        while let Some(chunk) = remote.stream.next().await {
            assert_not_aborted(signal, "read")?;
            let chunk =
                chunk.map_err(|error| map_error(error, "read", &target.display_path, signal))?;
            if output.len().saturating_add(chunk.len()) > max_bytes {
                return Err(fs_error(
                    format!(
                        "cannot read {:?}: content exceeds the {max_bytes}-byte limit",
                        target.display_path
                    ),
                    FsErrorCode::FsTooLarge,
                ));
            }
            output.extend_from_slice(&chunk);
        }
        cancellation.0 = None;
        Ok(output)
    }

    async fn list_dir(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Vec<FsDirEntry>> {
        let Some(info) = self.stat(target, signal).await? else {
            return Err(fs_error(
                format!("cannot list {:?}: not found", target.display_path),
                FsErrorCode::FsNotFound,
            ));
        };
        if info.kind != FsKind::Directory {
            return Err(fs_error(
                format!("cannot list {:?}: not a directory", target.display_path),
                FsErrorCode::FsNotDirectory,
            ));
        }
        let result = async {
            let sandbox = self.sandbox().await?;
            let listed = sandbox
                .files()
                .list(target.target_key.as_str(), 1, signal)
                .await?;
            let mut entries = Vec::new();
            for entry in listed {
                let display_path = posix_join(&target.display_path, &entry.name);
                let (canonical, resolved) = if entry.symlink_target.is_some() {
                    let canonical = self
                        .canonical_path(sandbox.as_ref(), &entry.path, signal)
                        .await?;
                    let resolved = self.probe(&canonical, &display_path, signal).await?;
                    (canonical, resolved)
                } else {
                    (entry.path.clone(), Some(entry.clone()))
                };
                entries.push(FsDirEntry {
                    name: entry.name,
                    kind: resolved.as_ref().map_or(FsKind::Other, entry_kind),
                    target: FsTarget {
                        target_key: FsTargetKey::new(canonical),
                        display_path,
                    },
                    version: resolved.as_ref().map(entry_version),
                    size: resolved
                        .as_ref()
                        .filter(|entry| entry.kind == E2bFileType::File)
                        .map(|entry| entry.size),
                });
            }
            entries.sort_by(|left, right| left.name.cmp(&right.name));
            Ok(entries)
        }
        .await;
        result.map_err(|error| map_error(error, "list", &target.display_path, signal))
    }

    async fn write_text(
        &self,
        target: &FsTarget,
        content: &str,
        expected: Option<&FsWriteIntent>,
        signal: Option<&AbortSignal>,
        _sandbox_policy: Option<&SandboxExecutionPolicy>,
    ) -> anyhow::Result<FsWriteOutcome> {
        let lock = self.lock_for(target.target_key.as_str());
        let _guard = lock.lock().await;
        let existing = self
            .probe(target.target_key.as_str(), &target.display_path, signal)
            .await?;
        if existing
            .as_ref()
            .is_some_and(|entry| entry.kind != E2bFileType::File)
        {
            return Err(fs_error(
                format!("cannot write {:?}: not a regular file", target.display_path),
                FsErrorCode::FsNotRegularFile,
            ));
        }
        Self::check_write_intent(existing.as_ref(), expected, target)?;
        let before = if existing.is_some() {
            self.read_for_diff(target, signal).await?
        } else {
            None
        };
        let create_if_absent = matches!(expected, Some(FsWriteIntent::CreateIfAbsent));
        let version = self
            .write_atomic(target, content, existing.as_ref(), create_if_absent, signal)
            .await?;
        Ok(FsWriteOutcome {
            operation: if existing.is_some() {
                FsWriteOperation::Update
            } else {
                FsWriteOperation::Create
            },
            version,
            before,
            after: normalize_line_endings(content),
        })
    }

    async fn edit_text(
        &self,
        target: &FsTarget,
        edit: &FsEditRequest,
        expected: Option<&FsVersion>,
        signal: Option<&AbortSignal>,
        _sandbox_policy: Option<&SandboxExecutionPolicy>,
    ) -> anyhow::Result<FsEditOutcome> {
        let lock = self.lock_for(target.target_key.as_str());
        let _guard = lock.lock().await;
        let Some(existing) = self
            .probe(target.target_key.as_str(), &target.display_path, signal)
            .await?
        else {
            return Err(stale_edit(target));
        };
        if existing.kind != E2bFileType::File {
            return Err(fs_error(
                format!("cannot edit {:?}: not a regular file", target.display_path),
                FsErrorCode::FsNotRegularFile,
            ));
        }
        if expected.is_some_and(|version| entry_version(&existing) != *version) {
            return Err(stale_edit(target));
        }
        let raw = self.read_for_edit(target, signal).await?;
        let before = normalize_line_endings(&raw);
        let after = literal_edit(&before, edit, &target.display_path)?;
        let storage = restore_line_endings(&after, detects_crlf(&raw));
        let version = self
            .write_atomic(target, &storage, Some(&existing), false, signal)
            .await?;
        Ok(FsEditOutcome {
            version,
            before,
            after,
        })
    }
}

struct CancelOnDrop(Option<Arc<dyn Fn() + Send + Sync>>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(cancel) = self.0.take() {
            cancel();
        }
    }
}

fn fs_error(message: impl Into<String>, code: FsErrorCode) -> anyhow::Error {
    FsError::new(message, code).into()
}

fn stale_edit(target: &FsTarget) -> anyhow::Error {
    fs_error(
        format!(
            "cannot edit {:?}: file changed since it was read",
            target.display_path
        ),
        FsErrorCode::FsStaleVersion,
    )
}

fn assert_not_aborted(signal: Option<&AbortSignal>, operation: &str) -> anyhow::Result<()> {
    if signal.is_some_and(AbortSignal::is_aborted) {
        return Err(fs_error(
            format!("{operation} aborted"),
            FsErrorCode::FsAborted,
        ));
    }
    Ok(())
}

fn map_error(
    error: anyhow::Error,
    operation: &str,
    display_path: &str,
    signal: Option<&AbortSignal>,
) -> anyhow::Error {
    if error.downcast_ref::<FsError>().is_some() {
        return error;
    }
    let message = format!("{error:#}");
    if signal.is_some_and(AbortSignal::is_aborted)
        || message.contains("AbortError")
        || message.eq_ignore_ascii_case("aborted")
    {
        return fs_error(format!("{operation} aborted"), FsErrorCode::FsAborted);
    }
    if error.downcast_ref::<E2bFileNotFound>().is_some() {
        return fs_error(
            format!("cannot {operation} {display_path:?}: not found"),
            FsErrorCode::FsNotFound,
        );
    }
    if message.to_ascii_lowercase().contains("permission denied")
        || message
            .to_ascii_lowercase()
            .contains("operation not permitted")
    {
        return fs_error(
            format!("cannot {operation} {display_path:?}: permission denied"),
            FsErrorCode::FsPermissionDenied,
        );
    }
    fs_error(
        format!("cannot {operation} {display_path:?}: {message}"),
        FsErrorCode::FsIoError,
    )
}

fn normalize_line_endings(value: &str) -> String {
    value.replace("\r\n", "\n")
}

fn detects_crlf(value: &str) -> bool {
    let sample = &value[..value.len().min(4096)];
    let crlf = sample.matches("\r\n").count();
    let lf = sample.matches('\n').count().saturating_sub(crlf);
    crlf > lf
}

fn restore_line_endings(value: &str, crlf: bool) -> String {
    if crlf {
        normalize_line_endings(value).replace('\n', "\r\n")
    } else {
        value.to_owned()
    }
}

fn decode_text(bytes: &[u8], display_path: &str, binary_sample: usize) -> anyhow::Result<String> {
    if bytes[..bytes.len().min(binary_sample)].contains(&0) {
        return Err(fs_error(
            format!("cannot read {display_path:?}: binary file"),
            FsErrorCode::FsNotText,
        ));
    }
    String::from_utf8(bytes.to_vec()).map_err(|_| {
        fs_error(
            format!("cannot read {display_path:?}: invalid UTF-8 text"),
            FsErrorCode::FsNotText,
        )
    })
}

fn decode_utf8_prefix(
    pending: &mut Vec<u8>,
    final_chunk: bool,
    display_path: &str,
) -> anyhow::Result<Option<String>> {
    match std::str::from_utf8(pending) {
        Ok(text) => {
            let text = text.to_owned();
            pending.clear();
            Ok(Some(text))
        }
        Err(error) if error.error_len().is_none() && !final_chunk => {
            let valid = error.valid_up_to();
            let text = std::str::from_utf8(&pending[..valid])
                .expect("valid_up_to is valid UTF-8")
                .to_owned();
            pending.drain(..valid);
            Ok(Some(text))
        }
        Err(_) => Err(fs_error(
            format!("cannot read {display_path:?}: invalid UTF-8 text"),
            FsErrorCode::FsNotText,
        )),
    }
}

fn literal_edit(
    content: &str,
    request: &FsEditRequest,
    display_path: &str,
) -> anyhow::Result<String> {
    let old = normalize_line_endings(&request.old_string);
    let new = normalize_line_endings(&request.new_string);
    if old.is_empty() {
        return Err(fs_error(
            format!("cannot edit {display_path:?}: old_string must be non-empty"),
            FsErrorCode::FsEditNotFound,
        ));
    }
    let matches = content.match_indices(&old).count();
    if matches == 0 {
        return Err(fs_error(
            format!("cannot edit {display_path:?}: old_string was not found"),
            FsErrorCode::FsEditNotFound,
        ));
    }
    if !request.replace_all && matches != 1 {
        return Err(fs_error(
            format!("cannot edit {display_path:?}: old_string matched {matches} times"),
            FsErrorCode::FsAmbiguousEdit,
        ));
    }
    Ok(if request.replace_all {
        content.replace(&old, &new)
    } else {
        content.replacen(&old, &new, 1)
    })
}

fn entry_kind(entry: &E2bEntryInfo) -> FsKind {
    match entry.kind {
        E2bFileType::File => FsKind::File,
        E2bFileType::Directory => FsKind::Directory,
        E2bFileType::Other => FsKind::Other,
    }
}

fn entry_version(entry: &E2bEntryInfo) -> FsVersion {
    let facts = serde_json::to_vec(&(
        entry.metadata.get(VERSION_METADATA_KEY),
        &entry.path,
        entry.kind,
        entry.size,
        entry.mode,
        &entry.modified_time,
        &entry.symlink_target,
    ))
    .expect("entry facts serialize");
    FsVersion::new(format!("e2b:{}", hex::encode(Sha256::digest(facts))))
}

fn decode_canonical_path(encoded: &str) -> anyhow::Result<String> {
    if encoded.is_empty() {
        anyhow::bail!("fs-e2b: canonical path transport returned invalid base64");
    }
    let framed = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| anyhow::anyhow!("fs-e2b: canonical path transport returned invalid base64"))?;
    anyhow::ensure!(
        base64::engine::general_purpose::STANDARD.encode(&framed) == encoded,
        "fs-e2b: canonical path transport returned invalid base64"
    );
    anyhow::ensure!(
        framed.len() >= 2 && framed.last() == Some(&0) && !framed[..framed.len() - 1].contains(&0),
        "fs-e2b: canonical path transport returned invalid NUL framing"
    );
    let path = std::str::from_utf8(&framed[..framed.len() - 1])
        .map_err(|_| anyhow::anyhow!("fs-e2b: canonical path is not valid UTF-8"))?;
    anyhow::ensure!(
        path.starts_with('/'),
        "fs-e2b: canonical path is not absolute"
    );
    Ok(path.to_owned())
}

fn quote_shell_arg(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn control_envs() -> BTreeMap<String, String> {
    BTreeMap::from([("LC_ALL".to_owned(), "C".to_owned())])
}

fn posix_resolve(cwd: &str, path: &str) -> String {
    let combined = if path.starts_with('/') {
        path.to_owned()
    } else {
        posix_join(cwd, path)
    };
    let mut parts = Vec::new();
    for part in combined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    format!("/{}", parts.join("/"))
}

fn posix_join(parent: &str, child: &str) -> String {
    if parent == "/" {
        format!("/{child}")
    } else {
        format!("{}/{child}", parent.trim_end_matches('/'))
    }
}

fn posix_dirname(path: &str) -> &str {
    path.rsplit_once('/').map_or(
        ".",
        |(parent, _)| {
            if parent.is_empty() { "/" } else { parent }
        },
    )
}

fn posix_basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Builds the loader-compatible E2B filesystem plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, _config| {
        Box::pin(async move {
            E2bFileSystem::install(&context)?;
            Ok(())
        })
    })
}
