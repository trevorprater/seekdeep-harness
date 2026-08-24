//! Behavioral mirror of the permission-preset service and session-default source suite.

mod support;

use std::{panic::AssertUnwindSafe, time::Duration};

use seekdeep_core::session::AppendOptions;
use seekdeep_permission_presets::{
    CUSTOM_PRESET, PERMISSION_SETTINGS_NAMESPACE, effective_permission_preset,
};
use seekdeep_sandbox::SandboxMode;
use seekdeep_sandbox_policy::set_sandbox_mode;
use seekdeep_settings::settings_namespace;
use seekdeep_user_approval::{ApprovalPolicy, set_approval_policy};
use serde_json::json;

use support::{
    MountOptions, base, create_session, default_config, event_pairs, fresh_session, mount,
    mount_permission,
};

async fn eventually(mut condition: impl FnMut() -> bool, message: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{message}"));
}

#[test]
fn effective_preset_folds_to_the_last_selection_across_unrelated_events() {
    let session = fresh_session("permission-fold");
    assert_eq!(effective_permission_preset(&session.events()), None);
    session
        .append(
            "permission/preset",
            json!({"preset": "danger-full-access"}),
            AppendOptions::default(),
        )
        .unwrap();
    session
        .append(
            "permission/preset",
            json!({"preset": "workspace-write"}),
            AppendOptions::default(),
        )
        .unwrap();
    assert_eq!(
        effective_permission_preset(&session.events()).as_deref(),
        Some("workspace-write")
    );
    set_sandbox_mode(&session, SandboxMode::ReadOnly).unwrap();
    assert_eq!(
        effective_permission_preset(&session.events()).as_deref(),
        Some("workspace-write")
    );
}

#[tokio::test]
async fn table_order_derivation_ties_custom_and_options_match_the_source() {
    let harness = mount(default_config(), MountOptions::default())
        .await
        .unwrap();
    assert_eq!(
        harness.service.names(),
        ["workspace-write", "danger-full-access"]
    );
    assert_eq!(
        harness
            .service
            .resolve("danger-full-access")
            .unwrap()
            .sandbox,
        SandboxMode::DangerFullAccess
    );
    assert_eq!(
        harness
            .service
            .resolve("danger-full-access")
            .unwrap()
            .approval,
        ApprovalPolicy::Never
    );
    assert!(
        harness
            .service
            .resolve("plan")
            .unwrap_err()
            .to_string()
            .contains("unknown preset \"plan\"")
    );

    let session = fresh_session("permission-current");
    assert_eq!(
        harness.service.current(&session.events()),
        "workspace-write"
    );
    harness.service.set(&session, "danger-full-access").unwrap();
    assert_eq!(
        harness.service.current(&session.events()),
        "danger-full-access"
    );
    set_sandbox_mode(&session, SandboxMode::ReadOnly).unwrap();
    assert_eq!(harness.service.current(&session.events()), CUSTOM_PRESET);
    assert!(harness.service.resolve(CUSTOM_PRESET).is_err());

    let option = harness.service.option_of("danger-full-access");
    assert_eq!(option.value, "danger-full-access");
    assert_eq!(option.name, "danger-full-access");
    assert_eq!(
        option.description.as_deref(),
        Some("Full file access without approval prompts.")
    );
    assert_eq!(harness.service.option_of(CUSTOM_PRESET).name, "Custom");
    assert!(
        std::panic::catch_unwind(AssertUnwindSafe(|| { harness.service.option_of("plan") }))
            .is_err()
    );

    let bare = mount(
        json!({"presets": {"plain": {"sandbox": "workspace-write", "approval": "ask"}}}),
        MountOptions::default(),
    )
    .await
    .unwrap();
    let plain = bare.service.option_of("plain");
    assert_eq!(plain.value, "plain");
    assert_eq!(plain.name, "plain");
    assert_eq!(plain.description, None);

    let tied = mount(
        json!({"presets": {
            "workspace-write": {"sandbox": "workspace-write", "approval": "ask"},
            "agentish": {"sandbox": "workspace-write", "approval": "ask"},
            "danger-full-access": {"sandbox": "danger-full-access", "approval": "never"}
        }}),
        MountOptions::default(),
    )
    .await
    .unwrap();
    let tied_session = fresh_session("permission-tie");
    tied.service.set(&tied_session, "agentish").unwrap();
    assert_eq!(tied.service.current(&tied_session.events()), "agentish");
    set_approval_policy(&tied_session, ApprovalPolicy::Never).unwrap();
    set_sandbox_mode(&tied_session, SandboxMode::DangerFullAccess).unwrap();
    assert_eq!(
        tied.service.current(&tied_session.events()),
        "danger-full-access"
    );
}

