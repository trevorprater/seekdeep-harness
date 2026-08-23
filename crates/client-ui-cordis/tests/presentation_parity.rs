//! User-visible card, panel, Slot, and `@pluginId` projection parity.

use std::collections::{BTreeMap, BTreeSet};

use seekdeep_client_ui_cordis::*;
use seekdeep_cordis_client_runner::{CordisRunActivity, DynamicCordisLivePackage};
use seekdeep_cordis_dynamic_types::{
    CordisDiagnosticPhase, CordisHalfState, CordisHalfStatus, CordisRunDiagnostic, CordisRunStatus,
    DynamicCordisActiveRun, DynamicCordisInventoryPackage, DynamicCordisRunAttempt,
};
use seekdeep_identity::SessionId;

fn plugin(value: &str) -> CordisDynamicPluginId {
    CordisDynamicPluginId::new(value)
}

fn package(value: &str) -> CordisDynamicPackageId {
    CordisDynamicPackageId::new(value)
}

fn run(value: &str) -> CordisDynamicPluginRunId {
    CordisDynamicPluginRunId::new(value)
}

fn inventory_row(plugin_id: &str, session_id: &str, client: bool) -> DynamicCordisInventoryRow {
    DynamicCordisInventoryRow {
        plugin_id: plugin(plugin_id),
        agent_id: SessionId::new(session_id),
        packages: vec![DynamicCordisInventoryPackage {
            package_id: package("pkg-1"),
            name: "Clock".to_owned(),
            purpose: "show time".to_owned(),
            has_host_half: true,
            has_client_half: client,
        }],
        current_package_id: Some(package("pkg-1")),
        next_package_id: None,
        active_run: Some(DynamicCordisActiveRun {
            package_id: package("pkg-1"),
            plugin_run_id: run("run-1"),
        }),
        latest_run: None,
    }
}

fn run_card() -> CordisRunCard {
    CordisRunCard {
        plugin_id: Some(plugin("clock-1")),
        package_id: Some(package("pkg-1")),
        plugin_run_id: Some(run("run-1")),
        mode: Some(DynamicCordisRunMode::Run),
        seq: Some(10),
        output: Some("running".to_owned()),
        error_summary: None,
        state: CordisToolState::Ok,
    }
}

fn failed_attempt() -> DynamicCordisRunAttempt {
    DynamicCordisRunAttempt {
        plugin_run_id: run("run-1"),
        package_id: package("pkg-1"),
        mode: DynamicCordisRunMode::Run,
        status: CordisRunStatus::Failed,
        approval_request_id: None,
        requires_approval: None,
        host: CordisHalfState {
            status: CordisHalfStatus::Failed,
            waiting_for: Vec::new(),
            error: Some("broken".to_owned()),
        },
        client: CordisHalfState {
            status: CordisHalfStatus::Absent,
            waiting_for: Vec::new(),
            error: None,
        },
        error: Some(CordisRunDiagnostic {
            phase: CordisDiagnosticPhase::HostApply,
            message: "broken".to_owned(),
            stack: None,
            plugin_id: plugin("clock-1"),
            package_id: package("pkg-1"),
            plugin_run_id: run("run-1"),
        }),
    }
}

#[test]
fn define_row_uses_live_status_removal_and_source_availability_fallback() {
    let card = CordisDefineCard {
        plugin_id: Some(plugin("clock-1")),
        package_id: Some(package("pkg-1")),
        name: Some("Clock".to_owned()),
        purpose: Some("show time".to_owned()),
        host_code: Some("HOST".to_owned()),
        client_code: None,
        output: None,
        error_summary: None,
        state: CordisToolState::Ok,
    };
    let row = cordis_define_row_model(
        card.clone(),
        "call-1",
        &[inventory_row("clock-1", "session-1", false)],
        &BTreeSet::new(),
        &[],
        CordisSourceTab::Client,
    );
    assert_eq!(row.reading, CordisDefineReading::Running);
    assert_eq!(row.active_source, CordisSourceTab::Host);
    assert_eq!(row.active_code.as_deref(), Some("HOST"));
    assert!(row.expandable);

    let removed = [plugin("clock-1")].into_iter().collect();
    assert_eq!(
        cordis_define_row_model(card, "call-1", &[], &removed, &[], CordisSourceTab::Host,).reading,
        CordisDefineReading::Removed
    );
}

