//! Per-call sandbox-policy fence over the trusted local filesystem backend.

use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use futures::stream::BoxStream;
use seekdeep_cordis::{Context, Plugin};
use seekdeep_fs::{
    FileSystem, FileSystemService,
    types::{
        FsDirEntry, FsEditOutcome, FsEditRequest, FsError, FsErrorCode, FsInfo, FsPathInfo,
        FsTarget, FsVersion, FsWriteIntent, FsWriteOutcome,
    },
};
use seekdeep_fs_local::{Config, LocalFileSystem};
use seekdeep_llm::AbortSignal;
use seekdeep_sandbox::{SandboxExecutionPolicy, SandboxMode, writable_roots};
use seekdeep_sandbox_policy::{SANDBOX_POLICY, SandboxPolicyRequest, SandboxPolicyService};

/// Cordis plugin name.
pub const NAME: &str = "fs-sandbox";
/// Required capability seats.
pub const INJECT: &[&str] = &["sandboxPolicy"];

fn comparable_path(path: &str, case_sensitive: bool) -> String {
    if case_sensitive {
        path.to_owned()
    } else {
        path.to_lowercase()
    }
}

fn lexically_under(path: &str, root: &str, case_sensitive: bool) -> bool {
    let path = comparable_path(path, case_sensitive);
    let root = comparable_path(root, case_sensitive);
    if path == root {
        return true;
    }
    let prefix = if root.ends_with(std::path::MAIN_SEPARATOR) {
        root
    } else {
        format!("{root}{}", std::path::MAIN_SEPARATOR)
    };
    path.starts_with(&prefix)
}

async fn metadata_if_present(path: &Path) -> anyhow::Result<Option<std::fs::Metadata>> {
    match tokio::fs::metadata(path).await {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

/// Determines whether a canonical target is a writable root or its descendant.
///
/// The identity fallback recognizes alias-equivalent existing ancestors while
/// retaining conservative behavior for missing targets and roots.
///
/// # Errors
///
/// Returns host metadata failures other than absence or a non-directory segment.
pub async fn is_path_under(path: &str, root: &str, case_sensitive: bool) -> anyhow::Result<bool> {
    if lexically_under(path, root, case_sensitive) {
        return Ok(true);
    }
    let root = Path::new(root);
    let Some(_) = metadata_if_present(root).await? else {
        return Ok(false);
    };
    let mut ancestor = Path::new(path);
    loop {
        if metadata_if_present(ancestor).await?.is_some()
            && same_file::is_same_file(ancestor, root)?
        {
            return Ok(true);
        }
        let Some(parent) = ancestor.parent() else {
            return Ok(false);
        };
        if parent == ancestor {
            return Ok(false);
        }
        ancestor = parent;
    }
}

/// Sandbox-enforcing filesystem provider occupying `ctx.fs`.
pub struct SandboxedFileSystem {
    local: Arc<LocalFileSystem>,
    policy: Arc<SandboxPolicyService>,
}

impl std::fmt::Debug for SandboxedFileSystem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SandboxedFileSystem")
            .field("default_mode", &self.policy.default_mode)
            .finish_non_exhaustive()
    }
}

impl SandboxedFileSystem {
    async fn checked_target(
        &self,
        target: &FsTarget,
        supplied: Option<&SandboxExecutionPolicy>,
    ) -> anyhow::Result<FsTarget> {
        let policy = match supplied {
            Some(policy) => policy.clone(),
            None => self.policy.resolve(SandboxPolicyRequest::default())?,
        };
        match policy.mode {
            SandboxMode::DangerFullAccess => return Ok(target.clone()),
            SandboxMode::ReadOnly => {
                return Err(anyhow::Error::new(FsError::new(
                    format!(
                        "cannot write \"{}\": file access denied under read-only mode",
                        target.display_path
                    ),
                    FsErrorCode::FsSandboxDenied,
                )));
            }
            SandboxMode::WorkspaceWrite => {}
        }

        let fresh = self.local.resolve(&target.display_path, None, None).await?;
        for root in writable_roots(&policy) {
            if is_path_under(
                fresh.target_key.as_str(),
                &root.to_string_lossy(),
                !cfg!(windows),
            )
            .await?
            {
                return Ok(fresh);
            }
        }
        Err(anyhow::Error::new(FsError::new(
            format!(
                "cannot write \"{}\": file access denied under workspace-write mode",
                target.display_path
            ),
            FsErrorCode::FsSandboxDenied,
        )))
    }
}

