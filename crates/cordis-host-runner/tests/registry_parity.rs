//! Dynamic package registry identity, ordering, ownership, and claim parity.

use seekdeep_cordis_host_runner::{
    ApprovalRequestId, CordisDynamicPackageId, CordisDynamicPluginId, CordisDynamicPluginRunId,
    DynamicCordisDefinition, DynamicCordisPendingRequest, DynamicCordisPluginState,
    DynamicCordisRegistry, DynamicCordisRunMode,
};
use seekdeep_llm::SessionId;

fn pending(
    session: &SessionId,
    plugin: &CordisDynamicPluginId,
    package: &CordisDynamicPackageId,
    run: &CordisDynamicPluginRunId,
) -> DynamicCordisPendingRequest {
    DynamicCordisPendingRequest {
        agent_id: session.clone(),
        plugin_id: plugin.clone(),
        package_id: package.clone(),
        plugin_run_id: run.clone(),
        mode: DynamicCordisRunMode::Run,
        requires_approval: true,
    }
}

#[test]
fn monotonic_ids_never_reuse_deleted_plugin_or_prior_version_suffixes() {
    let registry = DynamicCordisRegistry::new();
    let session = SessionId::new("session-a");
    let first = registry.mint_plugin_id("clock");
    assert_eq!(first.as_str(), "clock-1");
    registry.add(DynamicCordisPluginState::new(
        first.clone(),
        session.clone(),
    ));
    assert!(registry.delete(&first));
    assert_eq!(registry.mint_plugin_id("clock").as_str(), "clock-2");
    assert_eq!(registry.mint_package_id().as_str(), "pkg-1");
    assert_eq!(registry.mint_package_id().as_str(), "pkg-2");
    assert_eq!(registry.mint_plugin_run_id().as_str(), "run-1");
    assert_eq!(registry.mint_approval_request_id().as_str(), "approval-1");
}

#[test]
fn registry_preserves_plugin_package_and_session_creation_order() {
    let registry = DynamicCordisRegistry::new();
    let session_a = SessionId::new("session-a");
    let session_b = SessionId::new("session-b");
    let clock = registry.mint_plugin_id("clock");
    let panel = registry.mint_plugin_id("panel");
    registry.add(DynamicCordisPluginState::new(
        clock.clone(),
        session_a.clone(),
    ));
    registry.add(DynamicCordisPluginState::new(
        panel.clone(),
        session_b.clone(),
    ));
    let clock_state = registry.get(&clock).unwrap();
    for name in ["v1", "v2"] {
        let package_id = registry.mint_package_id();
        clock_state.lock().packages.insert(
            package_id.clone(),
            DynamicCordisDefinition {
                package_id,
                name: name.to_owned(),
                purpose: "show time".to_owned(),
                host_code: Some("return { apply() {} }".to_owned()),
                client_code: None,
            },
        );
    }
    assert_eq!(
        registry
            .all()
            .iter()
            .map(|plugin| plugin.lock().plugin_id.clone())
            .collect::<Vec<_>>(),
        [clock.clone(), panel]
    );
    assert_eq!(
        registry
            .of_session(&session_a)
            .iter()
            .map(|plugin| plugin.lock().plugin_id.clone())
            .collect::<Vec<_>>(),
        [clock]
    );
    assert_eq!(
        clock_state
            .lock()
            .packages
            .values()
            .map(|package| package.name.as_str())
            .collect::<Vec<_>>(),
        ["v1", "v2"]
    );
}

#[test]
fn pending_request_claim_is_first_answer_wins_and_plugin_addressable() {
    let registry = DynamicCordisRegistry::new();
    let session = SessionId::new("session-a");
    let plugin = CordisDynamicPluginId::new("panel-1");
    let package = CordisDynamicPackageId::new("pkg-1");
    let run = CordisDynamicPluginRunId::new("run-1");
    let approval = ApprovalRequestId::new("approval-1");
    let request = pending(&session, &plugin, &package, &run);
    registry.arm_request(approval.clone(), request.clone());
    assert_eq!(
        registry.pending_request_for(&plugin),
        Some(approval.clone())
    );
    assert_eq!(registry.peek_request(&approval), Some(request.clone()));
    assert_eq!(registry.claim_request(&approval), Some(request));
    assert_eq!(registry.claim_request(&approval), None);
    assert_eq!(registry.pending_request_for(&plugin), None);

    registry.arm_request(approval.clone(), pending(&session, &plugin, &package, &run));
    registry.disarm_request(&approval);
    assert_eq!(registry.peek_request(&approval), None);
}
