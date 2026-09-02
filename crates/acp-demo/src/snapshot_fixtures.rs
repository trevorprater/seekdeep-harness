//! Compiled test-only plugins referenced by ACP snapshot overlays.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_agent::{AgentEvent, AgentLifecycleEvent};
use seekdeep_agent_loop::{AgentInboxMessage, AgentPreStepEvent};
use seekdeep_compaction::{CompactionId, compact_checkpoint_source};
use seekdeep_cordis::{EventOptions, EventReply, Plugin, fiber::EffectHandle};
use seekdeep_core::session::{AppendOptions, Session, SessionEvent, SessionId, SurfaceOp};
use seekdeep_llm::{AbortSignal, ContentBlock, MessageSource, UserMessage};
use seekdeep_sandbox::{
    ConfinedArgv, RunnerFailureRule, SandboxEnforcement, SandboxPolicy, SandboxProvider,
    SandboxService,
};
use seekdeep_subprocess::ProcessGroupId;
use seekdeep_subagent::{
    ContinuableCreateRequest, ContinuableCreateSpec, ResolvedSubagentStartRequest, SUBAGENTS,
    SubagentCapabilities, SubagentFollowupOptions, SubagentProvider, SubagentRun,
};
use seekdeep_subagent_in_process_driver::{InProcessRunOptions, start_in_process_run};
use seekdeep_terminal::{
    TERMINALS, TerminalBackend, TerminalBackendRef, TerminalBackendSession,
    TerminalBackendSessionRef, TerminalBackendSpawnSpec, TerminalReadRequest, TerminalReadResult,
    TerminalResult, TerminalSendOperation, TerminalSendOperationRef, TerminalSendRead,
    TerminalSendRequest, TerminalSendResult, TerminalSessionStatus, TerminalSignal,
    TerminalSignalResult, TerminalWaitReason,
};
use seekdeep_tools::{TOOLS, ToolExecutionResult};
use seekdeep_user_questions::{
    AskUserQuestionAnswer, AskUserQuestionRequest, USER_QUESTIONS, UserQuestionProvider,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::Notify;

const PARTIAL_LANDLOCK_NOTICE: &str = "landlock-run: partial enforcement (older Landlock ABI)";
const WEB_FIXTURE_PORT: u16 = 43_117;
const PLACEHOLDER_CHILD_ID: &str = "33333333-3333-4333-8333-333333333333";
const UNKNOWN_CHILD_ID: &str = "22222222-2222-4222-8222-222222222222";
const FAILED_CHECKPOINT_TURN: u64 = 3;
const WEB_FIXTURE_PAGE: &str = r#"<!doctype html>
<html><head><title>Menu</title><style>.x{color:red}</style><script>ignored()</script></head>
<body>
<h1>Caf&eacute; menu</h1>
<p>Prices include <strong>service &amp; <em>tax</em></strong> &mdash; updated daily.</p>
<ul><li>Espresso</li><li>Flat white</li></ul>
<table><thead><tr><th>Drink</th><th>Price</th></tr></thead><tbody><tr><td>Espresso</td><td>&euro;2</td></tr><tr><td>Flat white</td><td>&euro;3</td></tr></tbody></table>
<p>See <a href="https://fixture.invalid/specials">today&rsquo;s specials</a>.</p>
</body></html>
"#;

pub(crate) fn subagent_settlement_marker_plugin() -> Plugin {
    Plugin::new(
        "subagent-settlement-marker",
        std::iter::empty::<&'static str>(),
        |context, _| {
            Box::pin(async move {
                context.events().on_sync(
                    &context,
                    "subagent/end",
                    |_, _| {
                        std::fs::write(
                            std::env::current_dir()?.join(".seekdeep-snapshot-subagent-settled"),
                            "",
                        )?;
                        Ok(EventReply::Undefined)
                    },
                    global_events(),
                )?;
                Ok(())
            })
        },
    )
}

#[derive(Debug)]
struct PartialLandlockSandbox;

impl SandboxProvider for PartialLandlockSandbox {
    fn confine(&self, argv: &[String], policy: &SandboxPolicy) -> anyhow::Result<ConfinedArgv> {
        if std::env::var("SEEKDEEP_SNAPSHOT_MISSING_SANDBOX_RUNNER").as_deref() == Ok("1") {
            return Ok(ConfinedArgv {
                argv: std::iter::once(
                    policy
                        .workspace_root
                        .join(".seekdeep-missing-sandbox-runner")
                        .to_string_lossy()
                        .into_owned(),
                )
                .chain(argv.iter().cloned())
                .collect(),
                enforcement: SandboxEnforcement::Full,
                denial_signatures: vec!["permission denied".to_owned()],
                runner_failure_rules: vec![RunnerFailureRule {
                    allowed_exit_codes: None,
                    fatal_signatures: vec!["snapshot-runner: ".to_owned()],
                    informational_lines: None,
                }],
            });
        }
        Ok(ConfinedArgv {
            argv: [
                "bash".to_owned(),
                "-c".to_owned(),
                format!(
                    "printf '%s\\n' '{}' >&2; exec \"$@\"",
                    PARTIAL_LANDLOCK_NOTICE
                ),
                "partial-landlock-run".to_owned(),
            ]
            .into_iter()
            .chain(argv.iter().cloned())
            .collect(),
            enforcement: SandboxEnforcement::Partial,
            denial_signatures: vec!["permission denied".to_owned()],
            runner_failure_rules: vec![RunnerFailureRule {
                allowed_exit_codes: Some(vec![125]),
                fatal_signatures: vec!["landlock-run: ".to_owned()],
                informational_lines: Some(vec![PARTIAL_LANDLOCK_NOTICE.to_owned()]),
            }],
        })
    }
}

pub(crate) fn partial_landlock_sandbox_plugin() -> Plugin {
    Plugin::new(
        "partial-landlock-sandbox",
        std::iter::empty::<&'static str>(),
        |context, _| {
            Box::pin(async move {
                SandboxService::new(Arc::new(PartialLandlockSandbox)).provide(&context)?;
                Ok(())
            })
        },
    )
}

pub(crate) fn parent_sandbox_override_plugin() -> Plugin {
    Plugin::new(
        "parent-sandbox-override",
        std::iter::empty::<&'static str>(),
        |context, _| {
            Box::pin(async move {
                context.events().on_sync(
                    &context,
                    "agent/created",
                    |_, args| {
                        let event = args.get::<AgentLifecycleEvent>(0).ok_or_else(|| {
                            anyhow::anyhow!("agent/created lacks its Agent event")
                        })?;
                        if event.agent.session().header().parent_session.is_none() {
                            seekdeep_sandbox_policy::set_sandbox_mode(
                                event.agent.session(),
                                seekdeep_sandbox::SandboxMode::ReadOnly,
                            )?;
                        }
                        Ok(EventReply::Undefined)
                    },
                    global_events(),
                )?;
                Ok(())
            })
        },
    )
}

#[derive(Debug)]
struct ChildQuestionTripwire;

#[async_trait]
impl UserQuestionProvider for ChildQuestionTripwire {
    async fn ask(&self, _request: AskUserQuestionRequest) -> anyhow::Result<AskUserQuestionAnswer> {
        anyhow::bail!("snapshot tripwire: delegated question reached the UI provider")
    }
}

pub(crate) fn child_question_tripwire_plugin() -> Plugin {
    Plugin::new(
        "child-question-tripwire",
        ["userQuestions"],
        |context, _| {
            Box::pin(async move {
                let questions = context.get(USER_QUESTIONS).ok_or_else(|| {
                    anyhow::anyhow!("child-question-tripwire requires userQuestions")
                })?;
                questions.register_provider(&context, Arc::new(ChildQuestionTripwire))?;
                Ok(())
            })
        },
    )
}

pub(crate) fn web_fetch_fixture_server_plugin() -> Plugin {
    Plugin::new(
        "web-fetch-fixture-server",
        std::iter::empty::<&'static str>(),
        |context, _| {
            Box::pin(async move {
                let listener = tokio::net::TcpListener::bind((
                    std::net::Ipv4Addr::LOCALHOST,
                    WEB_FIXTURE_PORT,
                ))
                .await?;
                let task = tokio::spawn(async move {
                    loop {
                        let (mut stream, _) = listener.accept().await?;
                        let mut request = vec![0_u8; 8 * 1024];
                        let count = stream.read(&mut request).await?;
                        let menu = request[..count].starts_with(b"GET /menu.html ");
                        let (status, content_type, body) = if menu {
                            ("200 OK", "text/html; charset=utf-8", WEB_FIXTURE_PAGE)
                        } else {
                            ("404 Not Found", "text/plain; charset=utf-8", "not found")
                        };
                        let response = format!(
                            "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        stream.write_all(response.as_bytes()).await?;
                        stream.shutdown().await?;
                    }
                    #[allow(unreachable_code)]
                    Ok::<(), std::io::Error>(())
                });
                context.own(EffectHandle::new("web-fetch-fixture-server", move || {
                    task.abort();
                    Box::pin(async move {
                        let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
                        Ok(())
                    })
                }))?;
                Ok(())
            })
        },
    )
}

#[derive(Debug)]
struct SnapshotTerminalOperation {
    result: TerminalSendResult,
    delta: Mutex<Option<String>>,
}

impl TerminalSendOperation for SnapshotTerminalOperation {
    fn done(&self) -> futures::future::BoxFuture<'static, TerminalResult<TerminalSendResult>> {
        let result = self.result.clone();
        Box::pin(async move { Ok(result) })
    }

    fn read_output(&self) -> TerminalSendRead {
        TerminalSendRead {
            delta: self.delta.lock().take().unwrap_or_default(),
            truncated: false,
        }
    }

    fn cancel(&self) -> bool {
        false
    }
}

#[derive(Debug)]
struct SnapshotTerminalSession {
    scrollback: Mutex<String>,
    status: Mutex<TerminalSessionStatus>,
}

#[async_trait]
impl TerminalBackendSession for SnapshotTerminalSession {
    fn motd(&self) -> String {
        "seekdeep> ".to_owned()
    }

    fn pid(&self) -> Option<seekdeep_subprocess::ProcessId> {
        None
    }

    fn start_send(&self, request: TerminalSendRequest) -> TerminalResult<TerminalSendOperationRef> {
        let viewport = format!("{}\nPTY_OK\nseekdeep> ", request.text);
        self.scrollback.lock().push_str(&viewport);
        Ok(Arc::new(SnapshotTerminalOperation {
            result: TerminalSendResult {
                viewport: viewport.clone(),
                wait_reason: TerminalWaitReason::StdinRead,
                session_status: self.status.lock().clone(),
                truncated: false,
            },
            delta: Mutex::new(Some(viewport)),
        }))
    }

    fn read(&self, request: TerminalReadRequest) -> TerminalResult<TerminalReadResult> {
        let scrollback = self.scrollback.lock();
        let lines = scrollback.split('\n').collect::<Vec<_>>();
        let offset = request.offset.unwrap_or(0.0).max(0.0) as usize;
        let count = request.count.unwrap_or(500.0).max(0.0) as usize;
        let end = lines.len().saturating_sub(offset);
        let start = end.saturating_sub(count);
        let text = lines[start..end].join("\n");
        let line_count = text.split('\n').count();
        Ok(TerminalReadResult {
            text,
            total_lines: u64::try_from(lines.len()).unwrap_or(u64::MAX),
            line_begin: u64::try_from(offset).unwrap_or(u64::MAX),
            line_end: u64::try_from(offset.saturating_add(line_count)).unwrap_or(u64::MAX),
            truncated: false,
        })
    }

    async fn signal(&self, _signal: TerminalSignal) -> TerminalResult<TerminalSignalResult> {
        Ok(TerminalSignalResult::delivered(ProcessGroupId::new(1)))
    }

    fn status(&self) -> TerminalSessionStatus {
        self.status.lock().clone()
    }

    async fn close(&self, _reason: &str) -> TerminalResult<()> {
        *self.status.lock() = serde_json::from_value(serde_json::json!({
            "kind":"exited","exitCode":0,"signal":null
        }))
        .map_err(|error| seekdeep_terminal::TerminalFailure::new(error))?;
        Ok(())
    }
}

#[derive(Debug)]
struct SnapshotTerminalBackend;

#[async_trait]
impl TerminalBackend for SnapshotTerminalBackend {
    fn backend_type(&self) -> &str {
        "shell"
    }

    async fn spawn(
        &self,
        _spec: TerminalBackendSpawnSpec,
    ) -> TerminalResult<TerminalBackendSessionRef> {
        Ok(Arc::new(SnapshotTerminalSession {
            scrollback: Mutex::new("seekdeep> ".to_owned()),
            status: Mutex::new(TerminalSessionStatus::Running),
        }))
    }
}

pub(crate) fn pty_snapshot_backend_plugin() -> Plugin {
    Plugin::new("pty-snapshot-backend", ["terminals"], |context, _| {
        Box::pin(async move {
            let terminals = context
                .get(TERMINALS)
                .ok_or_else(|| anyhow::anyhow!("pty-snapshot-backend requires terminals"))?;
            let backend: TerminalBackendRef = Arc::new(SnapshotTerminalBackend);
            terminals.register_backend(&context, &backend)?;
            Ok(())
        })
    })
}

pub(crate) fn workspace_context_compaction_plugin() -> Plugin {
    Plugin::new("workspace-context-compaction", ["tools"], |context, _| {
        Box::pin(async move {
            let tools = context
                .get(TOOLS)
                .ok_or_else(|| anyhow::anyhow!("workspace-context-compaction requires tools"))?;
            tools.on_post_execute(
                &context,
                |execution, result, next| async move {
                    let downstream = next.run().await?;
                    let Some(agent) = execution.agent else {
                        return Ok(downstream);
                    };
                    if result.is_error()
                        || execution.name != "read"
                        || execution
                            .arguments
                            .get("file_path")
                            .and_then(serde_json::Value::as_str)
                            != Some("nested/task.txt")
                    {
                        return Ok(downstream);
                    }
                    let events = agent.session().events();
                    let baseline = agent
                        .session()
                        .surface_nodes()
                        .into_iter()
                        .filter_map(|seq| events.get(usize::try_from(seq).ok()?))
                        .find(|event| {
                            event.event_type == "user/message"
                                && event
                                    .data
                                    .pointer("/source/kind")
                                    .and_then(serde_json::Value::as_str)
                                    == Some("agent-instructions")
                                && event
                                    .data
                                    .pointer("/source/baseline")
                                    .and_then(serde_json::Value::as_bool)
                                    == Some(true)
                        })
                        .ok_or_else(|| {
                            anyhow::anyhow!("workspace baseline missing before snapshot compaction")
                        })?;
                    let message = UserMessage::new(
                        vec![ContentBlock::Text {
                            text: "Earlier context was compacted for this snapshot.".to_owned(),
                        }],
                        compact_checkpoint_source(
                            &CompactionId::new("workspace-context-fixture"),
                            None,
                        ),
                    );
                    agent.session().append(
                        "user/message",
                        serde_json::to_value(message)?,
                        AppendOptions {
                            surface_op: Some(SurfaceOp::replace(baseline.seq, baseline.seq)),
                            source_event_seqs: Some(vec![baseline.seq]),
                            ignorable: false,
                        },
                    )?;
                    Ok(downstream)
                },
                global_events(),
            )?;
            Ok(())
        })
    })
}

#[derive(Debug, Default)]
struct SettlementFence {
    child_ready: AtomicBool,
    child_ready_changed: Notify,
    parent_stopped: AtomicBool,
    parent_stopped_changed: Notify,
}

impl SettlementFence {
    fn mark_child_ready(&self) {
        self.child_ready.store(true, Ordering::Release);
        self.child_ready_changed.notify_waiters();
    }

    fn mark_parent_stopped(&self) {
        self.parent_stopped.store(true, Ordering::Release);
        self.parent_stopped_changed.notify_waiters();
    }

    async fn wait_child_ready(&self) {
        while !self.child_ready.load(Ordering::Acquire) {
            let notified = self.child_ready_changed.notified();
            if self.child_ready.load(Ordering::Acquire) {
                break;
            }
            notified.await;
        }
    }

    async fn wait_parent_stopped(&self) {
        while !self.parent_stopped.load(Ordering::Acquire) {
            let notified = self.parent_stopped_changed.notified();
            if self.parent_stopped.load(Ordering::Acquire) {
                break;
            }
            notified.await;
        }
    }
}

pub(crate) fn subagent_report_fence_plugin() -> Plugin {
    Plugin::new(
        "subagent-report-fence",
        std::iter::empty::<&'static str>(),
        |context, _| {
            Box::pin(async move {
                let fence = Arc::new(SettlementFence::default());
                let session_fence = fence.clone();
                context.events().on_sync(
                    &context,
                    "session/event",
                    move |_, args| {
                        let session = args
                            .get::<Session>(0)
                            .ok_or_else(|| anyhow::anyhow!("session/event lacks its session"))?;
                        let event = args
                            .get::<SessionEvent>(1)
                            .ok_or_else(|| anyhow::anyhow!("session/event lacks its event"))?;
                        if session.header().parent_session.is_none()
                            && event.event_type == "turn/end"
                            && event.data.get("turn").and_then(serde_json::Value::as_u64) == Some(1)
                        {
                            session_fence.mark_parent_stopped();
                        }
                        Ok(EventReply::Undefined)
                    },
                    global_events(),
                )?;
                context.events().on_waterfall(
                    &context,
                    "agent/pre-step",
                    move |_, args, next| {
                        let fence = fence.clone();
                        Box::pin(async move {
                            let event = args
                                .get::<AgentEvent<AgentPreStepEvent>>(0)
                                .ok_or_else(|| anyhow::anyhow!("agent/pre-step lacks its event"))?;
                            if event.agent.session().header().parent_session.is_some() {
                                fence.mark_child_ready();
                                fence.wait_parent_stopped().await;
                            } else if event.payload.turn == 1 && event.payload.step == 2 {
                                fence.wait_child_ready().await;
                            }
                            next.run().await
                        })
                    },
                    global_events(),
                )?;
                Ok(())
            })
        },
    )
}

#[derive(Debug, Default)]
struct DurabilityFixtureState {
    accepted: AtomicUsize,
    accepted_changed: Notify,
    parent_closed: AtomicBool,
    parent_closed_changed: Notify,
    released: AtomicBool,
    real_child_id: Mutex<Option<SessionId>>,
    child_turns: Mutex<HashMap<SessionId, u64>>,
}

impl DurabilityFixtureState {
    fn record_child_message(&self, child_id: &SessionId) {
        let accepted = self.accepted.fetch_add(1, Ordering::AcqRel) + 1;
        let mut real_child_id = self.real_child_id.lock();
        if real_child_id.is_none() {
            *real_child_id = Some(child_id.clone());
        }
        drop(real_child_id);
        if accepted >= 3 {
            self.accepted_changed.notify_waiters();
        }
    }

    fn mapped_child_id(&self, child_id: &str) -> Option<SessionId> {
        (child_id == PLACEHOLDER_CHILD_ID)
            .then(|| self.real_child_id.lock().clone())
            .flatten()
    }

    fn mark_parent_closed(&self) {
        self.parent_closed.store(true, Ordering::Release);
        self.parent_closed_changed.notify_waiters();
    }

    fn release(&self) {
        self.released.store(true, Ordering::Release);
        self.accepted_changed.notify_waiters();
        self.parent_closed_changed.notify_waiters();
    }

    async fn wait_followups(&self) {
        while self.accepted.load(Ordering::Acquire) < 3
            && !self.released.load(Ordering::Acquire)
        {
            let changed = self.accepted_changed.notified();
            if self.accepted.load(Ordering::Acquire) >= 3
                || self.released.load(Ordering::Acquire)
            {
                break;
            }
            changed.await;
        }
    }

    async fn wait_parent_closed(&self) {
        while !self.parent_closed.load(Ordering::Acquire)
            && !self.released.load(Ordering::Acquire)
        {
            let changed = self.parent_closed_changed.notified();
            if self.parent_closed.load(Ordering::Acquire)
                || self.released.load(Ordering::Acquire)
            {
                break;
            }
            changed.await;
        }
    }
}

struct PublishedFailureRun {
    inner: Arc<dyn SubagentRun>,
    signal: AbortSignal,
}

#[async_trait]
impl SubagentRun for PublishedFailureRun {
    fn id(&self) -> &SessionId {
        self.inner.id()
    }

    fn local_agent(&self) -> Option<&Arc<seekdeep_agent::Agent>> {
        self.inner.local_agent()
    }

    fn result(&self) -> futures::future::BoxFuture<'static, anyhow::Result<seekdeep_subagent::SubagentResult>> {
        Box::pin(async { anyhow::bail!("snapshot published run failed") })
    }

    fn dispose(&self) -> futures::future::BoxFuture<'static, anyhow::Result<()>> {
        let inner = self.inner.clone();
        let signal = self.signal.clone();
        Box::pin(async move {
            signal.abort_with_reason(serde_json::Value::String(
                "snapshot published run failed".to_owned(),
            ));
            inner.dispose().await?;
            anyhow::bail!("snapshot published handle disposal failed")
        })
    }
}

