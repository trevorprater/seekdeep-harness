//! Injectable process-boundary facts and package lookup callbacks.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Instant};

use crate::{Error, Result};

/// Host effects used by the synchronous SDK, including its monotonic timeout clock.
#[derive(Clone)]
pub struct Host {
    /// Current working directory when a path is resolved.
    pub cwd: Arc<dyn Fn() -> Result<PathBuf> + Send + Sync>,
    /// Caller environment captured when the subprocess starts.
    pub environment: Arc<dyn Fn() -> BTreeMap<String, String> + Send + Sync>,
    /// Monotonic seconds for request deadlines; tests may inject a virtual clock.
    pub monotonic: Arc<dyn Fn() -> f64 + Send + Sync>,
    /// Late lookup of the bundled runtime package's launch function.
    pub bundled_launch: Arc<dyn Fn() -> Result<Vec<String>> + Send + Sync>,
    /// Late lookup of the bundled runtime package's default configuration.
    pub bundled_config: Arc<dyn Fn() -> Result<String> + Send + Sync>,
}

impl Host {
    /// Connects native process facts to separately owned package-lookup adapters.
    pub fn native(
        bundled_launch: Arc<dyn Fn() -> Result<Vec<String>> + Send + Sync>,
        bundled_config: Arc<dyn Fn() -> Result<String> + Send + Sync>,
    ) -> Self {
        let epoch = Instant::now();
        Self {
            cwd: Arc::new(|| std::env::current_dir().map_err(|error| Error::io(&error, None))),
            environment: Arc::new(|| std::env::vars().collect()),
            monotonic: Arc::new(move || epoch.elapsed().as_secs_f64()),
            bundled_launch,
            bundled_config,
        }
    }
}
