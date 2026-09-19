//! Assembled model-facing filesystem tool parity over real providers.

use std::sync::Arc;

use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{AppendOptions, Session, SessionHeader, SessionId, SurfaceOp},
    session_store::SessionStore,
};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock, Message};
use seekdeep_lossless_json::{JsonNumber, JsonString, JsonValue};
use seekdeep_sandbox::SandboxMode;
use seekdeep_sandbox_policy::{SandboxPolicyConfig, SandboxPolicyService};
use seekdeep_scope::ScopeKey;
use seekdeep_session_persistence::SessionPersistence;
use seekdeep_session_persistence_jsonl::{JsonlConfig, JsonlSessionPersistence};
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
    harness_with_config(sandboxed, &seekdeep_tool_fs::Config::default())
}

fn harness_with_config(sandboxed: bool, config: &seekdeep_tool_fs::Config) -> Harness {
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
    seekdeep_tool_fs::apply(&context, config).unwrap();
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
            ContentBlock::Text { text } => Some(text.as_str().expect("fixture uses scalar text")),
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
            &json!({"file_path":"diff.txt", "content":"new\n"}).into(),
            &result
        ),
        Some(ToolResultView::Diff(DiffResultView { .. }))
    ));
}

#[tokio::test]
async fn read_preserves_split_surrogate_through_output_card_and_jsonl_replay() {
    for threshold in [1, u64::MAX] {
        let harness = harness_with_config(
            false,
            &seekdeep_tool_fs::Config {
                read_stream_min_size: Some(JsonNumber::from(threshold)),
                ..Default::default()
            },
        );
        let path = harness.root.path().join("unicode.rs");
        std::fs::write(&path, format!("{}😀x\nnext", "a".repeat(1999))).unwrap();
        let arguments = JsonValue::from(json!({"file_path":"unicode.rs"}));
        let result = call(&harness, "read", json!({"file_path":"unicode.rs"})).await;
        assert!(!result.is_error(), "{:?}", result.error());
        let mut expected = JsonString::from("a".repeat(1999));
        expected.push_utf16(&[0xd83d]);
        expected.push_str("... (line truncated to 2000 chars)");
        let line = result
            .json_value()
            .unwrap()
            .get("lines")
            .unwrap()
            .array_items()
            .unwrap()[0]
            .get("text")
            .unwrap()
            .deserialize::<JsonString>()
            .unwrap();
        assert_eq!(
            line, expected,
            "canonical value at streaming threshold {threshold}"
        );
        let meta_line = result
            .meta()
            .unwrap()
            .get("lines")
            .unwrap()
            .array_items()
            .unwrap()[0]
            .get("text")
            .unwrap()
            .deserialize::<JsonString>()
            .unwrap();
        assert_eq!(meta_line, expected, "persisted presentation metadata");
        let wire_result = ToolResult {
            content: result.content().to_vec(),
            is_error: false,
            meta: result.meta().cloned(),
        };
        let definition = harness.tools.get("read", None).unwrap();
        let presenter = definition.present_result.as_ref().unwrap();
        let live_card = presenter(&arguments, &wire_result).expect("structured read card");
        let ToolResultView::Read(card) = &live_card else {
            panic!("expected read card");
        };
        assert_eq!(
            JsonValue::from_serialize(&card.lines[0].text).unwrap(),
            JsonValue::from_serialize(&expected).unwrap()
        );
        let body = card.content.as_ref().unwrap();
        let [ContentBlock::Text { text: body }] = body.as_slice() else {
            panic!("expected one text body");
        };
        assert!(!body.starts_with("<path>"));
        assert!(
            body.utf16_units()
                .windows(expected.len_utf16())
                .any(|units| units == expected.utf16_units())
        );
        let encoded_card = JsonValue::from_serialize(&live_card).unwrap();
        assert_eq!(
            encoded_card.deserialize::<ToolResultView>().unwrap(),
            live_card
        );
        let client_block = seekdeep_client_ui_tool::ToolCallBlock::Settled {
            call_id: "read-call".to_owned(),
            call: None,
            call_view: None,
            result_view: Some(encoded_card),
            content: Vec::new(),
            is_error: false,
            error: None,
        };
        let client_card = seekdeep_client_ui_tool::read_card_model(&client_block, None)
            .expect("client read card");
        assert_eq!(client_card.lines[0].text, expected);

        let replayed_result = replay_read_result(&harness, &result).await;
        assert_eq!(presenter(&arguments, &replayed_result), Some(live_card));
        harness.context.fiber().dispose().await.unwrap();
    }
}

async fn replay_read_result(harness: &Harness, result: &ToolExecutionResult) -> ToolResult {
    let message = Message::tool_result(&CallId::new("read-call"), result.content().to_vec(), false);
    let data = JsonValue::object([
        ("message", JsonValue::from_serialize(&message).unwrap()),
        ("meta", result.meta().unwrap().clone()),
    ]);
    let session = harness.agent.session();
    session
        .append_json(
            "tool/result",
            data,
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..Default::default()
            },
        )
        .unwrap();
    let persistence_context = Context::new();
    let config = JsonlConfig::new(harness.root.path().join("sessions"));
    let writer = JsonlSessionPersistence::new(
        SessionStore::install(&persistence_context).unwrap(),
        config.clone(),
    )
    .unwrap();
    writer.create(session.header()).await.unwrap();
    writer
        .append(session.id(), &session.events())
        .await
        .unwrap();
    let reader_context = Context::new();
    let reader =
        JsonlSessionPersistence::new(SessionStore::install(&reader_context).unwrap(), config)
            .unwrap();
    let restored = reader.load(session.id()).await.unwrap();
    assert_eq!(restored.events, session.events());
    let replayed =
        Session::create(session.id(), Some(restored.events), Some(restored.meta)).unwrap();
    assert_eq!(replayed.derive_messages(), [message]);
    let durable = replayed
        .events()
        .into_iter()
        .find(|event| event.event_type == "tool/result")
        .unwrap();
    let message = durable
        .data
        .get("message")
        .unwrap()
        .deserialize::<Message>()
        .unwrap();
    let [
        ContentBlock::ToolResult {
            tool_call_id,
            content,
            is_error,
        },
    ] = message.content()
    else {
        panic!("replayed message must contain the correlated tool result");
    };
    assert_eq!(tool_call_id.as_str(), "read-call");
    assert_eq!(*is_error, Some(false));
    let replayed_result = ToolResult {
        content: content.clone(),
        is_error: false,
        meta: Some(durable.data.get("meta").unwrap().to_owned()),
    };
    reader_context.fiber().dispose().await.unwrap();
    persistence_context.fiber().dispose().await.unwrap();
    replayed_result
}