struct SnapshotSpawnProvider {
    capabilities: SubagentCapabilities,
}

impl SnapshotSpawnProvider {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            capabilities: SubagentCapabilities {
                output_schema: true,
                depth_limit: true,
                tool_filter: true,
                persona: true,
            },
        })
    }
}

#[async_trait]
impl SubagentProvider for SnapshotSpawnProvider {
    fn name(&self) -> &str {
        "spawn"
    }

    fn capabilities(&self) -> &SubagentCapabilities {
        &self.capabilities
    }

    fn inherits_parent_context(&self) -> bool {
        false
    }

    fn supports_continuable(&self) -> bool {
        true
    }

    async fn start(
        &self,
        request: ResolvedSubagentStartRequest,
    ) -> anyhow::Result<Arc<dyn SubagentRun>> {
        let published_failure =
            std::env::var("SEEKDEEP_SUBAGENT_PUBLISHED_FAILURE").as_deref() == Ok("1");
        let signal = request.request.signal.clone();
        let run = start_in_process_run(request, InProcessRunOptions::default()).await?;
        if published_failure {
            Ok(Arc::new(PublishedFailureRun { inner: run, signal }))
        } else {
            Ok(run)
        }
    }

    async fn prepare_continuable(
        &self,
        _request: ContinuableCreateRequest,
    ) -> anyhow::Result<ContinuableCreateSpec> {
        Ok(ContinuableCreateSpec::default())
    }
}

