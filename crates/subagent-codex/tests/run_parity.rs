//! Real subprocess, provider registration, and quiescence parity.

#![cfg(unix)]

use std::{collections::BTreeMap, sync::Arc};

use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionHeader, SessionId};
use seekdeep_llm::{AbortSignal, ContentBlock, ProviderId};
use seekdeep_scope::ScopeKey;
use seekdeep_subagent::{SubagentRuntime, SubagentStartRequest, SubagentStopReason};
use seekdeep_subagent_codex::{
    CodexRunSpec, Config, DEFAULT_DISPOSE_GRACE_MS, INJECT, NAME, apply, codex_app_server_argv,
    plugin, start_codex_run, text_task,
};
use seekdeep_subprocess::{SUBPROCESS, SubprocessEnvironment};
use seekdeep_subprocess_local::LocalSubprocessRuntime;
use seekdeep_tool_subagent::{BackgroundMode, MaxDepth, ProviderManaged};
use seekdeep_tools::ToolRuntimeConfig;
use serde_json::{Value, json};

struct Harness {
    _context: Context,
    runtime: Arc<LocalSubprocessRuntime>,
    service: Arc<seekdeep_subprocess::SubprocessService>,
    path: tempfile::TempDir,
    workspace: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let context = Context::new();
        let spill = tempfile::tempdir().unwrap();
        let runtime = LocalSubprocessRuntime::install_runtime(
            &context,
            Arc::new(LocalSubprocessRuntime::with_spill_dir(spill.path())),
        )
        .unwrap();
        let service = context.get(SUBPROCESS).unwrap();
        let path = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(
            env!("CARGO_BIN_EXE_seekdeep-codex-app-server-fixture"),
            path.path().join("codex"),
        )
        .unwrap();
        Self {
            _context: context,
            runtime,
            service,
            path,
            workspace: tempfile::tempdir().unwrap(),
        }
    }

    fn spec(&self, mode: &str) -> CodexRunSpec {
        CodexRunSpec {
            cwd: self.workspace.path().to_string_lossy().into_owned(),
            env: BTreeMap::from([
                (
                    "PATH".to_owned(),
                    Some(self.path.path().to_string_lossy().into_owned()),
                ),
                (
                    "SEEKDEEP_CODEX_FIXTURE_MODE".to_owned(),
                    Some(mode.to_owned()),
                ),
            ]),
            dispose_grace_ms: DEFAULT_DISPOSE_GRACE_MS,
            subprocess: Arc::clone(&self.service),
            on_error: None,
        }
    }

    fn request(&self, signal: seekdeep_llm::AbortSignal) -> SubagentStartRequest {
        SubagentStartRequest {
            label: None,
            prompt: vec![ContentBlock::Text {
                text: "do the task".into(),
            }],
            parent: agent(&self.workspace.path().to_string_lossy(), "fixture-parent"),
            signal,
            agent_options: None,
            output_schema: None,
            max_depth: None,
            tool_filter: None,
            persona: None,
        }
    }

    async fn quiescent(&self) {
        for _ in 0..100 {
            if self.runtime.live_process_count() == 0 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(self.runtime.live_process_count(), 0);
    }
}

fn agent(cwd: &str, id: &str) -> Arc<Agent> {
    let id = SessionId::new(id);
    let mut header = SessionHeader::new(id.clone());
    header.cwd = Some(cwd.to_owned());
    let session = Session::create(&id, None, Some(header)).unwrap();
    let inbox = Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
    Arc::new(Agent::new(
        id,
        AgentOptions {
            provider: Some(ProviderId::new("fixture")),
            ..AgentOptions::default()
        },
        session,
        inbox,
        Context::new(),
        ScopeKey::new(),
    ))
}

fn text(result: &seekdeep_subagent::SubagentResult) -> String {
    result
        .output
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str().expect("fixture uses scalar text")),
            _ => None,
        })
        .collect()
}

#[test]
fn fixed_command_task_and_config_boundaries_are_exact() {
    assert_eq!(
        codex_app_server_argv("win32"),
        [
            "cmd.exe",
            "/d",
            "/s",
            "/c",
            "codex",
            "app-server",
            "--stdio"
        ]
    );
    assert_eq!(
        codex_app_server_argv("linux"),
        ["codex", "app-server", "--stdio"]
    );
    assert_eq!(
        text_task(&[
            ContentBlock::Text { text: "one".into() },
            ContentBlock::Text { text: "two".into() }
        ])
        .unwrap(),
        ["one", "two"]
    );
    assert!(
        text_task(&[])
            .unwrap_err()
            .to_string()
            .contains("only text blocks")
    );
    assert!(
        text_task(&[ContentBlock::Reasoning {
            text: "hidden".into()
        }])
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
    assert_eq!(NAME, "subagent-codex");
    assert_eq!(INJECT, ["subagents", "subprocess"]);
}

#[tokio::test]
async fn real_managed_process_returns_exact_output_denies_approval_and_reaches_quiescence() {
    for (mode, expected) in [
        ("success", "fixture answer"),
        ("approval", "approval denied safely"),
    ] {
        let harness = Harness::new();
        let run = start_codex_run(harness.request(AbortSignal::default()), harness.spec(mode))
            .await
            .unwrap();
        assert!(run.local_agent().is_none());
        let result = run.result().await.unwrap();
        assert_eq!(result.stop_reason, SubagentStopReason::Completed);
        assert_eq!(text(&result), expected);
        run.dispose().await.unwrap();
        run.dispose().await.unwrap();
        harness.quiescent().await;
    }
}

#[tokio::test]
async fn real_managed_process_preserves_surrogate_text_and_ignores_opaque_wire_metadata() {
    let harness = Harness::new();
    let expected = ContentBlock::text_utf16(&[0x41, 0xd800, 0x42, 0xdc00]);
    let mut request = harness.request(AbortSignal::default());
    request.prompt = vec![expected.clone()];
    let run = start_codex_run(request, harness.spec("raw-text"))
        .await
        .unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), run.result())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.stop_reason, SubagentStopReason::Completed);
    assert_eq!(result.output, vec![expected]);
    run.dispose().await.unwrap();
    harness.quiescent().await;
}