#[tokio::test]
async fn composition_validation_and_explicit_defaults_fail_or_derive_exactly() {
    let custom_defaults = mount(
        json!({"defaultPreset": "workspace-write"}),
        MountOptions {
            approval_policy: ApprovalPolicy::Never,
            ..MountOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        custom_defaults
            .service
            .current(&fresh_session("custom-defaults").events()),
        CUSTOM_PRESET
    );
    assert_eq!(custom_defaults.service.default_preset(), "workspace-write");

    let unconfined = mount(
        default_config(),
        MountOptions {
            shell_mode: None,
            ..MountOptions::default()
        },
    )
    .await
    .err()
    .unwrap();
    assert!(unconfined.to_string().contains("does not confine"));

    let reserved = mount(
        json!({"presets": {"custom": {"sandbox": "read-only", "approval": "ask"}}}),
        MountOptions::default(),
    )
    .await
    .err()
    .unwrap();
    assert!(
        reserved
            .to_string()
            .contains("reserved for the derived not-a-preset state")
    );

    let missing_default = mount(
        default_config(),
        MountOptions {
            approval_policy: ApprovalPolicy::Never,
            ..MountOptions::default()
        },
    )
    .await
    .err()
    .unwrap();
    assert!(
        missing_default
            .to_string()
            .contains("configure defaultPreset explicitly")
    );

    let typed_ask = mount(default_config(), MountOptions::default())
        .await
        .unwrap();
    let typed_session = fresh_session("typed-ask-default");
    typed_ask
        .service
        .set(&typed_session, "workspace-write")
        .unwrap();
    assert!(typed_session.events().is_empty());
}

#[tokio::test]
async fn set_writes_only_the_changed_preset_and_knob_events() {
    let harness = mount(default_config(), MountOptions::default())
        .await
        .unwrap();
    let session = fresh_session("permission-set");
    harness.service.set(&session, "danger-full-access").unwrap();
    assert_eq!(
        event_pairs(&session),
        [
            (
                "permission/preset".into(),
                json!({"preset": "danger-full-access"})
            ),
            ("sandbox/mode".into(), json!({"mode": "danger-full-access"})),
            ("approval/policy".into(), json!({"policy": "never"})),
        ]
    );

    let noop = fresh_session("permission-noop");
    harness.service.set(&noop, "workspace-write").unwrap();
    assert!(noop.events().is_empty());

    set_sandbox_mode(&session, SandboxMode::ReadOnly).unwrap();
    harness.service.set(&session, "danger-full-access").unwrap();
    assert_eq!(
        event_pairs(&session)[4..],
        [
            (
                "permission/preset".into(),
                json!({"preset": "danger-full-access"})
            ),
            ("sandbox/mode".into(), json!({"mode": "danger-full-access"})),
        ]
    );
}

#[tokio::test]
async fn settings_default_is_snapshotted_per_new_session_and_rejects_unknown_names() {
    let harness = mount(
        default_config(),
        MountOptions {
            with_settings: true,
            ..MountOptions::default()
        },
    )
    .await
    .unwrap();
    let first = create_session(&harness, "permission-first", None);
    assert_eq!(
        event_pairs(&first),
        [
            (
                "permission/preset".into(),
                json!({"preset": "workspace-write"})
            ),
            ("sandbox/mode".into(), json!({"mode": "workspace-write"})),
            ("approval/policy".into(), json!({"policy": "ask"})),
        ]
    );

    let settings = harness.settings.as_ref().unwrap();
    let namespace = settings_namespace(PERMISSION_SETTINGS_NAMESPACE).unwrap();
    settings
        .update(
            &namespace,
            json!({"defaultPreset": "danger-full-access"}),
            None,
        )
        .await
        .unwrap();
    eventually(
        || harness.service.default_preset() == "danger-full-access",
        "permission settings watcher did not publish the new default",
    )
    .await;
    let second = create_session(&harness, "permission-second", None);
    assert_eq!(harness.service.current(&first.events()), "workspace-write");
    assert_eq!(
        harness.service.current(&second.events()),
        "danger-full-access"
    );
    assert_eq!(
        second
            .events()
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        ["permission/preset", "sandbox/mode", "approval/policy"]
    );

    let error = settings
        .update(&namespace, json!({"defaultPreset": "missing"}), None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("defaultPreset"));
    assert_eq!(harness.service.default_preset(), "danger-full-access");
}

#[tokio::test]
async fn seeded_and_partial_sessions_preserve_effective_facts_and_fill_only_missing_ones() {
    let harness = mount(
        default_config(),
        MountOptions {
            with_settings: true,
            ..MountOptions::default()
        },
    )
    .await
    .unwrap();
    let namespace = settings_namespace(PERMISSION_SETTINGS_NAMESPACE).unwrap();
    harness
        .settings
        .as_ref()
        .unwrap()
        .update(
            &namespace,
            json!({"defaultPreset": "danger-full-access"}),
            None,
        )
        .await
        .unwrap();
    eventually(
        || harness.service.default_preset() == "danger-full-access",
        "permission settings watcher did not settle",
    )
    .await;

    let legacy_source = fresh_session("legacy-source");
    legacy_source
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .unwrap();
    legacy_source
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .unwrap();
    let resumed = create_session(&harness, "legacy-resumed", Some(legacy_source.events()));
    assert_eq!(
        harness.service.current(&resumed.events()),
        "workspace-write"
    );
    assert_eq!(
        resumed
            .events()
            .iter()
            .rev()
            .take(3)
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        ["approval/policy", "sandbox/mode", "permission/preset"]
    );

    let empty = create_session(&harness, "empty-resumed", Some(Vec::new()));
    assert_eq!(harness.service.current(&empty.events()), "workspace-write");
    assert_eq!(
        empty
            .events()
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        [
            "session/end-seed",
            "permission/preset",
            "sandbox/mode",
            "approval/policy"
        ]
    );

    let partial_source = fresh_session("partial-source");
    set_sandbox_mode(&partial_source, SandboxMode::WorkspaceWrite).unwrap();
    set_approval_policy(&partial_source, ApprovalPolicy::Ask).unwrap();
    let partial = create_session(&harness, "partial-resumed", Some(partial_source.events()));
    assert_eq!(
        partial.events().last().unwrap().event_type,
        "permission/preset"
    );

    let custom_source = fresh_session("custom-source");
    set_sandbox_mode(&custom_source, SandboxMode::ReadOnly).unwrap();
    set_approval_policy(&custom_source, ApprovalPolicy::Never).unwrap();
    let custom = create_session(&harness, "custom-resumed", Some(custom_source.events()));
    assert_eq!(harness.service.current(&custom.events()), CUSTOM_PRESET);
    assert_eq!(
        custom.events().last().unwrap().event_type,
        "session/end-seed"
    );

    let approval_source = fresh_session("approval-fallback-source");
    set_sandbox_mode(&approval_source, SandboxMode::WorkspaceWrite).unwrap();
    let approval = create_session(
        &harness,
        "approval-fallback-resumed",
        Some(approval_source.events()),
    );
    assert_eq!(
        approval.events().last().unwrap().event_type,
        "approval/policy"
    );
    assert_eq!(
        approval.events().last().unwrap().data,
        json!({"policy": "ask"})
    );
}

#[tokio::test]
async fn late_mount_pins_sessions_that_already_exist() {
    let base = base(MountOptions::default()).await.unwrap();
    let existing = base
        .sessions
        .create(
            &base.context,
            Some(seekdeep_core::session::SessionId::new(
                "permission-existing",
            )),
            seekdeep_core::session_store::CreateSessionOptions::default(),
        )
        .unwrap();
    assert!(existing.events().is_empty());
    let harness = mount_permission(base, default_config()).await.unwrap();
    assert_eq!(
        existing
            .events()
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        ["permission/preset", "sandbox/mode", "approval/policy"]
    );
    assert_eq!(
        harness.service.current(&existing.events()),
        "workspace-write"
    );
}