#[test]
fn define_row_keeps_accessible_lifecycle_and_call_id_fallback() {
    let card = CordisDefineCard {
        plugin_id: None,
        package_id: None,
        name: None,
        purpose: None,
        host_code: None,
        client_code: None,
        output: None,
        error_summary: None,
        state: CordisToolState::Running,
    };
    let row = cordis_define_row_model(
        card,
        "call-9",
        &[],
        &BTreeSet::new(),
        &[],
        CordisSourceTab::Client,
    );
    assert_eq!(row.name, "call-9");
    assert_eq!(row.a11y_state_key, Some("a11y.defining"));
    assert!(!row.expandable);
}

#[test]
fn run_row_assigns_business_view_only_to_the_latest_successful_exact_generation() {
    let card = run_card();
    let key = cordis_tool_view_key(&plugin("clock-1"), &package("pkg-1"));
    let latest = [(
        key.clone(),
        CordisRunCardPointer {
            key,
            call_id: "call-new".to_owned(),
            seq: 20,
            plugin_run_id: run("run-2"),
        },
    )]
    .into_iter()
    .collect();
    let model = cordis_run_row_model(
        card,
        "call-old",
        &[inventory_row("clock-1", "session-1", false)],
        &BTreeSet::new(),
        &[],
        &latest,
        &BTreeMap::new(),
    );
    assert_eq!(model.reading, CordisRunReading::Superseded);
    assert!(!model.show_business);
    assert_eq!(model.summary, "clock-1 · pkg-1");
}

#[test]
fn run_row_precedence_is_removed_then_approval_then_exact_attempt_failure() {
    let card = run_card();
    let mut row = inventory_row("clock-1", "session-1", false);
    row.latest_run = Some(failed_attempt());
    let failure = cordis_run_row_model(
        card.clone(),
        "call-1",
        &[row.clone()],
        &BTreeSet::new(),
        &[],
        &BTreeMap::new(),
        &BTreeMap::new(),
    );
    assert_eq!(failure.reading, CordisRunReading::Failed);
    assert_eq!(failure.failure_message.as_deref(), Some("broken"));

    let activity = [(
        plugin("clock-1"),
        CordisRunActivity::AwaitingApproval {
            request_id: ApprovalRequestId::new("approval-1"),
            agent_id: SessionId::new("session-1"),
            package_id: package("pkg-1"),
            mode: DynamicCordisRunMode::Run,
            name: "Clock".to_owned(),
            purpose: "show time".to_owned(),
        },
    )]
    .into_iter()
    .collect();
    assert_eq!(
        cordis_run_row_model(
            card.clone(),
            "call-1",
            &[row.clone()],
            &BTreeSet::new(),
            &[],
            &BTreeMap::new(),
            &activity,
        )
        .reading,
        CordisRunReading::AwaitingApproval
    );
    assert_eq!(
        cordis_run_row_model(
            card,
            "call-1",
            &[row],
            &[plugin("clock-1")].into_iter().collect(),
            &[],
            &BTreeMap::new(),
            &activity,
        )
        .reading,
        CordisRunReading::Removed
    );
}

#[test]
fn action_row_localizes_stop_and_remove_and_prefers_error_summary() {
    let card = CordisActionCard {
        plugin_id: Some(plugin("clock-1")),
        output: Some("broken\ndetail".to_owned()),
        error_summary: Some("broken".to_owned()),
        state: CordisToolState::Error,
    };
    let remove = cordis_action_row_model(card, "call-1", "cordis_undefine");
    assert!(remove.remove);
    assert_eq!(remove.title_key, "row.removeTitle");
    assert_eq!(remove.summary, "broken");
}

