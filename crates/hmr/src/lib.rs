//! Lifecycle-owned Host file watching over Loader HMR transactions.

pub mod error;
pub mod ignore;

use std::{path::PathBuf, sync::Arc};

use futures::future::BoxFuture;
use seekdeep_cordis::{Plugin, ServiceKey, fiber::EffectHandle};
use seekdeep_loader::LOADER;
use serde::{Deserialize, Serialize};

/// Cordis service name.
pub const NAME: &str = "hmr";
/// Services required by the Host watcher.
pub const INJECT: &[&str] = &["loader", "timer"];
/// Typed Host HMR service slot.
pub const HMR: ServiceKey<HostHmrService> = ServiceKey::new(NAME);

/// Full-process restart callback selected by the launcher.
pub type RestartHook = Arc<dyn Fn() -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

/// Host watcher configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    /// Base directory for relative roots.
    pub base: Option<PathBuf>,
    /// Recursive roots observed for changes.
    pub root: Vec<PathBuf>,
    /// Quiet period used to coalesce a burst.
    pub debounce: u64,
    /// Picomatch patterns matched against paths relative to the canonical base.
    pub ignored: Vec<String>,
    /// Additional Chokidar options passed to each watcher.
    #[serde(flatten)]
    pub watcher_options: serde_json::Map<String, serde_json::Value>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base: None,
            root: vec![PathBuf::from(".")],
            debounce: 100,
            ignored: vec![
                "**/node_modules".to_owned(),
                "**/.*".to_owned(),
                "cache".to_owned(),
                "data".to_owned(),
            ],
            watcher_options: serde_json::Map::new(),
        }
    }
}

mod watcher;
pub use watcher::{ConfigRefresh, HostHmrService};

/// Builds the Loader-compatible Host HMR plugin.
#[must_use]
pub fn plugin(restart: RestartHook) -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        let restart = restart.clone();
        Box::pin(async move {
            let config: Config = serde_json::from_value(config)?;
            let loader = context
                .get(LOADER)
                .ok_or_else(|| anyhow::anyhow!("Host HMR requires loader"))?;
            let service = HostHmrService::start(context.clone(), loader, config, restart)?;
            let cleanup = service.clone();
            context.own(EffectHandle::new("Host HMR watcher", move || {
                let cleanup = cleanup.clone();
                Box::pin(async move { cleanup.dispose().await })
            }))?;
            context.provide(HMR, service)?;
            Ok(())
        })
    })
}
