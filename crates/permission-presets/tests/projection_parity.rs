//! Behavioral mirror of the permissions projection and `/permission` command source suite.

mod support;

use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_commands::COMMANDS;
use seekdeep_llm::AbortSignal;
use seekdeep_permission_presets::{CUSTOM_PRESET, PermissionSelect};
use seekdeep_sandbox::SandboxMode;
use seekdeep_sandbox_policy::set_sandbox_mode;
use seekdeep_session_projection::SESSION_PROJECTIONS;
use serde_json::{Value, json};

use support::{MountOptions, agent, base, create_session, default_config, mount, mount_permission};

fn permission_value(
    harness: &support::Harness,
    session: &Arc<seekdeep_core::session::Session>,
) -> PermissionSelect {
    let projections = harness.context.get(SESSION_PROJECTIONS).unwrap();
    serde_json::from_value(
        projections
            .snapshot(session)
            .unwrap()
            .values
            .get("permissions")
            .cloned()
            .expect("permissions projection"),
    )
    .unwrap()
}

#[tokio::test]
async fn projection_serves_defaults_tracks_knob_changes_and_appends_custom_only_while_current() {
    let harness = mount(
        default_config(),
        MountOptions {
            with_projections: true,
            ..MountOptions::default()
        },
    )
    .await
    .unwrap();
    let session = create_session(&harness, "permission-projected", None);
    let initial = permission_value(&harness, &session);
    assert_eq!(initial.current_value, "workspace-write");
    assert_eq!(
        initial
            .options
            .iter()
            .map(|option| option.value.as_str())
            .collect::<Vec<_>>(),
        ["workspace-write", "danger-full-access"]
    );

    let changes = Arc::new(Mutex::new(Vec::<(String, Value, u64)>::new()));
    let observed = changes.clone();
    harness
        .context
        .get(SESSION_PROJECTIONS)
        .unwrap()
        .on_changed(
            &harness.context,
            Arc::new(move |_, key, value, seq| {
                observed.lock().push((key.to_owned(), value.clone(), seq));
                Ok(())
            }),
        )
        .unwrap();
    harness.service.set(&session, "danger-full-access").unwrap();
    assert_eq!(changes.lock().len(), 3);
    assert_eq!(changes.lock().last().unwrap().0, "permissions");
    assert_eq!(
        changes.lock().last().unwrap().1["currentValue"],
        "danger-full-access"
    );
    session
        .append(
            "turn/start",
            json!({"turn": 1}),
            seekdeep_core::session::AppendOptions::default(),
        )
        .unwrap();
    assert_eq!(changes.lock().len(), 3);

    set_sandbox_mode(&session, SandboxMode::ReadOnly).unwrap();
    let custom = permission_value(&harness, &session);
    assert_eq!(custom.current_value, CUSTOM_PRESET);
    assert_eq!(custom.options.last().unwrap().value, CUSTOM_PRESET);
    assert_eq!(custom.options.last().unwrap().name, "Custom");
}

#[tokio::test]
async fn projection_key_appears_with_service_and_disappears_on_unload() {
    let base = base(MountOptions {
        with_projections: true,
        ..MountOptions::default()
    })
    .await
    .unwrap();
    let session = base
        .sessions
        .create(
            &base.context,
            Some(seekdeep_core::session::SessionId::new("permission-hmr")),
            seekdeep_core::session_store::CreateSessionOptions::default(),
        )
        .unwrap();
    let projections = base.context.get(SESSION_PROJECTIONS).unwrap();
    assert!(
        !projections
            .snapshot(&session)
            .unwrap()
            .values
            .contains_key("permissions")
    );
    let harness = mount_permission(base, default_config()).await.unwrap();
    assert_eq!(
        permission_value(&harness, &session).current_value,
        "workspace-write"
    );
    harness.plugin.dispose().await.unwrap();
    assert!(
        !projections
            .snapshot(&session)
            .unwrap()
            .values
            .contains_key("permissions")
    );
}

#[tokio::test]
async fn permission_command_switches_through_live_approval_and_records_lifecycle() {
    let harness = mount(
        default_config(),
        MountOptions {
            with_commands: true,
            ..MountOptions::default()
        },
    )
    .await
    .unwrap();
    let session = create_session(&harness, "permission-command-switch", None);
    let (agent, controller) = agent(session.clone());
    let execution = harness
        .context
        .get(COMMANDS)
        .unwrap()
        .execute(
            agent,
            "/permission danger-full-access",
            AbortSignal::default(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(execution.result.kind(), "success");
    assert_eq!(execution.result.text(), Some("preset danger-full-access"));
    assert_eq!(
        harness.service.current(&session.events()),
        "danger-full-access"
    );
    let sent = controller.sent.lock();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].text,
        "The approval policy changed from \"ask\" to \"never\" (changed by the user)."
    );
    assert_eq!(sent[0].target, seekdeep_agent::InboxTarget::NextStep);
    assert!(!sent[0].wakeup);
    let run = session
        .events()
        .into_iter()
        .find(|event| event.event_type == "command/run")
        .unwrap();
    assert_eq!(run.data["name"], "permission");
    assert_eq!(run.data["args"], " danger-full-access");
}

#[tokio::test]
async fn bare_and_unknown_permission_commands_report_without_domain_mutation() {
    let harness = mount(
        default_config(),
        MountOptions {
            with_commands: true,
            ..MountOptions::default()
        },
    )
    .await
    .unwrap();
    let commands = harness.context.get(COMMANDS).unwrap();

    let bare_session = create_session(&harness, "permission-command-bare", None);
    let (bare_agent, _) = agent(bare_session.clone());
    let bare = commands
        .execute(bare_agent, "/permission", AbortSignal::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bare.result.kind(), "success");
    assert_eq!(
        bare.result.text(),
        Some("current preset workspace-write (available: workspace-write, danger-full-access)")
    );
    assert_eq!(
        bare_session
            .events()
            .iter()
            .filter(|event| event.event_type == "permission/preset")
            .count(),
        1
    );

    let unknown_session = create_session(&harness, "permission-command-unknown", None);
    let before = unknown_session
        .events()
        .into_iter()
        .filter(|event| !matches!(event.event_type.as_str(), "command/run" | "command/done"))
        .collect::<Vec<_>>();
    let (unknown_agent, _) = agent(unknown_session.clone());
    let unknown = commands
        .execute(unknown_agent, "/permission yolo", AbortSignal::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unknown.result.kind(), "error");
    assert_eq!(
        unknown.result.text(),
        Some("unknown preset \"yolo\" (available: workspace-write, danger-full-access)")
    );
    assert_eq!(
        unknown_session
            .events()
            .into_iter()
            .filter(|event| !matches!(event.event_type.as_str(), "command/run" | "command/done"))
            .collect::<Vec<_>>(),
        before
    );
}
