//! Serialized browser reload queue over an injected platform.

use std::sync::Arc;

use futures::{FutureExt, future::BoxFuture};
use parking_lot::Mutex;

use crate::PluginsEventFrame;

/// Browser operations for one exact entry swap.
pub trait ClientHmrPlatform: Send + Sync + 'static {
    /// Runs invalidate/prefetch/teardown/style-removal/refresh for `id`.
    fn reload(&self, id: String) -> BoxFuture<'static, anyhow::Result<()>>;
}

/// Detached browser task executor.
pub trait ClientHmrSpawner: Send + Sync + 'static {
    /// Drives one serialized queue tail.
    fn spawn(&self, future: BoxFuture<'static, ()>);
}

/// Browser diagnostic outlet.
pub type ClientHmrLogger = Arc<dyn Fn(String, Option<String>) + Send + Sync>;

/// Frame consumer whose rebuilt work never interleaves.
pub struct ClientHmrRuntime {
    platform: Arc<dyn ClientHmrPlatform>,
    spawner: Arc<dyn ClientHmrSpawner>,
    logger: ClientHmrLogger,
    tail: Mutex<futures::future::Shared<BoxFuture<'static, ()>>>,
}

impl std::fmt::Debug for ClientHmrRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClientHmrRuntime")
            .finish_non_exhaustive()
    }
}

impl ClientHmrRuntime {
    /// Creates one page-local serialized consumer.
    #[must_use]
    pub fn new(
        platform: Arc<dyn ClientHmrPlatform>,
        spawner: Arc<dyn ClientHmrSpawner>,
        logger: ClientHmrLogger,
    ) -> Arc<Self> {
        Arc::new(Self {
            platform,
            spawner,
            logger,
            tail: Mutex::new(futures::future::ready(()).boxed().shared()),
        })
    }

    /// Handles one known or forward-compatible frame.
    pub fn handle(&self, frame: PluginsEventFrame) {
        let PluginsEventFrame::Rebuilt { id, .. } = frame else {
            return;
        };
        let previous = self.tail.lock().clone();
        let platform = self.platform.clone();
        let logger = self.logger.clone();
        let failure_id = id.clone();
        let next = async move {
            previous.await;
            if let Err(error) = platform.reload(id).await {
                logger(
                    format!("client-hmr: reload of {failure_id:?} failed"),
                    Some(format!("{error:#}")),
                );
            }
        }
        .boxed()
        .shared();
        *self.tail.lock() = next.clone();
        self.spawner.spawn(Box::pin(next));
    }

    /// Waits for every frame admitted before this call.
    pub async fn settled(&self) {
        let tail = self.tail.lock().clone();
        tail.await;
    }
}
