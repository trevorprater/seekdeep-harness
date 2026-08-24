//! Assembled model-facing filesystem tool parity over real providers.

use std::sync::Arc;

use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionHeader, SessionId};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock};
use seekdeep_sandbox::SandboxMode;
use seekdeep_sandbox_policy::{SandboxPolicyConfig, SandboxPolicyService};
use seekdeep_scope::ScopeKey;
use seekdeep_tools::{
    DiffResultView, ToolExecutionInput, ToolExecutionResult, ToolPresentationMode, ToolResult,
    ToolResultView, ToolRuntime, ToolRuntimeConfig,
};
use serde_json::{Value, json};

struct Harness {
    context: Context,
    tools: Arc<ToolRuntime>,
    agent: Arc<Agent>,
    root: tempfile::TempDir,
}

fn agent(context: &Context, root: &std::path::Path, id: &str) -> Arc<Agent> {
    let id = SessionId::new(id);
    let mut header = SessionHeader::new(id.clone());
    header.cwd = Some(root.to_string_lossy().into_owned());
    let session = Session::create(&id, None, Some(header)).unwrap();
    let inbox = Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
    Arc::new(Agent::new(
        id,
        AgentOptions::default(),
        session,
        inbox,
        context.clone(),
        ScopeKey::new(),
    ))
}

fn harness(sandboxed: bool) -> Harness {
    let root = tempfile::tempdir().unwrap();
    let context = Context::new();
    let prompt = seekdeep_system_prompt::install(
        &context,
        seekdeep_system_prompt::SystemPromptConfig::default(),
    )
    .unwrap();
    let tools = seekdeep_tools::install(
        &context,
        &prompt,
        ToolRuntimeConfig {
            mode: ToolPresentationMode::Native,
            ..Default::default()
        },
    )
    .unwrap();
    if sandboxed {
        SandboxPolicyService::new(SandboxPolicyConfig {
            mode: SandboxMode::ReadOnly,
            workspace_root: Some(root.path().to_owned()),
        })
        .unwrap()
        .provide(&context)
        .unwrap();
        seekdeep_fs_sandbox::apply(
            &context,
            seekdeep_fs_local::Config {
                cwd: Some(root.path().to_string_lossy().into_owned()),
                ..Default::default()
            },
        )
        .unwrap();
    } else {
        seekdeep_fs_local::LocalFileSystem::install(
            &context,
            seekdeep_fs_local::Config {
                cwd: Some(root.path().to_string_lossy().into_owned()),
                ..Default::default()
            },
        )
        .unwrap();
    }
    seekdeep_fs_observation_policy::apply(&context).unwrap();
    seekdeep_tool_fs::apply(&context, &seekdeep_tool_fs::Config::default()).unwrap();
    let agent = agent(&context, root.path(), "fs-agent");
    Harness {
        context,
        tools,
        agent,
        root,
    }
}

async fn call(harness: &Harness, name: &str, arguments: Value) -> ToolExecutionResult {
    harness
        .tools
        .execute(
            ToolExecutionInput::new(
                CallId::new(format!("{name}-call")),
                name,
                arguments,
                AbortSignal::default(),
            )
            .with_agent(harness.agent.clone()),
        )
        .await
}

fn text(result: &ToolExecutionResult) -> String {
    result
        .content()
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn write_read_edit_round_trip_uses_session_cwd_observation_and_real_disk() {
    let harness = harness(false);
    let write = call(
        &harness,
        "write",
        json!({"file_path":"note.txt", "content":"alpha\nbeta\n"}),
    )
    .await;
    assert!(!write.is_error(), "{:?}", write.error());
    assert_eq!(
        std::fs::read_to_string(harness.root.path().join("note.txt")).unwrap(),
        "alpha\nbeta\n"
    );
    let read = call(
        &harness,
        "read",
        json!({"file_path":"note.txt", "offset":2, "limit":1}),
    )
    .await;
    assert!(!read.is_error());
    assert!(text(&read).contains("beta"));
    let edit = call(
        &harness,
        "edit",
        json!({"file_path":"note.txt", "old_string":"beta", "new_string":"gamma"}),
    )
    .await;
    assert!(!edit.is_error(), "{:?}", edit.error());
    assert_eq!(
        std::fs::read_to_string(harness.root.path().join("note.txt")).unwrap(),
        "alpha\ngamma\n"
    );
    assert!(write.meta().is_some() && edit.meta().is_some());
}

#[tokio::test]
async fn observation_is_owner_scoped_and_edit_requires_reading_first() {
    let harness = harness(false);
    std::fs::write(harness.root.path().join("shared.txt"), "before").unwrap();
    let other = agent(&harness.context, harness.root.path(), "other-agent");
    let denied = harness
        .tools
        .execute(
            ToolExecutionInput::new(
                CallId::new("edit"),
                "edit",
                json!({"file_path":"shared.txt", "old_string":"before", "new_string":"after"}),
                AbortSignal::default(),
            )
            .with_agent(other.clone()),
        )
        .await;
    assert!(denied.is_error());
    assert_eq!(
        denied
            .error()
            .and_then(|error| error.info.as_ref())
            .map(|info| info.code.as_str()),
        Some("FS_NOT_OBSERVED")
    );
    harness
        .tools
        .execute(
            ToolExecutionInput::new(
                CallId::new("read"),
                "read",
                json!({"file_path":"shared.txt"}),
                AbortSignal::default(),
            )
            .with_agent(other.clone()),
        )
        .await;
    let edited = harness
        .tools
        .execute(
            ToolExecutionInput::new(
                CallId::new("edit-2"),
                "edit",
                json!({"file_path":"shared.txt", "old_string":"before", "new_string":"after"}),
                AbortSignal::default(),
            )
            .with_agent(other),
        )
        .await;
    assert!(!edited.is_error(), "{:?}", edited.error());
}

#[tokio::test]
async fn sandboxed_provider_advertises_escalation_and_renders_real_denial() {
    let harness = harness(true);
    let schema = harness
        .tools
        .schemas(None)
        .into_iter()
        .find(|schema| schema.name == "write")
        .unwrap();
    assert!(
        schema.parameters["properties"]
            .get("sandbox_permissions")
            .is_some()
    );
    let denied = call(
        &harness,
        "write",
        json!({"file_path":"denied.txt", "content":"x"}),
    )
    .await;
    assert!(denied.is_error());
    assert!(text(&denied).contains("file access denied under read-only mode"));
    assert!(!harness.root.path().join("denied.txt").exists());
}

#[tokio::test]
async fn completed_write_and_edit_present_replay_safe_diff_cards() {
    let harness = harness(false);
    std::fs::write(harness.root.path().join("diff.txt"), "old\n").unwrap();
    let _ = call(&harness, "read", json!({"file_path":"diff.txt"})).await;
    let write = call(
        &harness,
        "write",
        json!({"file_path":"diff.txt", "content":"new\n"}),
    )
    .await;
    let definition = harness.tools.get("write", None).unwrap();
    let result = ToolResult {
        content: write.content().to_vec(),
        is_error: false,
        meta: write.meta().cloned(),
    };
    assert!(matches!(
        definition.present_result.as_ref().unwrap()(
            &json!({"file_path":"diff.txt", "content":"new\n"}),
            &result
        ),
        Some(ToolResultView::Diff(DiffResultView { .. }))
    ));
}
