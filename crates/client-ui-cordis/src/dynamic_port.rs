//! Host operations used by the frame-wide Cordis panel.

use futures::future::BoxFuture;
use seekdeep_cordis_dynamic_types::{CordisDynamicPluginId, DynamicCordisInventoryRow};
use seekdeep_identity::SessionId;

/// Result of one panel lifecycle gesture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CordisActionResult {
    /// The requested state already holds or the transition succeeded.
    Success,
    /// The Host rejected the transition with user-facing detail.
    Failure {
        /// Stable diagnostic displayed by the panel.
        message: String,
    },
}

impl CordisActionResult {
    /// Whether the requested transition succeeded.
    #[must_use]
    pub const fn is_ok(&self) -> bool {
        matches!(self, Self::Success)
    }
}

/// RPC boundary kept outside presentation and inventory state.
pub trait CordisDynamicPort: Send + Sync + 'static {
    /// Stops one Plugin while retaining its immutable Packages.
    fn stop(
        &self,
        session_id: SessionId,
        plugin_id: CordisDynamicPluginId,
    ) -> BoxFuture<'static, anyhow::Result<CordisActionResult>>;

    /// Stops and removes one Plugin together with every Package.
    fn remove(
        &self,
        session_id: SessionId,
        plugin_id: CordisDynamicPluginId,
    ) -> BoxFuture<'static, anyhow::Result<CordisActionResult>>;

    /// Reads the frame-wide Plugin inventory.
    fn inventory(&self) -> BoxFuture<'static, anyhow::Result<Vec<DynamicCordisInventoryRow>>>;
}