#[async_trait]
impl FileSystem for SandboxedFileSystem {
    fn sandbox_mode(&self) -> Option<SandboxMode> {
        Some(self.policy.default_mode)
    }

    async fn resolve(
        &self,
        path: &str,
        cwd: Option<&str>,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<FsTarget> {
        self.local.resolve(path, cwd, signal).await
    }

    fn process_path(&self, target: &FsTarget) -> String {
        self.local.process_path(target)
    }

    fn file_url(&self, target: &FsTarget) -> String {
        self.local.file_url(target)
    }

    fn contains(&self, parent: &FsTarget, child: &FsTarget) -> bool {
        self.local.contains(parent, child)
    }

    async fn stat(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Option<FsInfo>> {
        self.local.stat(target, signal).await
    }

    async fn lstat(
        &self,
        path: &str,
        cwd: Option<&str>,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Option<FsPathInfo>> {
        self.local.lstat(path, cwd, signal).await
    }

    async fn read_text(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<String> {
        self.local.read_text(target, signal).await
    }

    async fn stream_text(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<BoxStream<'static, anyhow::Result<String>>> {
        self.local.stream_text(target, signal).await
    }

    async fn read_bytes(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
        max_bytes: usize,
    ) -> anyhow::Result<Vec<u8>> {
        self.local.read_bytes(target, signal, max_bytes).await
    }

    async fn list_dir(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Vec<FsDirEntry>> {
        self.local.list_dir(target, signal).await
    }

    async fn write_text(
        &self,
        target: &FsTarget,
        content: &str,
        expected: Option<&FsWriteIntent>,
        signal: Option<&AbortSignal>,
        sandbox_policy: Option<&SandboxExecutionPolicy>,
    ) -> anyhow::Result<FsWriteOutcome> {
        let target = self.checked_target(target, sandbox_policy).await?;
        self.local
            .write_text(&target, content, expected, signal, None)
            .await
    }

    async fn edit_text(
        &self,
        target: &FsTarget,
        edit: &FsEditRequest,
        expected: Option<&FsVersion>,
        signal: Option<&AbortSignal>,
        sandbox_policy: Option<&SandboxExecutionPolicy>,
    ) -> anyhow::Result<FsEditOutcome> {
        let target = self.checked_target(target, sandbox_policy).await?;
        self.local
            .edit_text(&target, edit, expected, signal, None)
            .await
    }
}

/// Installs the sandboxed filesystem provider.
///
/// # Errors
///
/// Returns missing policy, invalid local configuration, or service registration failures.
pub fn apply(context: &Context, config: Config) -> anyhow::Result<Arc<SandboxedFileSystem>> {
    let policy = context
        .get(SANDBOX_POLICY)
        .ok_or_else(|| anyhow::anyhow!("fs-sandbox requires sandboxPolicy"))?;
    let local = LocalFileSystem::new(config)?;
    let filesystem = Arc::new(SandboxedFileSystem { local, policy });
    let erased: Arc<dyn FileSystem> = filesystem.clone();
    FileSystemService::new(erased).provide(context)?;
    Ok(filesystem)
}

/// Builds the Loader-compatible sandboxed filesystem plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let resolved = seekdeep_fs_local::config_schema()
                .resolve(&config)
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            let config: Config = serde_json::from_value(resolved)?;
            apply(&context, config)?;
            Ok(())
        })
    })
    .with_config_validator(|value| {
        seekdeep_fs_local::config_schema()
            .resolve(value)
            .map_err(|error| anyhow::anyhow!("{error}"))
    })
}
