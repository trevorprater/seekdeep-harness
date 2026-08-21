//! Trusted Loader plugin inventory wire vocabulary.

use serde::{Deserialize, Serialize};

/// Stable Loader-tree identity of one configured plugin entry.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PluginEntryId(String);

impl PluginEntryId {
    /// Brands one Loader-owned identity without normalizing it.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the exact wire spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Lifecycle state of an entry's live root Fiber.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginFiberPhase {
    /// Waiting for required services.
    Pending,
    /// Plugin callback is running.
    Loading,
    /// Plugin is active.
    Active,
    /// Plugin callback or configuration failed.
    Failed,
    /// Disposers are running.
    Unloading,
}

/// One non-group Loader entry exposed to trusted clients.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginInventoryEntry {
    /// Stable configured-entry identity.
    pub entry_id: PluginEntryId,
    /// Exact module specifier imported by the Loader entry.
    pub module_name: String,
    /// Effective enablement, including disabled ancestor groups.
    pub enabled: bool,
    /// Live Fiber phase, or `None` when no root Fiber exists.
    pub fiber_phase: Option<PluginFiberPhase>,
}

/// Point-in-time trusted plugin inventory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginInventorySnapshot {
    /// Ordered non-group entries.
    pub entries: Vec<PluginInventoryEntry>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn inventory_types_preserve_exact_nullable_phase_wire_shape() {
        let snapshot = PluginInventorySnapshot {
            entries: vec![
                PluginInventoryEntry {
                    entry_id: PluginEntryId::new("active"),
                    module_name: "plugin-a".to_owned(),
                    enabled: true,
                    fiber_phase: Some(PluginFiberPhase::Active),
                },
                PluginInventoryEntry {
                    entry_id: PluginEntryId::new("missing"),
                    module_name: "plugin-b".to_owned(),
                    enabled: false,
                    fiber_phase: None,
                },
            ],
        };
        let value = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(
            value,
            json!({
                "entries": [
                    {"entryId": "active", "moduleName": "plugin-a", "enabled": true, "fiberPhase": "active"},
                    {"entryId": "missing", "moduleName": "plugin-b", "enabled": false, "fiberPhase": null},
                ]
            })
        );
        assert_eq!(
            serde_json::from_value::<PluginInventorySnapshot>(value).unwrap(),
            snapshot
        );
        assert!(
            serde_json::from_value::<PluginFiberPhase>(json!("disposed")).is_err(),
            "the source union is closed and excludes disposed"
        );
    }
}
