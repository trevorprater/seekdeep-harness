//! Required keyless real Claude Agent SDK 0.3.220 and Claude Code 2.1.220 integration against
//! loopback Messages.
//!
//! Like the source, the tests run the SDK's own pinned platform CLI, found beside the SDK package
//! the way `import.meta.resolve` finds it and symlinked into a `PATH` entry whose name carries
//! shell metacharacters, so a host-installed `claude` never answers for it. The source also
//! deletes an ambient `ANTHROPIC_MODEL` and `ANTHROPIC_SMALL_FAST_MODEL` for the file; this
//! process cannot unset variables safely, so the settings-inheritance assertion relies on the
//! developer shell leaving them unset, as CI does.

#![cfg(unix)]

mod support;

use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionHeader, SessionId};
use seekdeep_llm::{AbortSignal, ContentBlock};
use seekdeep_scope::ScopeKey;
use seekdeep_subagent::{SubagentRuntime, SubagentStartRequest, SubagentStopReason};
use seekdeep_subagent_claude_code::{Config, apply};
use serde_json::{Value, json};

use support::{
    messages_fixture::{Behavior, MessagesFixture, RecordedRequest},
    sdk_package::PinnedCli,
    spying_subprocess::SpyingSubprocessRuntime,
};

const SETTINGS_MODEL: &str = "seekdeep-settings-inheritance-marker";
const FAKE_KEY: &str = "seekdeep-fake-anthropic-key";

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
            ContentBlock::Text { text } => Some(text.as_str().expect("fixture uses scalar text")),
            _ => None,
        })
        .collect()
}

fn header<'a>(request: &'a RecordedRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(header, _)| header == name)
        .map(|(_, value)| value.as_str())
}

/// Every text block across the request's messages, as the source flattens them.
fn message_texts(body: &Value) -> Vec<String> {
    body.get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|message| message.get("content").and_then(Value::as_array))
        .flatten()
        .filter(|block| block.get("type") == Some(&json!("text")))
        .filter_map(|block| block.get("text").and_then(Value::as_str).map(str::to_owned))
        .collect()
}

struct RealHarness {
    context: Context,
    /// Owns the real local runtime's teardown behind the spy.
    owner: Context,
    subagents: Arc<SubagentRuntime>,
    subprocess: Arc<SpyingSubprocessRuntime>,
    parent: Arc<Agent>,
    executable: PathBuf,
    env: BTreeMap<String, String>,
    _root: tempfile::TempDir,
}

