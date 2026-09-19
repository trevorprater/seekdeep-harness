//! Filesystem-seam source access for the generic stdio LSP provider.

use std::{error::Error, fmt};

use futures::StreamExt;
use seekdeep_fs::{FileSystem, FsKind, FsTarget};
use seekdeep_llm::AbortSignal;

use crate::abort::throw_if_aborted;

/// A canonical workspace in the filesystem and subprocess execution world.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostWorkspace {
    /// Stable filesystem identity used for provider pooling.
    pub target: FsTarget,
    /// Canonical absolute path accepted as a subprocess working directory.
    pub canonical_path: String,
    /// Canonical file URI sent during LSP initialization.
    pub file_url: String,
}

/// A validated source and the exact URI sent to the language server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostSource {
    /// Canonical file URI in the execution world's platform syntax.
    pub file_url: String,
    /// Current complete UTF-8 text.
    pub text: String,
}

/// Resolves and validates one workspace through the filesystem provider.
///
/// # Errors
///
/// Returns the signal's abort reason, a wrapped resolution failure, an
/// unmodified metadata-provider failure, or a not-a-directory failure.
pub async fn canonicalize_workspace(
    filesystem: &dyn FileSystem,
    workspace_root: &str,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<HostWorkspace> {
    throw_if_aborted(signal)?;
    let target = match filesystem.resolve(workspace_root, None, signal).await {
        Ok(target) => target,
        Err(error) => {
            throw_if_aborted(signal)?;
            return Err(wrap_provider_error(
                format!("workspace root \"{workspace_root}\" cannot be resolved"),
                error,
            ));
        }
    };
    throw_if_aborted(signal)?;
    let info = match filesystem.stat(&target, signal).await {
        Ok(info) => info,
        Err(error) => {
            throw_if_aborted(signal)?;
            return Err(error);
        }
    };
    throw_if_aborted(signal)?;
    if !info.is_some_and(|info| info.kind == FsKind::Directory) {
        anyhow::bail!("workspace root \"{workspace_root}\" is not a directory");
    }
    Ok(HostWorkspace {
        canonical_path: filesystem.process_path(&target),
        file_url: filesystem.file_url(&target),
        target,
    })
}

/// Resolves, contains, and reads one byte-bounded source through the filesystem.
///
/// # Errors
///
/// Returns the signal's abort reason, a wrapped provider failure, containment
/// failure, or the exact configured byte-limit failure.
pub async fn read_host_source(
    filesystem: &dyn FileSystem,
    file_path: &str,
    workspace: &HostWorkspace,
    max_document_bytes: usize,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<HostSource> {
    throw_if_aborted(signal)?;
    let target = match filesystem
        .resolve(file_path, Some(&workspace.canonical_path), signal)
        .await
    {
        Ok(target) => target,
        Err(error) => {
            throw_if_aborted(signal)?;
            return Err(wrap_provider_error(
                format!("source \"{file_path}\" cannot be resolved"),
                error,
            ));
        }
    };
    throw_if_aborted(signal)?;
    if !filesystem.contains(&workspace.target, &target) {
        anyhow::bail!("source \"{file_path}\" resolves outside the workspace");
    }

    let mut stream = match filesystem.stream_text(&target, signal).await {
        Ok(stream) => stream,
        Err(error) => {
            throw_if_aborted(signal)?;
            return Err(wrap_provider_error(
                format!("source \"{file_path}\" could not be read"),
                error,
            ));
        }
    };
    let mut chunks = Vec::new();
    let mut bytes = 0usize;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                throw_if_aborted(signal)?;
                return Err(wrap_provider_error(
                    format!("source \"{file_path}\" could not be read"),
                    error,
                ));
            }
        };
        throw_if_aborted(signal)?;
        bytes = bytes.saturating_add(chunk.len());
        if bytes > max_document_bytes {
            break;
        }
        chunks.push(chunk);
    }
    if bytes > max_document_bytes {
        anyhow::bail!(
            "source \"{file_path}\" exceeds the {max_document_bytes}-byte limit; reading stopped after {bytes} bytes"
        );
    }
    throw_if_aborted(signal)?;
    Ok(HostSource {
        file_url: filesystem.file_url(&target),
        text: chunks.concat(),
    })
}

#[derive(Debug)]
struct ProviderBoundaryError {
    prefix: String,
    source: anyhow::Error,
}

impl fmt::Display for ProviderBoundaryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.prefix, self.source)
    }
}

impl Error for ProviderBoundaryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

fn wrap_provider_error(prefix: String, source: anyhow::Error) -> anyhow::Error {
    ProviderBoundaryError { prefix, source }.into()
}
