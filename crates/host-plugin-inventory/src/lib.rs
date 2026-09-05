//! Trusted Loader plugin inventory and live projection service.

use std::sync::Arc;

use seekdeep_api_gateway::register_invocable_service_if_available;
use seekdeep_cordis::{FiberState, Plugin, ServiceKey};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_loader::{LOADER, LoaderSettlement};
use seekdeep_typert_protocol::{
    RemoteInvocationMarker, RemoteMethodMarker, TypertBoundaryValue, TypertHostArgument,
    TypertInvocableService, TypertInvocationFuture,
};
use serde::{Deserialize, Serialize};

/// Cordis service name exposed to trusted Remote adapters.
pub const NAME: &str = "pluginInventory";
/// Loader dependency required by the inventory projection.
pub const INJECT: &[&str] = &["loader"];
/// Typed inventory service slot.
pub const PLUGIN_INVENTORY: ServiceKey<PluginInventoryService> = ServiceKey::new(NAME);
/// Package identity reserved in the invariant registry.
pub const INVARIANT_PACKAGE: &str = "seekdeep-host-plugin-inventory";

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

/// Read-through projection of the current Loader tree.
pub struct PluginInventoryService {
    loader: Arc<LoaderSettlement>,
}

impl std::fmt::Debug for PluginInventoryService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PluginInventoryService")
            .finish_non_exhaustive()
    }
}

impl PluginInventoryService {
    /// Creates a live read-through projection.
    #[must_use]
    pub fn new(loader: Arc<LoaderSettlement>) -> Arc<Self> {
        Arc::new(Self { loader })
    }

    /// Reads current non-group entries in Loader order.
    ///
    /// # Errors
    ///
    /// Returns when the Loader generation is unavailable.
    pub fn list(&self) -> anyhow::Result<PluginInventorySnapshot> {
        let entries = self
            .loader
            .entries()?
            .into_iter()
            .filter(|entry| !entry.group)
            .map(|entry| PluginInventoryEntry {
                entry_id: PluginEntryId::new(entry.id.as_str()),
                module_name: entry.plugin.as_str().to_owned(),
                enabled: !entry.disabled,
                fiber_phase: entry.state.and_then(fiber_phase),
            })
            .collect();
        Ok(PluginInventorySnapshot { entries })
    }
}

impl TypertInvocableService for PluginInventoryService {
    fn service_key(&self) -> &str {
        NAME
    }

    fn namespace(&self) -> &str {
        NAME
    }

    fn remote_methods(&self) -> Vec<RemoteMethodMarker> {
        vec![RemoteMethodMarker {
            method: "list".to_owned(),
            export_name: None,
            invocation: RemoteInvocationMarker::Direct,
        }]
    }

    fn parameter_names(&self, implementation: &str) -> Option<Vec<String>> {
        (implementation == "list").then(Vec::new)
    }

    fn has_method(&self, implementation: &str) -> bool {
        implementation == "list"
    }

    fn invoke(
        self: Arc<Self>,
        implementation: &str,
        _arguments: Vec<TypertHostArgument>,
    ) -> TypertInvocationFuture {
        let implementation = implementation.to_owned();
        Box::pin(async move {
            anyhow::ensure!(implementation == "list", "unknown plugin inventory method");
            Ok(TypertBoundaryValue::json(serde_json::to_value(
                self.list()?,
            )?))
        })
    }
}

fn fiber_phase(state: FiberState) -> Option<PluginFiberPhase> {
    match state {
        FiberState::Pending => Some(PluginFiberPhase::Pending),
        FiberState::Loading => Some(PluginFiberPhase::Loading),
        FiberState::Active => Some(PluginFiberPhase::Active),
        FiberState::Failed => Some(PluginFiberPhase::Failed),
        FiberState::Unloading => Some(PluginFiberPhase::Unloading),
        FiberState::Disposed => None,
    }
}

/// Builds the Loader-compatible inventory plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, _| {
        Box::pin(async move {
            let loader = context
                .get(LOADER)
                .ok_or_else(|| anyhow::anyhow!("plugin inventory requires loader"))?;
            let inventory = PluginInventoryService::new(loader);
            context.provide(PLUGIN_INVENTORY, inventory)?;
            register_invocable_service_if_available(&context, PLUGIN_INVENTORY)?;
            Ok(())
        })
    })
}

/// Registers the package's intentionally empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(INVARIANT_PACKAGE, InvariantInstaller::noop())
}

#[cfg(test)]
mod tests {
    use seekdeep_cordis::{Context, Plugin, PluginFiber};
    use seekdeep_invariants::InvariantConfig;
    use seekdeep_loader::{
        Entry, EntryId, EntryParent, EntryUpdate, LOADER, LoadedComposition, LoaderSettlement,
        PluginCatalog, PluginSpecifier,
    };
    use serde_json::json;

    use super::*;

    struct InventoryHarness {
        composition: LoadedComposition,
        inventory_fiber: Arc<PluginFiber>,
        inventory: Arc<PluginInventoryService>,
        loader: Arc<LoaderSettlement>,
    }