impl RealHarness {
    fn new(fixture: &MessagesFixture, pinned: &PinnedCli) -> anyhow::Result<Self> {
        let root = tempfile::Builder::new()
            .prefix("seekdeep-claude-code-real-")
            .tempdir()?;
        let workspace = root.path().join("workspace");
        let claude_config = root.path().join("claude-config");
        let xdg_config = root.path().join("xdg");
        let native_bin = root.path().join("native&%literal%!bang!bin");
        for directory in [&workspace, &claude_config, &xdg_config, &native_bin] {
            std::fs::create_dir(directory)?;
        }
        let executable = native_bin.join("claude");
        std::os::unix::fs::symlink(&pinned.claude_bin, &executable)?;
        std::fs::write(
            claude_config.join("settings.json"),
            format!(
                "{}\n",
                serde_json::to_string_pretty(&json!({ "model": SETTINGS_MODEL }))?
            ),
        )?;
        let path = std::env::join_paths(std::iter::once(native_bin.clone()).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))?
        .to_string_lossy()
        .into_owned();
        let text = |value: &str| value.to_owned();
        let env = BTreeMap::from([
            ("PATH".to_owned(), path),
            ("ANTHROPIC_API_KEY".to_owned(), text(FAKE_KEY)),
            ("ANTHROPIC_BASE_URL".to_owned(), fixture.base_url.clone()),
            (
                "CLAUDE_CONFIG_DIR".to_owned(),
                claude_config.to_string_lossy().into_owned(),
            ),
            (
                "HOME".to_owned(),
                root.path().to_string_lossy().into_owned(),
            ),
            (
                "XDG_CONFIG_HOME".to_owned(),
                xdg_config.to_string_lossy().into_owned(),
            ),
            (
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_owned(),
                text("1"),
            ),
            (
                "CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL".to_owned(),
                text("1"),
            ),
            ("DISABLE_TELEMETRY".to_owned(), text("1")),
            ("DISABLE_ERROR_REPORTING".to_owned(), text("1")),
            ("HTTP_PROXY".to_owned(), String::new()),
            ("HTTPS_PROXY".to_owned(), String::new()),
            ("ALL_PROXY".to_owned(), String::new()),
            ("NO_PROXY".to_owned(), text("127.0.0.1,localhost")),
        ]);
        let context = Context::new();
        let owner = Context::new();
        let subagents = SubagentRuntime::install(&context)?;
        let subprocess = SpyingSubprocessRuntime::install(&owner, &context)?;
        apply(
            &context,
            Config {
                env: env.clone(),
                dispose_grace_ms: 3_000.0,
            },
        )?;
        let parent = parent(&context, &workspace.to_string_lossy());
        Ok(Self {
            context,
            owner,
            subagents,
            subprocess,
            parent,
            executable,
            env,
            _root: root,
        })
    }

    async fn start(
        &self,
        prompt: &str,
        signal: AbortSignal,
    ) -> anyhow::Result<Arc<dyn seekdeep_subagent::SubagentRun>> {
        self.subagents
            .start(
                "claude-code",
                SubagentStartRequest {
                    label: None,
                    prompt: vec![ContentBlock::Text {
                        text: prompt.into(),
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
    }

    async fn dispose(self) -> anyhow::Result<()> {
        self.context.fiber().dispose().await?;
        self.owner.fiber().dispose().await
    }
}

#[tokio::test]
async fn inherits_host_settings_and_sends_the_exact_task_and_fake_key_to_local_messages()
-> anyhow::Result<()> {
    let sentinel = "REAL_CLAUDE_CODE_SENTINEL_2_1_220";
    let task = "Return the fixture sentinel exactly.";
    let fixture = MessagesFixture::start(Behavior::Complete {
        text: sentinel.to_owned(),
    })
    .await?;
    let pinned = PinnedCli::resolve()?;
    let harness = RealHarness::new(&fixture, &pinned)?;
    pinned.assert_versions(&harness.executable, &harness.env)?;

    let run = harness.start(task, AbortSignal::default()).await?;
    let result = tokio::time::timeout(Duration::from_secs(60), run.result()).await??;
    assert_eq!(result.stop_reason, SubagentStopReason::Completed);
    assert_eq!(result.output.len(), 1);
    assert_eq!(result_text(&result), sentinel);
    run.dispose().await?;

    let spawns = harness.subprocess.spawns();
    assert_eq!(
        spawns.first().and_then(|spec| spec.argv.first()),
        Some(&harness.executable.to_string_lossy().into_owned())
    );

    let requests = fixture.requests.lock().clone();
    assert_eq!(requests.len(), 1);
    let recorded = &requests[0];
    assert!(recorded.path == "/v1/messages" || recorded.path.starts_with("/v1/messages?"));
    assert_eq!(header(recorded, "x-api-key"), Some(FAKE_KEY));
    assert_eq!(recorded.body["model"], SETTINGS_MODEL);
    assert!(recorded.body["messages"].is_array());
    let texts = message_texts(&recorded.body);
    assert_eq!(
        texts
            .iter()
            .filter(|text| text.contains(task))
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec![task]
    );
    harness.subprocess.expect_quiescent().await;
    harness.dispose().await?;
    fixture.close().await;
    Ok(())
}

#[tokio::test]
async fn maps_a_real_cli_process_failure_to_error() -> anyhow::Result<()> {
    let fixture = MessagesFixture::start(Behavior::Hold).await?;
    let pinned = PinnedCli::resolve()?;
    let harness = RealHarness::new(&fixture, &pinned)?;
    let run = harness
        .start("Exercise the failure path.", AbortSignal::default())
        .await?;
    fixture.started.acquire().await?.forget();
    let handles = harness.subprocess.handles();
    assert_eq!(handles.len(), 1);
    handles[0].terminate();
    let result = tokio::time::timeout(Duration::from_secs(60), run.result()).await??;
    assert_eq!(result.stop_reason, SubagentStopReason::Error);
    assert!(result.output.is_empty());
    run.dispose().await?;
    {
        let requests = fixture.requests.lock();
        assert_eq!(requests.len(), 1);
        assert_eq!(header(&requests[0], "x-api-key"), Some(FAKE_KEY));
    }
    harness.subprocess.expect_quiescent().await;
    harness.dispose().await?;
    fixture.close().await;
    Ok(())
}

#[tokio::test]
async fn settles_cancellation_and_leaves_the_real_sdk_spawned_cli_tree_quiescent()
-> anyhow::Result<()> {
    let fixture = MessagesFixture::start(Behavior::Hold).await?;
    let pinned = PinnedCli::resolve()?;
    let harness = RealHarness::new(&fixture, &pinned)?;
    let signal = AbortSignal::default();
    let run = harness
        .start("Wait for cancellation.", signal.clone())
        .await?;
    fixture.started.acquire().await?.forget();
    signal.abort_with_reason(json!("real product cancellation"));
    let result = tokio::time::timeout(Duration::from_secs(60), run.result()).await??;
    assert_eq!(result.stop_reason, SubagentStopReason::Aborted);
    assert!(result.output.is_empty());
    run.dispose().await?;
    harness.subprocess.expect_quiescent().await;
    harness.dispose().await?;
    fixture.close().await;
    Ok(())
}