#[test]
fn panel_merges_unlisted_approval_activity_groups_sessions_and_orders_blockers_first() {
    let listed = inventory_row("clock-1", "session-1", false);
    let activities = [(
        plugin("pending-1"),
        CordisRunActivity::AwaitingApproval {
            request_id: ApprovalRequestId::new("approval-1"),
            agent_id: SessionId::new("session-2"),
            package_id: package("pkg-2"),
            mode: DynamicCordisRunMode::Run,
            name: "Pending".to_owned(),
            purpose: "needs approval".to_owned(),
        },
    )]
    .into_iter()
    .collect();
    let model = cordis_panel_model(
        &[listed],
        &activities,
        &[],
        Some(&SessionId::new("session-1")),
        &BTreeMap::new(),
        &BTreeSet::new(),
    );
    assert_eq!(model.mine.len(), 1);
    assert_eq!(model.mine[0].status, CordisPanelStatus::Running);
    assert_eq!(model.theirs[0].plugin_id.as_str(), "pending-1");
    assert_eq!(model.theirs[0].status, CordisPanelStatus::AwaitingApproval);
    assert_eq!(model.approvals, 1);
    assert_eq!(model.running, 1);
}

#[test]
fn panel_exposes_exact_actions_for_running_and_client_pending_versions() {
    let host_only = cordis_panel_model(
        &[inventory_row("clock-1", "session-1", false)],
        &BTreeMap::new(),
        &[],
        Some(&SessionId::new("session-1")),
        &BTreeMap::new(),
        &BTreeSet::new(),
    );
    let actions = &host_only.mine[0].actions;
    assert!(actions.contains(&CordisPanelAction::Stop));
    assert!(actions.contains(&CordisPanelAction::Remove));
    assert!(!actions.contains(&CordisPanelAction::RunSelected));

    let client = inventory_row("clock-1", "session-1", true);
    let pending = cordis_panel_model(
        &[client],
        &BTreeMap::new(),
        &[],
        Some(&SessionId::new("session-1")),
        &BTreeMap::new(),
        &BTreeSet::new(),
    );
    assert_eq!(pending.mine[0].status, CordisPanelStatus::ClientPending);
    assert!(
        pending.mine[0]
            .actions
            .contains(&CordisPanelAction::RetryClient)
    );
}

#[test]
fn input_trigger_filters_by_owner_and_query_and_prefers_next_package_purpose() {
    let mut row = inventory_row("clock-1", "session-1", false);
    row.packages.push(DynamicCordisInventoryPackage {
        package_id: package("pkg-2"),
        name: "Clock v2".to_owned(),
        purpose: "show seconds".to_owned(),
        has_host_half: true,
        has_client_half: false,
    });
    row.next_package_id = Some(package("pkg-2"));
    let other = inventory_row("clock-other", "session-2", false);
    let candidates =
        cordis_trigger_candidates(&[row, other], &SessionId::new("session-1"), "ock-1");
    assert_eq!(
        candidates,
        vec![CordisTriggerCandidate {
            name: "clock-1".to_owned(),
            description: Some("show seconds".to_owned()),
        }]
    );
    assert_eq!(cordis_trigger_pick(&candidates[0]), "@clock-1 ");
}

#[test]
fn slot_contract_has_all_five_entries_and_run_declares_the_business_child() {
    assert_eq!(UI_CORDIS_INJECT.len(), 6);
    assert_eq!(UI_CORDIS_REGISTRATIONS.len(), 5);
    let run = UI_CORDIS_REGISTRATIONS
        .iter()
        .find(|entry| entry.key == Some("cordis_run"))
        .unwrap();
    assert_eq!(run.slot, "tool.call.toolview");
    assert!(run.declares_tool_view);
    assert_eq!(CORDIS_TOOL_VIEW_SLOT, "tool.view.cordis");
}

#[test]
fn fully_loaded_client_package_changes_panel_status_to_running() {
    let row = inventory_row("clock-1", "session-1", true);
    let loaded = [DynamicCordisLivePackage {
        plugin_id: plugin("clock-1"),
        package_id: package("pkg-1"),
        plugin_run_id: run("run-1"),
        name: "Clock".to_owned(),
        slots: Vec::new(),
        style_count: 0,
    }];
    let model = cordis_panel_model(
        &[row],
        &BTreeMap::new(),
        &loaded,
        Some(&SessionId::new("session-1")),
        &BTreeMap::new(),
        &BTreeSet::new(),
    );
    assert_eq!(model.mine[0].status, CordisPanelStatus::Running);
}
