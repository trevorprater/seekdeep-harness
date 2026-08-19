//! Shared path resolution and regular-file validation for model-facing read tools.

use std::sync::Arc;

use seekdeep_cordis::{Context, EventArgs};
use seekdeep_fs::{FS, FsError, FsErrorCode, FsInfo, FsKind, FsObservation, FsTarget};
use seekdeep_tools::ToolExecution;

use crate::session_resolve_options;

/// Emits a present-or-absent observation for one resolved target.
///
/// # Errors
///
/// Returns a listener failure or panic before the emission detaches.
pub fn emit_fs_observed(
    context: &Context,
    target: &FsTarget,
    observation: FsObservation,
    exec: &ToolExecution,
) -> anyhow::Result<()> {
    let args = EventArgs::from_values(vec![
        Arc::new(target.clone()),
        Arc::new(observation),
        Arc::new(exec.clone()),
    ]);
    context.events().emit(context, "fs/observed", &args)
}

/// Resolves a model-supplied path, observes absence, and requires a regular file.
///
/// # Errors
///
/// Returns a not-found or not-a-regular-file failure, or a filesystem failure.
pub async fn resolve_regular_read_target(
    context: &Context,
    exec: &ToolExecution,
    requested_path: &str,
) -> anyhow::Result<(FsTarget, FsInfo)> {
    let filesystem = context
        .get(FS)
        .ok_or_else(|| anyhow::anyhow!("tool-fs requires fs"))?
        .filesystem();
    let options = session_resolve_options(exec, requested_path, None);
    let target = filesystem
        .resolve(
            requested_path,
            options.cwd.as_deref(),
            Some(&options.signal),
        )
        .await?;
    let info = filesystem.stat(&target, Some(&exec.signal())).await?;
    let Some(info) = info else {
        emit_fs_observed(context, &target, FsObservation::Absent, exec)?;
        return Err(anyhow::Error::new(FsError::new(
            format!("cannot read {:?}: not found", target.display_path),
            FsErrorCode::FsNotFound,
        )));
    };
    if info.kind != FsKind::File {
        return Err(anyhow::Error::new(FsError::new(
            format!("cannot read {:?}: not a regular file", target.display_path),
            FsErrorCode::FsNotRegularFile,
        )));
    }
    Ok((target, info))
}
