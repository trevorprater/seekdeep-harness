//! Crash-durable same-directory atomic whole-file replacement.

use std::path::Path;

use tokio::io::AsyncWriteExt as _;
use uuid::Uuid;

/// Durably replaces one path through a private same-directory temporary file.
///
/// # Errors
///
/// Preserves filesystem open, write, sync, rename, and directory-sync failures.
pub async fn write_atomic(path: &Path, data: &str) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "target has no parent directory",
        )
    })?;
    let temporary = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    let result = async {
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).await?;
        file.write_all(data.as_bytes()).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temporary, path).await?;
        fsync_directory(parent).await
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result
}

#[cfg(unix)]
async fn fsync_directory(path: &Path) -> std::io::Result<()> {
    tokio::fs::File::open(path).await?.sync_all().await
}

#[cfg(not(unix))]
async fn fsync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}