#[tokio::test]
async fn local_cancellation_wins_and_startup_failure_rolls_back_the_process() {
    let harness = Harness::new();
    let signal = seekdeep_llm::AbortSignal::default();
    let run = start_codex_run(harness.request(signal.clone()), harness.spec("wait"))
        .await
        .unwrap();
    signal.abort_with_reason(json!("stop"));
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), run.result())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.stop_reason, SubagentStopReason::Aborted);
    run.dispose().await.unwrap();
    harness.quiescent().await;

    let harness = Harness::new();
    let Err(error) = start_codex_run(
        harness.request(AbortSignal::default()),
        harness.spec("bad-initialize"),
    )
    .await
    else {
        panic!("bad initialize must reject");
    };
    assert!(error.to_string().contains("invalid initialize response"));
    harness.quiescent().await;
}

#[tokio::test]
async fn plugin_registers_one_fixed_provider_unwinds_and_composes_with_foreground_tool() {
    let context = Context::new();
    let subagents = SubagentRuntime::install(&context).unwrap();
    LocalSubprocessRuntime::install(&context).unwrap();
    let fiber = context.plugin(plugin(), Value::Null).unwrap();
    fiber.await_settled().await.unwrap();
    let provider = subagents.get_provider("codex").unwrap();
    assert_eq!(
        provider.capabilities(),
        &seekdeep_subagent::no_start_capabilities()
    );
    assert!(!provider.inherits_parent_context());
    assert_eq!(subagents.list(), ["codex"]);

    let prompt = seekdeep_system_prompt::install(
        &context,
        seekdeep_system_prompt::SystemPromptConfig::default(),
    )
    .unwrap();
    let tools = seekdeep_tools::install(&context, &prompt, ToolRuntimeConfig::default()).unwrap();
    seekdeep_tool_subagent::apply(
        &context,
        seekdeep_tool_subagent::Config {
            provider: "codex".to_owned(),
            tool_name: Some("codex".to_owned()),
            enable_run_in_background: Some(false),
            background_mode: Some(BackgroundMode::OneShot),
            agent_options: None,
            persona: None,
            tool_filter: None,
            max_depth: Some(MaxDepth::ProviderManaged(ProviderManaged::Value)),
        },
    )
    .unwrap();
    assert!(tools.get("codex", None).is_some());

    fiber.dispose().await.unwrap();
    assert!(subagents.get_provider("codex").is_none());
    context.fiber().dispose().await.unwrap();

    for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let context = Context::new();
        SubagentRuntime::install(&context).unwrap();
        LocalSubprocessRuntime::install(&context).unwrap();
        let error = apply(
            &context,
            Config {
                dispose_grace_ms: invalid,
                ..Config::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("positive finite"));
    }
}

#[tokio::test]
async fn provider_requires_parent_workspace_before_spawning() {
    let context = Context::new();
    let subagents = SubagentRuntime::install(&context).unwrap();
    let runtime = LocalSubprocessRuntime::install(&context).unwrap();
    apply(&context, Config::default()).unwrap();
    let id = SessionId::new("no-cwd");
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
    let Err(error) = subagents
        .start(
            "codex",
            SubagentStartRequest {
                label: None,
                prompt: vec![ContentBlock::Text {
                    text: "task".into(),
                }],
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
    else {
        panic!("missing cwd must reject");
    };
    assert_eq!(
        error.to_string(),
        "subagent-codex: no working directory for the child — delegate from a parent session that has one"
    );
    assert_eq!(runtime.live_process_count(), 0);
}

#[test]
fn environment_shape_stays_string_only_at_public_config_boundary() {
    let config: Config = serde_json::from_value(json!({
        "env":{"OPENAI_API_KEY":"fake"},
        "disposeGraceMs":3000
    }))
    .unwrap();
    assert_eq!(config.env["OPENAI_API_KEY"], "fake");
    let environment: SubprocessEnvironment = config
        .env
        .into_iter()
        .map(|(key, value)| (key, Some(value)))
        .collect();
    assert_eq!(environment["OPENAI_API_KEY"], Some("fake".to_owned()));
}
