//! Node-half, invariant-name, and canonical settings Slot contract parity.

#![cfg(not(target_arch = "wasm32"))]

use seekdeep_client_ui_settings::*;
use seekdeep_client_ui_slots::{SlotKind, SlotScope};
use seekdeep_cordis::{Context, FiberState};
use serde_json::Value;

#[tokio::test]
async fn node_half_is_a_dependency_free_no_op_placeholder() {
    let plugin = host_plugin();
    assert_eq!(plugin.name(), "client-ui-settings");
    assert!(plugin.inject().is_empty());
    let context = Context::new();
    let fiber = context.plugin(plugin, Value::Null).unwrap();
    fiber.await_settled().await.unwrap();
    assert_eq!(fiber.fiber().state(), FiberState::Active);
    fiber.dispose().await.unwrap();
}

#[test]
fn invariant_companion_keeps_the_exact_explained_empty_identity() {
    assert_eq!(INVARIANT_NAME, "client-ui-settings-invariant");
}

#[test]
fn canonical_settings_slots_keep_exact_names_kinds_and_root_scope() {
    let singles = [
        settings_trigger_slot(),
        settings_header_slot(),
        settings_close_slot(),
    ];
    assert_eq!(
        singles
            .iter()
            .map(|slot| slot.name().as_str())
            .collect::<Vec<_>>(),
        vec![
            SETTINGS_TRIGGER_SLOT,
            SETTINGS_HEADER_SLOT,
            SETTINGS_CLOSE_SLOT,
        ]
    );
    for slot in singles {
        let spec = slot.spec::<()>(None);
        assert_eq!(spec.kind, SlotKind::Single);
        assert_eq!(spec.scope, SlotScope::Root);
    }

    let lists = [
        settings_action_slot(),
        settings_section_slot(),
        settings_plugins_tab_slot(),
        settings_onboarding_slot(),
        settings_general_item_slot(),
    ];
    assert_eq!(
        lists
            .iter()
            .map(|slot| slot.name().as_str())
            .collect::<Vec<_>>(),
        vec![
            SETTINGS_ACTION_SLOT,
            SETTINGS_SECTION_SLOT,
            SETTINGS_PLUGINS_TAB_SLOT,
            SETTINGS_ONBOARDING_SLOT,
            SETTINGS_GENERAL_ITEM_SLOT,
        ]
    );
    for slot in lists {
        let spec = slot.spec::<()>(None);
        assert_eq!(spec.kind, SlotKind::List);
        assert_eq!(spec.scope, SlotScope::Root);
    }
}
