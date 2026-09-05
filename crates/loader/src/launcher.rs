//! Generic `cordis.yml` launcher over the Rust Loader and Cordis core.

use std::{future::Future, path::Path};

use seekdeep_cordis::Context;

use crate::PluginCatalog;

/// Loads `cordis.yml` from `cwd`, then holds active compositions until shutdown.
///
/// An empty composition settles and exits immediately, matching an empty Node
/// event loop. Any active plugin generation is held explicitly because Rust
/// executor tasks do not provide a safe process-global active-handle probe.
///
/// # Errors
///
/// Returns current-directory URL, file, parse, import, activation, or disposal failures.
pub async fn run_cordis_file(
    cwd: &Path,
    shutdown: impl Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let config_path = cwd.join("cordis.yml");
    let launcher_path = cwd.join(".cordis-launcher.yml");
    let configured_path = serde_json::to_string(&config_path.to_string_lossy())?;
    let source =
        format!("- id: include\n  name: cordis:include\n  config:\n    path: {configured_path}\n");
    let context = Context::new();
    let catalog = PluginCatalog::new();
    let composition = catalog
        .load_yaml_at(&context, &source, &launcher_path)
        .await?;
    if !composition.fibers().is_empty() {
        shutdown.await?;
    }
    composition.dispose().await?;
    context.fiber().dispose().await?;
    Ok(())
}

/// Waits for the host termination signal.
///
/// # Errors
///
/// Returns signal-handler installation failures.
pub async fn termination_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut interrupt = signal(SignalKind::interrupt())?;
        let mut terminate = signal(SignalKind::terminate())?;
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}
