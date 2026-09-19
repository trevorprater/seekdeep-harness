//! Shared status derivation over Host inventory and the page-local Client live set.

use seekdeep_cordis_client_runner::DynamicCordisLivePackage;
use seekdeep_cordis_dynamic_types::{
    CordisDynamicPackageId, DynamicCordisInventoryPackage, DynamicCordisInventoryRow,
};
use serde::Serialize;

/// Product-visible lifecycle reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CordisVisibleStatus {
    /// No Host activation owns this Package.
    Idle,
    /// Host is running and this page has not loaded the required Client half.
    ClientPending,
    /// Every required half is running.
    Running,
}

/// Locates one immutable Package inside its Plugin row.
#[must_use]
pub fn package_of<'a>(
    row: &'a DynamicCordisInventoryRow,
    package_id: &CordisDynamicPackageId,
) -> Option<&'a DynamicCordisInventoryPackage> {
    row.packages
        .iter()
        .find(|package| package.package_id == *package_id)
}

/// Derives the visible state of one immutable Package.
#[must_use]
pub fn cordis_visible_status(
    row: &DynamicCordisInventoryRow,
    package_id: &CordisDynamicPackageId,
    loaded: &[DynamicCordisLivePackage],
) -> CordisVisibleStatus {
    let Some(run) = row
        .active_run
        .as_ref()
        .filter(|run| run.package_id == *package_id)
    else {
        return CordisVisibleStatus::Idle;
    };
    if package_of(row, package_id).is_none_or(|package| !package.has_client_half) {
        return CordisVisibleStatus::Running;
    }
    if loaded.iter().any(|live| {
        live.plugin_id == row.plugin_id
            && live.package_id == *package_id
            && live.plugin_run_id == run.plugin_run_id
    }) {
        CordisVisibleStatus::Running
    } else {
        CordisVisibleStatus::ClientPending
    }
}
