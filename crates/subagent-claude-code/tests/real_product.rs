//! Manual keyless native Claude CLI integration against loopback Messages.

#![cfg(unix)]

mod support {
    pub(crate) mod messages_fixture;
}

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionHeader, SessionId};
use seekdeep_llm::{AbortSignal, ContentBlock};
use seekdeep_scope::ScopeKey;
use seekdeep_subagent::{SubagentRuntime, SubagentStartRequest, SubagentStopReason};
use seekdeep_subagent_claude_code::{Config, apply};
use seekdeep_subprocess_local::LocalSubprocessRuntime;

use support::messages_fixture::{Behavior, MessagesFixture};

fn parent(context: &Context, cwd: &str) -> Arc<Agent> {
    let id = SessionId::new("real-parent");
    let mut header = SessionHeader::new(id.clone());
    header.cwd = Some(cwd.to_owned());
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

struct RealHarness {
    context: Context,
    subagents: Arc<SubagentRuntime>,
    runtime: Arc<LocalSubprocessRuntime>,
    parent: Arc<Agent>,
    _root: tempfile::TempDir,
}

impl RealHarness {
    fn new(fixture: &MessagesFixture) -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let config = root.path().join("claude-config");
        let bin = root.path().join("bin");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir(&config).unwrap();
        std::fs::create_dir(&bin).unwrap();
        let native = std::process::Command::new("which")
            .arg("claude")
            .output()
            .unwrap();
        assert!(native.status.success(), "native claude is required");
        let native = String::from_utf8(native.stdout).unwrap();
        std::os::unix::fs::symlink(native.trim(), bin.join("claude")).unwrap();
        std::fs::write(
            config.join("settings.json"),
            serde_json::to_vec_pretty(&serde_json::json!({"model":"seekdeep-fixture-model"}))
                .unwrap(),
        )
        .unwrap();
        let environment = BTreeMap::from([
            (
                "PATH".to_owned(),
                format!("{}:/usr/bin:/bin", bin.display()),
            ),
            (
                "ANTHROPIC_API_KEY".to_owned(),
                "seekdeep-fake-key".to_owned(),
            ),
            ("ANTHROPIC_BASE_URL".to_owned(), fixture.base_url.clone()),
            (
                "CLAUDE_CONFIG_DIR".to_owned(),
                config.to_string_lossy().into_owned(),
            ),
            (
                "HOME".to_owned(),
                root.path().to_string_lossy().into_owned(),
            ),
            (
                "XDG_CONFIG_HOME".to_owned(),
                root.path().join("xdg").to_string_lossy().into_owned(),
            ),
            (
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_owned(),
                "1".to_owned(),
            ),
            (
                "CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL".to_owned(),
                "1".to_owned(),
            ),
            ("DISABLE_TELEMETRY".to_owned(), "1".to_owned()),
            ("DISABLE_ERROR_REPORTING".to_owned(), "1".to_owned()),
            ("HTTP_PROXY".to_owned(), String::new()),
            ("HTTPS_PROXY".to_owned(), String::new()),
            ("ALL_PROXY".to_owned(), String::new()),
            ("NO_PROXY".to_owned(), "127.0.0.1,localhost".to_owned()),
        ]);
        let context = Context::new();
        let subagents = SubagentRuntime::install(&context).unwrap();
        let runtime = LocalSubprocessRuntime::install(&context).unwrap();
        apply(
            &context,
            Config {
                env: environment,
                dispose_grace_ms: 3_000.0,
            },
        )
        .unwrap();
        let parent = parent(&context, &workspace.to_string_lossy());
        Self {
            context,
            subagents,
            runtime,
            parent,
            _root: root,
        }
    }

    async fn start(
        &self,
        prompt: &str,
        signal: AbortSignal,
    ) -> Arc<dyn seekdeep_subagent::SubagentRun> {
        self.subagents
            .start(
                "claude-code",
                SubagentStartRequest {
                    label: None,
                    prompt: vec![ContentBlock::Text {
                        text: prompt.to_owned(),
                    }],
                    parent: Arc::clone(&self.parent),
                    signal,
                    agent_options: None,
                    output_schema: None,
                    max_depth: None,
                    tool_filter: None,
                    persona: None,
                },
            )
            .await
            .unwrap()
    }

    async fn quiescent(&self) {
        for _ in 0..600 {
            if self.runtime.live_process_count() == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(self.runtime.live_process_count(), 0);
    }
}

#[tokio::test]
#[ignore = "requires a native claude installation"]
async fn native_cli_inherits_settings_sends_exact_task_and_cancels_quiescently() {
    let sentinel = "REAL_RUST_CLAUDE_CODE_SENTINEL";
    let task = "Return the fixture sentinel exactly.";
    let fixture = MessagesFixture::start(Behavior::Complete {
        text: sentinel.to_owned(),
    })
    .await
    .unwrap();
    let harness = RealHarness::new(&fixture);
    let run = harness.start(task, AbortSignal::default()).await;
    let result = run.result().await.unwrap();
    assert_eq!(result.stop_reason, SubagentStopReason::Completed);
    assert_eq!(result_text(&result), sentinel);
    run.dispose().await.unwrap();
    harness.quiescent().await;
    {
        let requests = fixture.requests.lock();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].path.starts_with("/v1/messages"));
        assert_eq!(
            requests[0]
                .headers
                .iter()
                .find(|(name, _)| name == "x-api-key")
                .map(|(_, value)| value.as_str()),
            Some("seekdeep-fake-key")
        );
        assert_eq!(requests[0].body["model"], "seekdeep-fixture-model");
        assert!(requests[0].body.to_string().contains(task));
    }
    harness.context.fiber().dispose().await.unwrap();
    fixture.close().await;

    let fixture = MessagesFixture::start(Behavior::Hold).await.unwrap();
    let harness = RealHarness::new(&fixture);
    let signal = AbortSignal::default();
    let run = harness
        .start("Wait for cancellation.", signal.clone())
        .await;
    fixture.started.acquire().await.unwrap().forget();
    signal.abort();
    assert_eq!(
        run.result().await.unwrap().stop_reason,
        SubagentStopReason::Aborted
    );
    run.dispose().await.unwrap();
    harness.quiescent().await;
    harness.context.fiber().dispose().await.unwrap();
    fixture.close().await;
}
