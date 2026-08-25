//! Native CLI protocol, provider registration, cancellation, and quiescence parity.

#![cfg(unix)]

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionHeader, SessionId};
use seekdeep_llm::{AbortSignal, ContentBlock};
use seekdeep_scope::ScopeKey;
use seekdeep_subagent::{SubagentRuntime, SubagentStartRequest, SubagentStopReason};
use seekdeep_subagent_claude_code::{
    CLAUDE_STREAM_ARGS, ClaudeCodeRunSpec, Config, DEFAULT_DISPOSE_GRACE_MS, INJECT, NAME,
    WINDOWS_BATCH_EXECUTABLE_ENV, apply, claude_spawn_spec, plugin, prompt_frame,
    start_claude_code_run, successful_result, text_task,
};
use seekdeep_subprocess::SUBPROCESS;
use seekdeep_subprocess_local::LocalSubprocessRuntime;
use serde_json::json;

const FIXTURE: &str = env!("CARGO_BIN_EXE_seekdeep-claude-code-fixture");

struct Harness {
    context: Context,
    runtime: Arc<LocalSubprocessRuntime>,
    subprocess: Arc<seekdeep_subprocess::SubprocessService>,
    workspace: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let context = Context::new();
        let runtime = LocalSubprocessRuntime::install(&context).unwrap();
        let subprocess = context.get(SUBPROCESS).unwrap();
        Self {
            context,
            runtime,
            subprocess,
            workspace: tempfile::tempdir().unwrap(),
        }
    }

    fn request(&self, signal: AbortSignal) -> SubagentStartRequest {
        SubagentStartRequest {
            label: Some("fixture".to_owned()),
            prompt: vec![ContentBlock::Text {
                text: "do the task".to_owned(),
            }],
            parent: agent(
                &self.context,
                Some(&self.workspace.path().to_string_lossy()),
                "parent",
            ),
            signal,
            agent_options: None,
            output_schema: None,
            max_depth: None,
            tool_filter: None,
            persona: None,
        }
    }

    fn spec(&self, mode: &str) -> ClaudeCodeRunSpec {
        ClaudeCodeRunSpec {
            cwd: self.workspace.path().to_string_lossy().into_owned(),
            executable: FIXTURE.to_owned(),
            env: BTreeMap::from([("SEEKDEEP_CLAUDE_FIXTURE_MODE".to_owned(), mode.to_owned())]),
            dispose_grace_ms: 200.0,
            subprocess: Arc::clone(&self.subprocess),
            on_error: None,
        }
    }

    async fn quiescent(&self) {
        for _ in 0..300 {
            if self.runtime.live_process_count() == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(self.runtime.live_process_count(), 0);
    }
}

