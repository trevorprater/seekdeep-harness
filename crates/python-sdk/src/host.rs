//! Injectable process-boundary facts and package lookup callbacks.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Instant};

use crate::{Error, Notification, NotificationData, Result};

/// Host effects used by the synchronous SDK, including its monotonic timeout clock.
#[derive(Clone)]
pub struct Host {
    /// Retains the foreign owner until both reader threads finish.
    pub reader_lifetime:
        Arc<dyn Fn() -> Result<Arc<dyn std::any::Any + Send + Sync>> + Send + Sync>,
    /// Creates the shared notification object before routing it to subscribers.
    pub notification: Arc<dyn Fn(NotificationData) -> Result<Notification> + Send + Sync>,
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
            reader_lifetime: Arc::new(|| Ok(Arc::new(()))),
            notification: Arc::new(|value| Ok(Notification::new(value))),
            cwd: Arc::new(|| std::env::current_dir().map_err(|error| Error::io(&error, None))),
            environment: Arc::new(|| std::env::vars().collect()),
            monotonic: Arc::new(move || epoch.elapsed().as_secs_f64()),
            bundled_launch,
            bundled_config,
        }
    }
}
