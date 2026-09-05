//! Deployment, session, path, prompt, and lifecycle policy parity.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use seekdeep_core::session::{Session, SessionHeader, SessionId};
use seekdeep_sandbox::{SANDBOX_MODES, SandboxMode};
use seekdeep_sandbox_policy::{
    SANDBOX_POLICY, SandboxPolicyConfig, SandboxPolicyRequest, effective_sandbox_mode, install,
    plugin, render_policy_context, set_sandbox_mode,
};
use seekdeep_system_prompt::{AssembleContext, SYSTEM_PROMPT, SystemPromptConfig};
use tempfile::tempdir;

fn session(id: &str, cwd: Option<&Path>) -> Arc<Session> {
    let id = SessionId::new(id);
    let mut header = SessionHeader::new(id.clone());
    header.created_at = 0;
    header.cwd = cwd.map(|path| path.to_str().expect("Unicode test path").to_owned());
    Session::create(&id, None, Some(header)).expect("session")
}

fn config(mode: SandboxMode, root: impl Into<PathBuf>) -> SandboxPolicyConfig {
    SandboxPolicyConfig {
        mode,
        workspace_root: Some(root.into()),
    }
}

#[test]
fn config_defaults_and_closed_deserialization_match_source() {
    let plugin = plugin();
    assert_eq!(plugin.name(), "sandbox-policy");
    assert!(plugin.inject().is_empty());
    let config: SandboxPolicyConfig = serde_json::from_value(serde_json::json!({})).unwrap();
    assert_eq!(config.mode, SandboxMode::ReadOnly);
    assert_eq!(config.workspace_root, None);
    let configured: SandboxPolicyConfig = serde_json::from_value(serde_json::json!({
        "mode": "workspace-write",
        "workspaceRoot": "/fallback"
    }))
    .unwrap();
    assert_eq!(configured.mode, SandboxMode::WorkspaceWrite);
    assert_eq!(configured.workspace_root, Some(PathBuf::from("/fallback")));
    assert!(
        serde_json::from_value::<SandboxPolicyConfig>(serde_json::json!({"mode": "yolo"})).is_err()
    );
    assert!(
        serde_json::from_value::<SandboxPolicyConfig>(serde_json::json!({"extra": true})).is_err()
    );
}

#[tokio::test]
async fn resolution_precedence_keeps_mode_root_and_typed_session_identity_together() {
    let context = seekdeep_cordis::Context::new();
    let installation = install(
        &context,
        config(SandboxMode::WorkspaceWrite, "/fallback/../fallback"),
    )
    .unwrap();
    let first = session("sess-first", Some(Path::new("/projects/first")));
    let second = session("sess-second", Some(Path::new("/projects/second")));
    set_sandbox_mode(&second, SandboxMode::ReadOnly).unwrap();

    let agentless = installation
        .resolve(SandboxPolicyRequest::default())
        .unwrap();
    assert_eq!(agentless.mode, SandboxMode::WorkspaceWrite);
    assert_eq!(agentless.workspace_root, PathBuf::from("/fallback"));
    assert_eq!(agentless.session_id, None);

    let first_policy = installation
        .resolve(SandboxPolicyRequest {
            session: Some(&first),
            mode: None,
        })
        .unwrap();
    assert_eq!(first_policy.mode, SandboxMode::WorkspaceWrite);
    assert_eq!(
        first_policy.workspace_root,
        PathBuf::from("/projects/first")
    );
    assert_eq!(
        first_policy.session_id.as_ref().map(SessionId::as_str),
        Some("sess-first")
    );

    let second_policy = installation
        .resolve(SandboxPolicyRequest {
            session: Some(&second),
            mode: None,
        })
        .unwrap();
    assert_eq!(second_policy.mode, SandboxMode::ReadOnly);
    assert_eq!(
        installation.override_of(&second),
        Some(SandboxMode::ReadOnly)
    );
    let approved = installation
        .resolve(SandboxPolicyRequest {
            session: Some(&second),
            mode: Some(SandboxMode::DangerFullAccess),
        })
        .unwrap();
    assert_eq!(approved.mode, SandboxMode::DangerFullAccess);
    assert_eq!(approved.workspace_root, PathBuf::from("/projects/second"));
    assert_eq!(installation.workspace_root, PathBuf::from("/fallback"));

    installation.dispose().await.unwrap();
    assert!(context.get(SANDBOX_POLICY).is_none());
}

#[cfg(unix)]
#[test]
fn workspace_resolution_preserves_physical_symlink_parent_semantics() {
    use std::os::unix::fs::symlink;

    let root = tempdir().unwrap();
    let lexical = root.path().join("lexical");
    let physical = root.path().join("physical");
    let child = physical.join("child");
    std::fs::create_dir(&lexical).unwrap();
    std::fs::create_dir_all(&child).unwrap();
    let link = lexical.join("link");
    symlink(&child, &link).unwrap();
    let cwd = link.join("..");
    let service = seekdeep_sandbox_policy::SandboxPolicyService::new(config(
        SandboxMode::WorkspaceWrite,
        "/fallback",
    ))
    .unwrap();
    let active = session("sess-symlink-parent", Some(&cwd));
    assert_eq!(
        service
            .resolve(SandboxPolicyRequest {
                session: Some(&active),
                mode: None
            })
            .unwrap()
            .workspace_root,
        std::fs::canonicalize(&physical).unwrap()
    );
}