fn agent(context: &Context, cwd: Option<&str>, id: &str) -> Arc<Agent> {
    let id = SessionId::new(id);
    let mut header = SessionHeader::new(id.clone());
    header.cwd = cwd.map(str::to_owned);
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

fn result_text(result: &seekdeep_subagent::SubagentResult) -> String {
    result
        .output
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
#[allow(clippy::too_many_lines)]
fn pure_task_result_spawn_and_config_contracts_are_exact() {
    assert_eq!(
        text_task(&[
            ContentBlock::Text { text: "one".into() },
            ContentBlock::Text { text: "two".into() }
        ])
        .unwrap(),
        "onetwo"
    );
    assert!(
        text_task(&[])
            .unwrap_err()
            .to_string()
            .contains("only text blocks")
    );
    assert!(
        text_task(&[ContentBlock::Reasoning { text: "x".into() }])
            .unwrap_err()
            .to_string()
            .contains("only text blocks")
    );
    assert!(
        text_task(&[ContentBlock::Text {
            text: " \n ".into()
        }])
        .unwrap_err()
        .to_string()
        .contains("must not be empty")
    );
    assert_eq!(
        successful_result(&json!({
            "type":"result","subtype":"success","is_error":false,"result":"answer"
        }))
        .unwrap(),
        "answer"
    );
    for invalid in [
        json!({"type":"result","subtype":"success","is_error":true,"result":"x"}),
        json!({"type":"result","subtype":"success","is_error":false,"result":" "}),
        json!({"type":"result","subtype":"error_during_execution","is_error":true,"errors":["one","two"]}),
    ] {
        assert!(successful_result(&invalid).is_err());
    }
    assert_eq!(
        prompt_frame("exact"),
        json!({
            "type":"user","session_id":"",
            "message":{"role":"user","content":[{"type":"text","text":"exact"}]},
            "parent_tool_use_id":null
        })
    );
    let signal = AbortSignal::default();
    let spec = claude_spawn_spec(
        "/native/claude",
        "/workspace",
        &BTreeMap::from([("ANTHROPIC_API_KEY".to_owned(), "fake".to_owned())]),
        321.0,
        signal,
        "other",
    )
    .unwrap();
    assert_eq!(spec.argv[0], "/native/claude");
    assert_eq!(
        spec.argv[1..]
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        CLAUDE_STREAM_ARGS
    );
    assert_eq!(spec.cwd.to_string_lossy(), "/workspace");
    assert_eq!(spec.grace_ms.to_bits(), 321.0_f64.to_bits());
    assert_eq!(
        spec.env.as_ref().unwrap()["ANTHROPIC_API_KEY"],
        Some("fake".to_owned())
    );
    assert_eq!(
        spec.env.as_ref().unwrap()["CLAUDE_CODE_ENTRYPOINT"],
        Some("sdk-ts".to_owned())
    );
    assert_eq!(
        spec.env.as_ref().unwrap()["CLAUDE_AGENT_SDK_VERSION"],
        Some("0.3.220".to_owned())
    );
    assert_eq!(spec.env.as_ref().unwrap()["NODE_OPTIONS"], None);
    let windows = claude_spawn_spec(
        r"C:\Program Files\Claude\claude.cmd",
        r"C:\workspace",
        &BTreeMap::new(),
        7.0,
        AbortSignal::default(),
        "win32",
    )
    .unwrap();
    assert_eq!(
        &windows.argv[..6],
        [
            "cmd.exe",
            "/d",
            "/v:off",
            "/s",
            "/c",
            "%SEEKDEEP_CLAUDE_CODE_EXECUTABLE%"
        ]
    );
    assert_eq!(
        windows.env.as_ref().unwrap()[WINDOWS_BATCH_EXECUTABLE_ENV],
        Some(r#""C:\Program Files\Claude\claude.cmd""#.to_owned())
    );
    assert_eq!(NAME, "subagent-claude-code");
    assert_eq!(INJECT, ["subagents", "subprocess"]);
    assert_eq!(
        Config::default().dispose_grace_ms.to_bits(),
        DEFAULT_DISPOSE_GRACE_MS.to_bits()
    );
}

#[tokio::test]
async fn real_fixture_receives_exact_task_flags_workspace_environment_and_latest_success() {
    let harness = Harness::new();
    let record = harness.workspace.path().join("record.json");
    let mut spec = harness.spec("two-success");
    spec.env.insert(
        "SEEKDEEP_CLAUDE_FIXTURE_RECORD".to_owned(),
        record.to_string_lossy().into_owned(),
    );
    spec.env.insert(
        "ANTHROPIC_API_KEY".to_owned(),
        "explicit-fake-key".to_owned(),
    );
    let first = start_claude_code_run(harness.request(AbortSignal::default()), spec)
        .await
        .unwrap();
    let outcome = first.result().await.unwrap();
    assert_eq!(outcome.stop_reason, SubagentStopReason::Completed);
    assert_eq!(result_text(&outcome), "latest");
    first.dispose().await.unwrap();
    first.dispose().await.unwrap();
    harness.quiescent().await;
    let recorded: serde_json::Value =
        serde_json::from_slice(&std::fs::read(record).unwrap()).unwrap();
    assert_eq!(recorded["args"], json!(CLAUDE_STREAM_ARGS));
    assert_eq!(
        recorded["cwd"],
        json!(
            std::fs::canonicalize(harness.workspace.path())
                .unwrap()
                .to_string_lossy()
        )
    );
    assert_eq!(recorded["apiKey"], json!("explicit-fake-key"));
    assert_eq!(recorded["input"], prompt_frame("do the task"));

    let second = start_claude_code_run(
        harness.request(AbortSignal::default()),
        harness.spec("success"),
    )
    .await
    .unwrap();
    assert_ne!(first.id(), second.id());
    second.dispose().await.unwrap();
    harness.quiescent().await;
    harness.context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn protocol_and_process_failures_flatten_to_error_and_notify_once() {
    for mode in [
        "error-result",
        "invalid-success",
        "missing-result",
        "success-then-error",
        "exit-error",
    ] {
        let harness = Harness::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&seen);
        let mut spec = harness.spec(mode);
        spec.on_error = Some(Arc::new(move |error, reason| {
            observed.lock().unwrap().push((error.to_string(), reason));
            panic!("observer failure is contained");
        }));
        let run = start_claude_code_run(harness.request(AbortSignal::default()), spec)
            .await
            .unwrap();
        let outcome = run.result().await.unwrap();
        assert_eq!(outcome.stop_reason, SubagentStopReason::Error, "{mode}");
        assert!(outcome.output.is_empty());
        assert_eq!(seen.lock().unwrap().len(), 1);
        run.dispose().await.unwrap();
        harness.quiescent().await;
        harness.context.fiber().dispose().await.unwrap();
    }

    let harness = Harness::new();
    let run = start_claude_code_run(
        harness.request(AbortSignal::default()),
        harness.spec("malformed-after-success"),
    )
    .await
    .unwrap();
    let outcome = run.result().await.unwrap();
    assert_eq!(outcome.stop_reason, SubagentStopReason::Completed);
    assert_eq!(result_text(&outcome), "must be discarded");
    run.dispose().await.unwrap();
    harness.quiescent().await;
    harness.context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn cancellation_and_prepublication_failures_reap_the_process_tree() {
    let harness = Harness::new();
    let ready = harness.workspace.path().join("ready");
    let signal = AbortSignal::default();
    let mut spec = harness.spec("hold");
    spec.env.insert(
        "SEEKDEEP_CLAUDE_FIXTURE_READY".to_owned(),
        ready.to_string_lossy().into_owned(),
    );
    let run = start_claude_code_run(harness.request(signal.clone()), spec)
        .await
        .unwrap();
    for _ in 0..300 {
        if ready.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(ready.exists());
    signal.abort_with_reason(json!("cancel"));
    assert_eq!(
        run.result().await.unwrap().stop_reason,
        SubagentStopReason::Aborted
    );
    run.dispose().await.unwrap();
    harness.quiescent().await;

    let sentinel = harness.workspace.path().join("spawned");
    let preabort = AbortSignal::default();
    preabort.abort();
    let mut spec = harness.spec("success");
    spec.env.insert(
        "SEEKDEEP_CLAUDE_FIXTURE_SPAWNED".to_owned(),
        sentinel.to_string_lossy().into_owned(),
    );
    let error = start_claude_code_run(harness.request(preabort), spec)
        .await
        .err()
        .expect("preabort must reject");
    assert!(error.to_string().contains("aborted before SDK startup"));
    assert!(!sentinel.exists());

    let missing = ClaudeCodeRunSpec {
        executable: harness
            .workspace
            .path()
            .join("missing-claude")
            .to_string_lossy()
            .into_owned(),
        ..harness.spec("success")
    };
    assert!(
        start_claude_code_run(harness.request(AbortSignal::default()), missing)
            .await
            .is_err()
    );
    harness.quiescent().await;
    harness.context.fiber().dispose().await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn plugin_registers_validates_unwinds_and_uses_parent_workspace() {
    let context = Context::new();
    let subagents = SubagentRuntime::install(&context).unwrap();
    let runtime = LocalSubprocessRuntime::install(&context).unwrap();
    let bin = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(FIXTURE, bin.path().join("claude")).unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let fiber = context
        .plugin(
            plugin(),
            json!({
                "env":{
                    "PATH":bin.path().to_string_lossy(),
                    "SEEKDEEP_CLAUDE_FIXTURE_MODE":"success",
                    "SEEKDEEP_CLAUDE_FIXTURE_ANSWER":"provider answer"
                },
                "disposeGraceMs":200
            }),
        )
        .unwrap();
    fiber.await_settled().await.unwrap();
    let provider = subagents.get_provider("claude-code").unwrap();
    assert_eq!(
        provider.capabilities(),
        &seekdeep_subagent::no_start_capabilities()
    );
    assert!(!provider.inherits_parent_context());
    let run = subagents
        .start(
            "claude-code",
            SubagentStartRequest {
                label: None,
                prompt: vec![ContentBlock::Text {
                    text: "provider".into(),
                }],
                parent: agent(
                    &context,
                    Some(&workspace.path().to_string_lossy()),
                    "provider-parent",
                ),
                signal: AbortSignal::default(),
                agent_options: None,
                output_schema: None,
                max_depth: None,
                tool_filter: None,
                persona: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(result_text(&run.result().await.unwrap()), "provider answer");
    run.dispose().await.unwrap();
    for _ in 0..100 {
        if runtime.live_process_count() == 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(runtime.live_process_count(), 0);
    fiber.dispose().await.unwrap();
    assert!(subagents.get_provider("claude-code").is_none());

    let id = SessionId::new("missing-cwd");
    let session = Session::create(&id, None, None).unwrap();
    let inbox = Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
    let parent = Arc::new(Agent::new(
        id,
        AgentOptions::default(),
        session,
        inbox,
        context.clone(),
        ScopeKey::new(),
    ));
    apply(
        &context,
        Config {
            env: BTreeMap::from([("PATH".to_owned(), bin.path().to_string_lossy().into_owned())]),
            dispose_grace_ms: 200.0,
        },
    )
    .unwrap();
    let error = subagents
        .start(
            "claude-code",
            SubagentStartRequest {
                label: None,
                prompt: vec![ContentBlock::Text { text: "x".into() }],
                parent,
                signal: AbortSignal::default(),
                agent_options: None,
                output_schema: None,
                max_depth: None,
                tool_filter: None,
                persona: None,
            },
        )
        .await
        .err()
        .expect("missing cwd must reject");
    assert!(error.to_string().contains("no working directory"));
    context.fiber().dispose().await.unwrap();

    for invalid in [
        0.0,
        -1.0,
        f64::NAN,
        f64::INFINITY,
        seekdeep_util::timeout::MAX_TIMER_DELAY_MS + 1.0,
    ] {
        let context = Context::new();
        SubagentRuntime::install(&context).unwrap();
        LocalSubprocessRuntime::install(&context).unwrap();
        assert!(
            apply(
                &context,
                Config {
                    dispose_grace_ms: invalid,
                    ..Config::default()
                }
            )
            .is_err()
        );
    }
}
