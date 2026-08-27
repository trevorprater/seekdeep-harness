//! Inventory filtering, status, expansion, copy, and Host-shell parity.

use seekdeep_client_ui_settings_plugin_inventory::{
    EN, InventoryStatus, PluginFiberPhase, PluginInventoryController, PluginInventoryEntry,
    PluginInventorySnapshot, ZH, module_short_name, phase_locale_key, remote_list_error,
};

fn entry(
    id: &str,
    module: &str,
    enabled: bool,
    phase: Option<PluginFiberPhase>,
) -> PluginInventoryEntry {
    PluginInventoryEntry {
        entry_id: id.to_owned(),
        module_name: module.to_owned(),
        enabled,
        fiber_phase: phase,
    }
}

fn snapshot() -> PluginInventorySnapshot {
    PluginInventorySnapshot {
        entries: vec![
            entry(
                "8a1b2c3d",
                "@seekdeep-ai/cordis-plugin-hmr",
                true,
                Some(PluginFiberPhase::Active),
            ),
            entry(
                "pending",
                "cordis:pending-name",
                true,
                Some(PluginFiberPhase::Pending),
            ),
            entry(
                "disabled-entry",
                "@seekdeep-ai/seekdeep-host-directory-picker-native",
                false,
                None,
            ),
        ],
    }
}

#[test]
fn dictionaries_and_phase_keys_are_complete_and_order_identical() {
    assert_eq!(
        ZH.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
        EN.iter().map(|(key, _)| *key).collect::<Vec<_>>()
    );
    assert_eq!(phase_locale_key(None), "unobserved");
    assert_eq!(phase_locale_key(Some(PluginFiberPhase::Pending)), "pending");
    assert_eq!(
        phase_locale_key(Some(PluginFiberPhase::Loading)),
        "loadingPhase"
    );
    assert_eq!(phase_locale_key(Some(PluginFiberPhase::Active)), "active");
    assert_eq!(phase_locale_key(Some(PluginFiberPhase::Failed)), "failed");
    assert_eq!(
        phase_locale_key(Some(PluginFiberPhase::Unloading)),
        "unloading"
    );
}

#[test]
fn module_names_and_queries_match_module_or_loader_identity() {
    assert_eq!(module_short_name("@seekdeep-ai/cordis-plugin-hmr"), "hmr");
    assert_eq!(module_short_name("cordis:pending-name"), "pending-name");
    assert_eq!(
        module_short_name("@seekdeep-ai/seekdeep-host-directory-picker-native"),
        "directory-picker-native"
    );
    let mut controller = PluginInventoryController::default();
    let generation = controller.begin_load();
    controller.finish_load(generation, Ok(snapshot()));
    assert_eq!(controller.status, InventoryStatus::Ready);
    assert_eq!(controller.filtered_entries().len(), 3);
    controller.set_query(" disabled-entry ");
    assert_eq!(controller.filtered_entries()[0].entry_id, "disabled-entry");
    controller.set_query("CORDIS-PLUGIN-HMR");
    assert_eq!(controller.filtered_entries()[0].entry_id, "8a1b2c3d");
    controller.set_query("not-a-plugin");
    assert!(controller.filtered_entries().is_empty());
}

#[test]
fn expansion_retry_generic_failure_and_stale_completions_follow_component_state() {
    let mut controller = PluginInventoryController::default();
    let stale = controller.begin_load();
    let current = controller.begin_load();
    controller.finish_load(stale, Ok(snapshot()));
    assert_eq!(controller.status, InventoryStatus::Loading);
    controller.finish_load(current, Ok(snapshot()));
    controller.toggle("8a1b2c3d");
    assert_eq!(controller.expanded.as_deref(), Some("8a1b2c3d"));
    controller.set_query("disabled-entry");
    assert!(controller.expanded.is_none());
    controller.toggle("disabled-entry");
    controller.toggle("disabled-entry");
    assert!(controller.expanded.is_none());
    let retry = controller.begin_load();
    controller.finish_load(retry, Err(()));
    assert_eq!(controller.status, InventoryStatus::Error);
    assert_eq!(
        remote_list_error("REMOTE_ERROR", "unavailable"),
        "pluginInventory.list failed: REMOTE_ERROR: unavailable"
    );
}

#[test]
fn host_half_is_inert_and_dependency_free() {
    let plugin = seekdeep_client_ui_settings_plugin_inventory::host_plugin();
    assert!(plugin.inject().is_empty());
}