#[test]
fn durable_mode_fold_and_write_path_are_last_event_wins_and_log_only() {
    assert_eq!(
        SANDBOX_MODES,
        [
            SandboxMode::ReadOnly,
            SandboxMode::WorkspaceWrite,
            SandboxMode::DangerFullAccess
        ]
    );
    let active = session("sess-fold", None);
    assert_eq!(effective_sandbox_mode(&active.events()), None);
    set_sandbox_mode(&active, SandboxMode::WorkspaceWrite).unwrap();
    set_sandbox_mode(&active, SandboxMode::ReadOnly).unwrap();
    assert_eq!(
        effective_sandbox_mode(&active.events()),
        Some(SandboxMode::ReadOnly)
    );
    let events = active.events();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event_type, "sandbox/mode");
    assert_eq!(
        events[0].data,
        serde_json::json!({"mode": "workspace-write"})
    );
    assert!(events[0].surface_op.is_none());

    let resumed =
        Session::create(active.id(), Some(events), Some(active.header().clone())).unwrap();
    assert_eq!(
        effective_sandbox_mode(&resumed.events()),
        Some(SandboxMode::ReadOnly)
    );
}

#[test]
fn malformed_durable_mode_fails_resolution_instead_of_silently_using_a_default() {
    let id = SessionId::new("sess-malformed-mode");
    let malformed = seekdeep_core::session::SessionEvent {
        event_type: "sandbox/mode".into(),
        seq: 0,
        time: 0,
        data: serde_json::json!({"mode": "host-root"}),
        source_event_seqs: None,
        surface_op: None,
        ignorable: None,
    };
    let active = Session::create(&id, Some(vec![malformed]), None).unwrap();
    let service =
        seekdeep_sandbox_policy::SandboxPolicyService::new(SandboxPolicyConfig::default()).unwrap();
    let error = service
        .resolve(SandboxPolicyRequest {
            session: Some(&active),
            mode: None,
        })
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "sandbox/mode carries unknown mode \"host-root\""
    );
}

#[test]
fn policy_context_uses_renamed_product_identity_and_capability_neutral_text() {
    let root = PathBuf::from("/projects/current");
    let make = |mode| seekdeep_sandbox::SandboxExecutionPolicy {
        mode,
        workspace_root: root.clone(),
        session_id: None,
    };
    assert_eq!(
        render_policy_context(&make(SandboxMode::ReadOnly)).unwrap(),
        "Current SeekDeep file policy: read-only. Any available operation enforced by the SeekDeep file sandbox cannot modify files in the standing mode. Do not refuse a required modification from this policy alone: try an available tool normally and follow any denial and escalation guidance it returns."
    );
    assert_eq!(
        render_policy_context(&make(SandboxMode::WorkspaceWrite)).unwrap(),
        "Current SeekDeep file policy: workspace-write. Any available operation enforced by the SeekDeep file sandbox may modify files under the session workspace: \"/projects/current\". Some platform temporary areas may also be writable."
    );
    assert_eq!(
        render_policy_context(&make(SandboxMode::DangerFullAccess)).unwrap(),
        "Current SeekDeep file policy: danger-full-access. The SeekDeep file sandbox does not restrict file modifications by available operations."
    );
}

#[tokio::test]
async fn prompt_contribution_tracks_switches_and_mount_order_and_disposes_with_policy() {
    let context = seekdeep_cordis::Context::new();
    let policy = install(&context, SandboxPolicyConfig::default()).unwrap();
    assert!(context.get(SYSTEM_PROMPT).is_none());
    let prompt = seekdeep_system_prompt::install(&context, SystemPromptConfig::default()).unwrap();
    let active = session("sess-prompt", Some(Path::new("/projects/current")));

    let assemble = || {
        prompt.assemble(AssembleContext {
            agent_session: Some(active.clone()),
            ..AssembleContext::default()
        })
    };
    let first = assemble().await.unwrap();
    let section = first
        .contexts
        .iter()
        .find(|section| section.name == "sandbox:policy")
        .unwrap();
    assert!(section.text.contains("read-only"));
    set_sandbox_mode(&active, SandboxMode::DangerFullAccess).unwrap();
    let danger = assemble().await.unwrap();
    assert!(
        danger
            .contexts
            .iter()
            .find(|section| section.name == "sandbox:policy")
            .unwrap()
            .text
            .contains("danger-full-access")
    );

    let agentless = prompt.assemble(AssembleContext::default()).await.unwrap();
    assert_eq!(
        agentless
            .contexts
            .iter()
            .find(|section| section.name == "sandbox:policy")
            .unwrap()
            .text,
        ""
    );

    policy.dispose().await.unwrap();
    let disposed = prompt
        .assemble(AssembleContext {
            agent_session: Some(active),
            ..AssembleContext::default()
        })
        .await
        .unwrap();
    assert!(
        disposed
            .contexts
            .iter()
            .all(|section| section.name != "sandbox:policy")
    );
}