    impl InventoryHarness {
        async fn open() -> Self {
            let catalog = PluginCatalog::new();
            catalog
                .register_named(
                    "active",
                    Plugin::new("active", std::iter::empty::<&str>(), |_, _| {
                        Box::pin(async { Ok(()) })
                    }),
                )
                .unwrap();
            catalog
                .register_named(
                    "pending",
                    Plugin::new("pending", ["neverReady"], |_, _| Box::pin(async { Ok(()) })),
                )
                .unwrap();
            let context = Context::new();
            let composition = catalog.load_yaml(&context, "[]\n").await.unwrap();
            let inventory_fiber = context.plugin(plugin(), serde_json::Value::Null).unwrap();
            inventory_fiber.await_settled().await.unwrap();
            let inventory = context.get(PLUGIN_INVENTORY).unwrap();
            let loader = context.get(LOADER).unwrap();
            Self {
                composition,
                inventory_fiber,
                inventory,
                loader,
            }
        }

        async fn close(self) {
            self.inventory_fiber.dispose().await.unwrap();
            self.composition.dispose().await.unwrap();
        }
    }

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

    #[tokio::test]
    async fn inventory_exposes_one_direct_list_remote() {
        let harness = InventoryHarness::open().await;
        assert_eq!(harness.inventory.service_key(), NAME);
        assert_eq!(harness.inventory.namespace(), NAME);
        assert_eq!(
            harness.inventory.remote_methods(),
            [RemoteMethodMarker {
                method: "list".to_owned(),
                export_name: None,
                invocation: RemoteInvocationMarker::Direct,
            }]
        );
        assert_eq!(harness.inventory.parameter_names("list"), Some(Vec::new()));
        assert!(harness.inventory.has_method("list"));
        assert!(
            harness
                .inventory
                .clone()
                .invoke("list", Vec::new())
                .await
                .unwrap()
                .as_json()
                .is_some()
        );
        harness.close().await;
    }

    #[tokio::test]
    async fn invariant_companion_reserves_and_releases_package_ownership() {
        let context = Context::new();
        let registry =
            Arc::new(InvariantRegistry::new(&context, &InvariantConfig::default()).unwrap());
        let first = register_invariant(&registry).unwrap();
        first.await_ready().await.unwrap();
        assert!(registry.is_registered(INVARIANT_PACKAGE));
        first.dispose().await.unwrap();
        assert!(!registry.is_registered(INVARIANT_PACKAGE));
        let second = register_invariant(&registry).unwrap();
        second.await_ready().await.unwrap();
        second.dispose().await.unwrap();
        context.fiber().dispose().await.unwrap();
    }

    #[tokio::test]
    async fn live_inventory_reads_loader_state_after_create_update_and_remove() {
        let harness = InventoryHarness::open().await;
        let inventory = &harness.inventory;
        let loader = &harness.loader;

        let active = EntryId::new("active-id").unwrap();
        loader
            .create_entry(
                Entry::new(active.clone(), PluginSpecifier::new("active").unwrap()),
                EntryParent::Root,
                None,
            )
            .await
            .unwrap();
        let pending = EntryId::new("pending-id").unwrap();
        loader
            .create_entry(
                Entry::new(pending.clone(), PluginSpecifier::new("pending").unwrap()),
                EntryParent::Root,
                None,
            )
            .await
            .unwrap();
        let mut disabled = Entry::new(
            EntryId::new("disabled-id").unwrap(),
            PluginSpecifier::new("not-installed").unwrap(),
        );
        disabled.disabled = true;
        loader
            .create_entry(disabled, EntryParent::Root, None)
            .await
            .unwrap();
        let mut group = Entry::new(
            EntryId::new("group-id").unwrap(),
            PluginSpecifier::new("active").unwrap(),
        );
        group.group = true;
        loader
            .create_entry(group, EntryParent::Root, None)
            .await
            .unwrap();

        let snapshot = inventory.list().unwrap();
        assert_eq!(snapshot.entries.len(), 3);
        assert!(snapshot.entries.contains(&PluginInventoryEntry {
            entry_id: PluginEntryId::new("active-id"),
            module_name: "active".to_owned(),
            enabled: true,
            fiber_phase: Some(PluginFiberPhase::Active),
        }));
        assert!(snapshot.entries.contains(&PluginInventoryEntry {
            entry_id: PluginEntryId::new("pending-id"),
            module_name: "pending".to_owned(),
            enabled: true,
            fiber_phase: Some(PluginFiberPhase::Pending),
        }));
        assert!(snapshot.entries.contains(&PluginInventoryEntry {
            entry_id: PluginEntryId::new("disabled-id"),
            module_name: "not-installed".to_owned(),
            enabled: false,
            fiber_phase: None,
        }));

        loader
            .update_entry(
                &active,
                EntryUpdate {
                    disabled: Some(true),
                    ..EntryUpdate::default()
                },
                EntryParent::Keep,
                None,
            )
            .await
            .unwrap();
        let updated = inventory
            .list()
            .unwrap()
            .entries
            .into_iter()
            .find(|entry| entry.entry_id.as_str() == "active-id")
            .unwrap();
        assert!(!updated.enabled);
        assert_eq!(updated.fiber_phase, None);

        loader.remove_entry(&pending).await.unwrap();
        assert!(
            inventory
                .list()
                .unwrap()
                .entries
                .iter()
                .all(|entry| entry.entry_id.as_str() != "pending-id")
        );
        harness.close().await;
    }
}