fn coordinator_source(sender: &SessionId) -> MessageSource {
    MessageSource {
        kind: "coordinator".to_owned(),
        fields: serde_json::Map::from_iter([
            (
                "form".to_owned(),
                serde_json::Value::String("relay".to_owned()),
            ),
            (
                "senderSessionId".to_owned(),
                serde_json::to_value(sender).unwrap_or(serde_json::Value::Null),
            ),
        ]),
    }
}

pub(crate) fn subagent_durability_failure_plugin() -> Plugin {
    Plugin::new(
        "subagent-durability-failure",
        ["subagents", "tools"],
        |context, _| {
            Box::pin(async move {
                let subagents = context.get(SUBAGENTS).ok_or_else(|| {
                    anyhow::anyhow!("subagent-durability-failure requires subagents")
                })?;
                let tools = context.get(TOOLS).ok_or_else(|| {
                    anyhow::anyhow!("subagent-durability-failure requires tools")
                })?;
                let provider: Arc<dyn SubagentProvider> = SnapshotSpawnProvider::new();
                context.own(subagents.register_provider(provider)?)?;

                let state = Arc::new(DurabilityFixtureState::default());
                let cleanup_state = state.clone();
                context.own(EffectHandle::synchronous(
                    "subagent snapshot ordering",
                    move || {
                        cleanup_state.release();
                        Ok(())
                    },
                ))?;

                let inbox_state = state.clone();
                context.events().on_sync(
                    &context,
                    "agent/inbox/inserted",
                    move |_, args| {
                        let event = args
                            .get::<AgentEvent<AgentInboxMessage>>(0)
                            .ok_or_else(|| {
                                anyhow::anyhow!("agent/inbox/inserted lacks its event")
                            })?;
                        if event.agent.session().header().parent_session.is_some() {
                            inbox_state.record_child_message(event.agent.id());
                        }
                        Ok(EventReply::Undefined)
                    },
                    global_events(),
                )?;

                let session_state = state.clone();
                context.events().on_sync(
                    &context,
                    "session/event",
                    move |_, args| {
                        let session = args
                            .get::<Session>(0)
                            .ok_or_else(|| anyhow::anyhow!("session/event lacks its session"))?;
                        let event = args
                            .get::<SessionEvent>(1)
                            .ok_or_else(|| anyhow::anyhow!("session/event lacks its event"))?;
                        if session.header().parent_session.is_none()
                            && event.event_type == "turn/end"
                            && event.data.get("turn").and_then(serde_json::Value::as_u64)
                                == Some(1)
                        {
                            session_state.mark_parent_closed();
                        }
                        if session.header().parent_session.is_some()
                            && event.event_type == "turn/start"
                            && let Some(turn) =
                                event.data.get("turn").and_then(serde_json::Value::as_u64)
                        {
                            session_state
                                .child_turns
                                .lock()
                                .insert(session.id().clone(), turn);
                        }
                        if session.header().parent_session.is_some()
                            && event.event_type == "turn/end"
                        {
                            session_state.child_turns.lock().remove(session.id());
                        }
                        Ok(EventReply::Undefined)
                    },
                    global_events(),
                )?;

                let step_state = state.clone();
                let published_failure =
                    std::env::var("SEEKDEEP_SUBAGENT_PUBLISHED_FAILURE").as_deref() == Ok("1");
                context.events().on_waterfall(
                    &context,
                    "agent/pre-step",
                    move |_, args, next| {
                        let state = step_state.clone();
                        Box::pin(async move {
                            let event = args
                                .get::<AgentEvent<AgentPreStepEvent>>(0)
                                .ok_or_else(|| anyhow::anyhow!("agent/pre-step lacks its event"))?;
                            if event.agent.session().header().parent_session.is_some() {
                                state.wait_followups().await;
                                if !published_failure {
                                    state.wait_parent_closed().await;
                                }
                            }
                            next.run().await
                        })
                    },
                    global_events(),
                )?;

                let flush_state = state.clone();
                context.events().on(
                    &context,
                    "session/flush",
                    move |_, args| {
                        let state = flush_state.clone();
                        Box::pin(async move {
                            let session = args.get::<Session>(0).ok_or_else(|| {
                                anyhow::anyhow!("session/flush lacks its session")
                            })?;
                            if session.header().parent_session.is_some()
                                && state.child_turns.lock().get(session.id()).copied()
                                    == Some(FAILED_CHECKPOINT_TURN)
                            {
                                anyhow::bail!("snapshot disk full")
                            }
                            Ok(EventReply::Undefined)
                        })
                    },
                    global_events(),
                )?;

                let dispatch_state = state.clone();
                tools.on_execute(
                    &context,
                    move |execution, next| {
                        let state = dispatch_state.clone();
                        let subagents = subagents.clone();
                        async move {
                            if execution.name != "send_message" {
                                return next.run().await;
                            }
                            let child_id = execution
                                .arguments
                                .get("subagent_id")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default();
                            if child_id == UNKNOWN_CHILD_ID {
                                state.wait_followups().await;
                                return next.run().await;
                            }
                            let Some(mapped) = state.mapped_child_id(child_id) else {
                                return next.run().await;
                            };
                            let parent = execution.agent.clone().ok_or_else(|| {
                                anyhow::anyhow!("send_message requires a calling agent")
                            })?;
                            let message = execution
                                .arguments
                                .get("message")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default()
                                .to_owned();
                            let message_id = subagents
                                .followup(
                                    &parent,
                                    &mapped,
                                    vec![ContentBlock::Text { text: message }],
                                    SubagentFollowupOptions {
                                        source: coordinator_source(parent.id()),
                                        signal: execution.signal(),
                                    },
                                )
                                .await?;
                            Ok(ToolExecutionResult::success(
                                serde_json::json!({"messageId": message_id}),
                                Vec::new(),
                            ))
                        }
                    },
                    global_events(),
                )?;
                Ok(())
            })
        },
    )
}

const fn global_events() -> EventOptions {
    EventOptions {
        global: true,
        prepend: false,
    }
}
