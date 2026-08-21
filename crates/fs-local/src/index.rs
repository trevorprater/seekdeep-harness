//! Host-filesystem implementation of ctx.fs.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use seekdeep_cordis::{Context, Plugin};
use seekdeep_fs::{
    FileSystem, FileSystemService,
    types::{
        FsDirEntry, FsEditOutcome, FsEditRequest, FsError, FsErrorCode, FsInfo, FsPathInfo,
        FsPathKind, FsTarget, FsVersion, FsWriteIntent, FsWriteOperation, FsWriteOutcome,
    },
};
use seekdeep_llm::AbortSignal;
use seekdeep_sandbox::SandboxExecutionPolicy;
use seekdeep_schemastery::Schema;
use serde::{Deserialize, Serialize};

use crate::fsio::{
    LocalDirEntry, apply_literal_edit, list_directory, probe, read_for_edit, read_whole_bytes,
    read_whole_text, resolve_local_target, restore_line_endings, write_file_atomic,
};

/// Cordis plugin name.
pub const NAME: &str = "fs-local";

/// Services required by the local filesystem backend.
pub const INJECT: &[&str] = &[];

const DEFAULT_DIFF_BASIS_MAX_BYTES: u64 = 10 * 1024 * 1024;
const MAX_DIFF_BASIS_BYTES: u64 = 1_073_741_823;

/// Configuration for the local filesystem backend.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Base directory for relative paths.
    pub cwd: Option<String>,
    /// Exclusive UTF-8 byte limit on each overwrite-diff side.
    pub diff_basis_max_bytes: Option<u64>,
}

/// The source-compatible admission schema for Config.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn config_schema() -> Schema {
    Schema::object([
        (
            "cwd",
            Schema::string().with_default(std::env::current_dir().map_or_else(
                |_| "/".to_owned(),
                |path| path.to_string_lossy().into_owned(),
            )),
        ),
        (
            "diffBasisMaxBytes",
            Schema::number().with_default(DEFAULT_DIFF_BASIS_MAX_BYTES),
        ),
    ])
}

#[derive(Clone, Debug)]
struct ResolvedConfig {
    cwd: String,
    diff_basis_max_bytes: u64,
}

fn ensure_not_aborted(signal: Option<&AbortSignal>, verb: &str) -> anyhow::Result<()> {
    if signal.is_some_and(AbortSignal::is_aborted) {
        anyhow::bail!(FsError::new(
            format!("{verb} aborted"),
            FsErrorCode::FsAborted
        ));
    }
    Ok(())
}

/// The host-filesystem backend.
pub struct LocalFileSystem {
    config: ResolvedConfig,
    write_lock: tokio::sync::Mutex<()>,
}

impl LocalFileSystem {
    /// Validates configuration and builds the backend.
    ///
    /// # Errors
    ///
    /// Returns an invalid diff-basis-bytes failure.
    pub fn new(config: Config) -> anyhow::Result<Arc<Self>> {
        let cwd = config.cwd.unwrap_or_else(|| {
            std::env::current_dir().map_or_else(
                |_| "/".to_owned(),
                |path| path.to_string_lossy().into_owned(),
            )
        });
        let diff_basis_max_bytes = config
            .diff_basis_max_bytes
            .unwrap_or(DEFAULT_DIFF_BASIS_MAX_BYTES);
        anyhow::ensure!(
            diff_basis_max_bytes > 0 && diff_basis_max_bytes <= MAX_DIFF_BASIS_BYTES,
            "fs-local: diffBasisMaxBytes must be a positive safe integer no greater than {MAX_DIFF_BASIS_BYTES}"
        );
        Ok(Arc::new(Self {
            config: ResolvedConfig {
                cwd,
                diff_basis_max_bytes,
            },
            write_lock: tokio::sync::Mutex::new(()),
        }))
    }

    /// Publishes this backend on the ctx.fs seat.
    ///
    /// # Errors
    ///
    /// Returns duplicate-service or inactive-owner failures.
    pub fn install(context: &Context, config: Config) -> anyhow::Result<Arc<Self>> {
        let backend = Self::new(config)?;
        FileSystemService::new(backend.clone()).provide(context)?;
        Ok(backend)
    }

    fn version_after_write(after: Option<&crate::fsio::PathInfo>, target: &FsTarget) -> FsVersion {
        after.map_or_else(
            || FsVersion::new(format!("missing:{}", target.target_key.as_str())),
            |info| info.version.clone(),
        )
    }
}

