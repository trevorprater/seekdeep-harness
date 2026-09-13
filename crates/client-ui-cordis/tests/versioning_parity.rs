//! Versioned card metadata, latest-card ownership, and visible-status parity.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use seekdeep_client_ui_cordis::*;
use seekdeep_cordis_client_runner::DynamicCordisLivePackage;
use seekdeep_cordis_dynamic_types::{DynamicCordisActiveRun, DynamicCordisInventoryPackage};
use seekdeep_identity::SessionId;
use serde_json::json;

fn plugin() -> CordisDynamicPluginId {
    CordisDynamicPluginId::new("clock-1")
}

fn package() -> CordisDynamicPackageId {
    CordisDynamicPackageId::new("pkg-1")
}

fn run() -> CordisDynamicPluginRunId {
    CordisDynamicPluginRunId::new("run-1")
}

fn row(has_client_half: bool) -> DynamicCordisInventoryRow {
    DynamicCordisInventoryRow {
        plugin_id: plugin(),
        agent_id: SessionId::new("session-1"),
        packages: vec![DynamicCordisInventoryPackage {
            package_id: package(),
            name: "Clock".to_owned(),
            purpose: "show time".to_owned(),
            has_host_half: true,
            has_client_half,
        }],
        current_package_id: Some(package()),
        next_package_id: None,
        active_run: Some(DynamicCordisActiveRun {
            package_id: package(),
            plugin_run_id: run(),
        }),
        latest_run: None,
    }
}

#[test]
fn define_reads_symmetric_host_and_client_source_fields() {
    let block = ToolCallBlock::Running(RunningToolCall {
        args_raw: json!({
            "plugin": {"kind": "new", "idPrefix": "clock"},
            "name": "Clock",
            "purpose": "show time",
            "code": {"host": "HOST_CODE", "client": "CLIENT_CODE"}
        })
        .to_string(),
    });
    let card = cordis_define_card(&block);
    assert_eq!(card.plugin_id, None);
    assert_eq!(card.package_id, None);
    assert_eq!(card.host_code.as_deref(), Some("HOST_CODE"));
    assert_eq!(card.client_code.as_deref(), Some("CLIENT_CODE"));
    assert_eq!(card.state, CordisToolState::Running);
}

#[test]
fn run_reads_exact_activation_metadata_from_a_successful_result() {
    let block = ToolCallBlock::Settled(Box::new(SettledToolResult {
        kind: "tool-result".to_owned(),
        seq: 9,
        call: Some(SettledToolCall {
            name: "cordis_run".to_owned(),
            args_raw: json!({
                "pluginId": plugin(),
                "packageId": package(),
                "mode": "run"
            })
            .to_string(),
        }),
        content: vec![json!({"type": "text", "text": "running"})],
        is_error: false,
        error: None,
        meta: Some(json!({
            "pluginId": plugin(),
            "packageId": package(),
            "pluginRunId": run()
        })),
    }));
    let card = cordis_run_card(&block);
    assert_eq!(card.plugin_id, Some(plugin()));
    assert_eq!(card.package_id, Some(package()));
    assert_eq!(card.plugin_run_id, Some(run()));
    assert_eq!(card.mode, Some(DynamicCordisRunMode::Run));
    assert_eq!(card.seq, Some(9));
    assert_eq!(card.state, CordisToolState::Ok);
}

#[test]
fn running_run_keeps_target_identities_while_waiting_for_approval() {
    let block = ToolCallBlock::Running(RunningToolCall {
        args_raw: json!({
            "pluginId": plugin(),
            "packageId": package(),
            "mode": "update"
        })
        .to_string(),
    });
    let card = cordis_run_card(&block);
    assert_eq!(card.plugin_id, Some(plugin()));
    assert_eq!(card.package_id, Some(package()));
    assert_eq!(card.plugin_run_id, None);
    assert_eq!(card.mode, Some(DynamicCordisRunMode::Update));
    assert_eq!(card.state, CordisToolState::Running);
}

#[test]
fn run_card_store_keeps_the_greatest_session_log_sequence() {
    let registry = CordisRunCardRegistry::default();
    let store = registry.for_session(SessionId::new("session-1"));
    assert!(Arc::ptr_eq(
        &store,
        &registry.for_session(SessionId::new("session-1"))
    ));
    let changes = Arc::new(AtomicUsize::new(0));
    let observed = changes.clone();
    let _subscription = store.subscribe(Arc::new(move || {
        observed.fetch_add(1, Ordering::Relaxed);
    }));
    let key = cordis_tool_view_key(&plugin(), &package());
    assert!(store.observe(CordisRunCardPointer {
        key: key.clone(),
        call_id: "new".to_owned(),
        seq: 20,
        plugin_run_id: run(),
    }));
    assert!(!store.observe(CordisRunCardPointer {
        key: key.clone(),
        call_id: "old".to_owned(),
        seq: 10,
        plugin_run_id: CordisDynamicPluginRunId::new("run-0"),
    }));
    assert_eq!(store.snapshot().get(&key).unwrap().call_id, "new");
    assert_eq!(changes.load(Ordering::Relaxed), 1);
}

#[test]
fn visible_status_distinguishes_host_only_client_pending_and_fully_loaded() {
    assert_eq!(
        cordis_visible_status(&row(false), &package(), &[]),
        CordisVisibleStatus::Running
    );
    assert_eq!(
        cordis_visible_status(&row(true), &package(), &[]),
        CordisVisibleStatus::ClientPending
    );
    let loaded = vec![DynamicCordisLivePackage {
        plugin_id: plugin(),
        package_id: package(),
        plugin_run_id: run(),
        name: "Clock".to_owned(),
        slots: Vec::new(),
        style_count: 0,
    }];
    assert_eq!(
        cordis_visible_status(&row(true), &package(), &loaded),
        CordisVisibleStatus::Running
    );
}

#[test]
fn generated_locales_keep_exact_namespace_keys_and_representative_copy() {
    assert_eq!(CORDIS_LOCALE_NAMESPACE, "cordis");
    assert_eq!(english_locale().len(), 49);
    assert_eq!(chinese_locale().len(), 49);
    assert_eq!(
        locale_message("en", "status.clientPending"),
        Some("Client ready to activate")
    );
    assert_eq!(
        locale_message("zh", "panel.empty"),
        Some("还没有定义任何插件")
    );
    assert_eq!(locale_message("fr", "panel.empty"), None);
}
