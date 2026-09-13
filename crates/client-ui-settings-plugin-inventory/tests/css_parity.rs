//! Exact plugin-inventory CSS projection parity.

use regex::Regex;
use seekdeep_client_ui_settings_plugin_inventory::PLUGIN_INVENTORY_STYLES;

const SOURCE: &str = include_str!(
    "../../../packages/client/ui-settings-plugin-inventory/src/client/PluginInventorySettingsTab.module.css"
);

fn namespace(source: &str) -> String {
    Regex::new(r"\.([A-Za-z_][A-Za-z0-9_-]*)")
        .unwrap()
        .replace_all(source, ".seekdeep-plugin-inventory-$1")
        .into_owned()
}

#[test]
fn compiled_plugin_inventory_styles_are_an_exact_namespaced_source_projection() {
    assert_eq!(PLUGIN_INVENTORY_STYLES, namespace(SOURCE));
}