#[async_trait]
impl FileSystem for LocalFileSystem {
    async fn resolve(
        &self,
        path: &str,
        cwd: Option<&str>,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<FsTarget> {
        ensure_not_aborted(signal, "resolve")?;
        let local = resolve_local_target(cwd.unwrap_or(&self.config.cwd), path).await?;
        ensure_not_aborted(signal, "resolve")?;
        Ok(FsTarget {
            target_key: local.target_key,
            display_path: local.display_path,
        })
    }

    fn process_path(&self, target: &FsTarget) -> String {
        target.target_key.as_str().to_owned()
    }

    fn file_url(&self, target: &FsTarget) -> String {
        url::Url::from_file_path(self.process_path(target))
            .map_or_else(|()| String::new(), |url| url.to_string())
    }

    fn contains(&self, parent: &FsTarget, child: &FsTarget) -> bool {
        Path::new(&self.process_path(child))
            .strip_prefix(Path::new(&self.process_path(parent)))
            .is_ok()
    }

    async fn stat(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Option<FsInfo>> {
        ensure_not_aborted(signal, "stat")?;
        let info = probe(target.target_key.as_str()).await?;
        ensure_not_aborted(signal, "stat")?;
        Ok(info.map(|info| FsInfo {
            version: info.version,
            kind: info.kind,
            size: Some(info.size),
        }))
    }

    async fn lstat(
        &self,
        path: &str,
        cwd: Option<&str>,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Option<FsPathInfo>> {
        ensure_not_aborted(signal, "lstat")?;
        if path.trim().is_empty() {
            anyhow::bail!(FsError::new(
                "file_path must be a non-empty string",
                FsErrorCode::FsNotFound
            ));
        }
        let absolute = crate::fsio::resolve_local_target(cwd.unwrap_or(&self.config.cwd), path)
            .await?
            .target_key;
        let meta = tokio::fs::symlink_metadata(absolute.as_str()).await;
        ensure_not_aborted(signal, "lstat")?;
        let info = match meta {
            Ok(meta) => {
                let kind = if meta.file_type().is_symlink() {
                    FsPathKind::Symlink
                } else if meta.is_file() {
                    FsPathKind::File
                } else if meta.is_dir() {
                    FsPathKind::Directory
                } else {
                    FsPathKind::Other
                };
                Some(FsPathInfo {
                    version: crate::fsio::version_of_meta(&meta),
                    kind,
                    size: Some(meta.len()),
                })
            }
            Err(_) => None,
        };
        Ok(info)
    }

    async fn read_text(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<String> {
        Ok(read_whole_text(
            &crate::fsio::LocalTarget {
                display_path: target.display_path.clone(),
                target_key: target.target_key.clone(),
            },
            signal,
        )
        .await?)
    }

    async fn stream_text(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<futures::stream::BoxStream<'static, anyhow::Result<String>>> {
        let text = read_whole_text(
            &crate::fsio::LocalTarget {
                display_path: target.display_path.clone(),
                target_key: target.target_key.clone(),
            },
            signal,
        )
        .await?;
        Ok(futures::stream::once(async move { Ok(text) }).boxed())
    }

    async fn read_bytes(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
        max_bytes: usize,
    ) -> anyhow::Result<Vec<u8>> {
        Ok(read_whole_bytes(
            &crate::fsio::LocalTarget {
                display_path: target.display_path.clone(),
                target_key: target.target_key.clone(),
            },
            signal,
            max_bytes,
        )
        .await?)
    }

    async fn list_dir(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Vec<FsDirEntry>> {
        let entries = list_directory(
            &crate::fsio::LocalTarget {
                display_path: target.display_path.clone(),
                target_key: target.target_key.clone(),
            },
            signal,
        )
        .await?;
        Ok(entries
            .into_iter()
            .map(|entry: LocalDirEntry| FsDirEntry {
                name: entry.name,
                kind: entry.kind,
                target: FsTarget {
                    target_key: entry.target.target_key,
                    display_path: entry.target.display_path,
                },
                version: entry.version,
                size: entry.size,
            })
            .collect())
    }

    async fn write_text(
        &self,
        target: &FsTarget,
        content: &str,
        expected: Option<&FsWriteIntent>,
        signal: Option<&AbortSignal>,
        _sandbox_policy: Option<&SandboxExecutionPolicy>,
    ) -> anyhow::Result<FsWriteOutcome> {
        let _guard = self.write_lock.lock().await;
        let existing = probe(target.target_key.as_str()).await?;
        if existing
            .as_ref()
            .is_some_and(|info| info.kind != seekdeep_fs::types::FsKind::File)
        {
            anyhow::bail!(FsError::new(
                format!(
                    "cannot write \"{}\": not a regular file",
                    target.display_path
                ),
                FsErrorCode::FsNotRegularFile
            ));
        }
        match expected {
            Some(FsWriteIntent::ReplaceIfVersion { version }) => {
                let Some(existing) = &existing else {
                    anyhow::bail!(FsError::new(
                        format!(
                            "cannot write \"{}\": file no longer exists",
                            target.display_path
                        ),
                        FsErrorCode::FsStaleVersion
                    ));
                };
                if existing.version != *version {
                    anyhow::bail!(FsError::new(
                        format!(
                            "cannot write \"{}\": file changed since it was read",
                            target.display_path
                        ),
                        FsErrorCode::FsStaleVersion
                    ));
                }
            }
            Some(FsWriteIntent::CreateIfAbsent) => {
                if existing.is_some() {
                    anyhow::bail!(FsError::new(
                        format!(
                            "cannot overwrite existing \"{}\" without reading it first",
                            target.display_path
                        ),
                        FsErrorCode::FsNotObserved
                    ));
                }
            }
            None => {}
        }
        let before =
            if existing.is_some() && (content.len() as u64) < self.config.diff_basis_max_bytes {
                Some(
                    read_whole_text(
                        &crate::fsio::LocalTarget {
                            display_path: target.display_path.clone(),
                            target_key: target.target_key.clone(),
                        },
                        signal,
                    )
                    .await?,
                )
            } else {
                None
            };
        write_file_atomic(
            target.target_key.as_str(),
            content,
            existing.as_ref().map(|info| info.mode),
            signal,
        )
        .await?;
        let after = probe(target.target_key.as_str()).await?;
        Ok(FsWriteOutcome {
            operation: if existing.is_some() {
                FsWriteOperation::Update
            } else {
                FsWriteOperation::Create
            },
            version: Self::version_after_write(after.as_ref(), target),
            before,
            after: crate::fsio::normalize_line_endings(content),
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
        let _guard = self.write_lock.lock().await;
        let existing = probe(target.target_key.as_str()).await?;
        let Some(existing) = existing else {
            anyhow::bail!(FsError::new(
                format!(
                    "cannot edit \"{}\": file changed since it was read",
                    target.display_path
                ),
                FsErrorCode::FsStaleVersion
            ));
        };
        if existing.kind != seekdeep_fs::types::FsKind::File {
            anyhow::bail!(FsError::new(
                format!(
                    "cannot edit \"{}\": not a regular file",
                    target.display_path
                ),
                FsErrorCode::FsNotRegularFile
            ));
        }
        if let Some(expected) = expected
            && existing.version != *expected
        {
            anyhow::bail!(FsError::new(
                format!(
                    "cannot edit \"{}\": file changed since it was read",
                    target.display_path
                ),
                FsErrorCode::FsStaleVersion
            ));
        }
        let (original, endings) = read_for_edit(
            target.target_key.as_str(),
            target.display_path.as_str(),
            signal,
        )
        .await?;
        let (edited, _replacements) = apply_literal_edit(
            &original,
            &edit.old_string,
            &edit.new_string,
            edit.replace_all,
            &target.display_path,
        )?;
        let content = restore_line_endings(&edited, endings);
        write_file_atomic(
            target.target_key.as_str(),
            &content,
            Some(existing.mode),
            signal,
        )
        .await?;
        let after = probe(target.target_key.as_str()).await?;
        Ok(FsEditOutcome {
            version: Self::version_after_write(after.as_ref(), target),
            before: original,
            after: edited,
        })
    }
}

/// Builds the loader-compatible local filesystem plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: Config = serde_json::from_value(config)?;
            LocalFileSystem::install(&context, config)?;
            Ok(())
        })
    })
    .with_config_validator(|value: &serde_json::Value| {
        config_schema()
            .resolve(value)
            .map_err(|error| anyhow::anyhow!("{error}"))
    })
}
