//! Command registration, execution, lifecycle, cancellation, and drain parity.

use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_agent::{
    Agent, AgentCancelCause, AgentControlError, AgentController, AgentOptions, CancelOptions,
    Inbox, InboxTarget, MaintenanceReservation, NoopInboxNotifications,
};
use seekdeep_commands::{CommandExecution, CommandResult, CommandRuntime};
use seekdeep_compaction::service::{
    CompactionAgentContext, CompactionEngine, CompactionService, CompactionTrigger,
    ManualCompactAgentContext, ManualCompactionError, ManualCompactionErrorCode,
};
use seekdeep_compaction::{CompactionId, CompactionResult, ShadowedRange};
use seekdeep_cordis::{Context, PluginFiber};
use seekdeep_core::session::{AppendOptions, Session, SessionId};
use seekdeep_llm::{AbortSignal, ContentBlock, UserMessage};
use seekdeep_scope::ScopeKey;
use serde_json::{Value, json};

type CompactOperation = Arc<
    dyn Fn(
            ManualCompactAgentContext,
            AbortSignal,
            Option<seekdeep_commands::CommandId>,
        ) -> BoxFuture<'static, anyhow::Result<Option<CompactionResult>>>
        + Send
        + Sync,
>;
type CompactCall = (
    Arc<Session>,
    AbortSignal,
    Option<seekdeep_commands::CommandId>,
);

#[derive(Default)]
struct IdleController;

impl AgentController for IdleController {
    fn send(
        &self,
        _message: UserMessage,
        _target: InboxTarget,
        _wakeup: bool,
    ) -> Result<(), AgentControlError> {
        Ok(())
    }

    fn cancel(
        &self,
        _cause: AgentCancelCause,
        _options: CancelOptions,
    ) -> Result<(), AgentControlError> {
        Ok(())
    }

    fn when_idle(&self) -> BoxFuture<'static, ()> {
        Box::pin(async {})
    }

    fn begin_maintenance(&self) -> Result<MaintenanceReservation, AgentControlError> {
        Ok(MaintenanceReservation::new(
            AbortSignal::default(),
            Arc::new(|| {}),
        ))
    }
}

struct StubEngine {
    result: Mutex<Option<CompactionResult>>,
    failure: Mutex<Option<(ManualCompactionErrorCode, String)>>,
    operation: Mutex<Option<CompactOperation>>,
    calls: Mutex<Vec<CompactCall>>,
}

impl StubEngine {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            result: Mutex::new(Some(compaction_result())),
            failure: Mutex::new(None),
            operation: Mutex::new(None),
            calls: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl CompactionEngine for StubEngine {
    async fn compact_if_needed(
        &self,
        _agent: &CompactionAgentContext,
        _trigger: CompactionTrigger,
        _signal: &AbortSignal,
    ) -> anyhow::Result<Option<CompactionResult>> {
        Ok(None)
    }

    async fn compact_now(
        &self,
        agent: &ManualCompactAgentContext,
        signal: &AbortSignal,
        source_command_id: Option<&seekdeep_commands::CommandId>,
    ) -> anyhow::Result<Option<CompactionResult>> {
        self.calls.lock().push((
            agent.session.clone(),
            signal.clone(),
            source_command_id.cloned(),
        ));
        let operation = self.operation.lock().clone();
        if let Some(operation) = operation {
            return operation(agent.clone(), signal.clone(), source_command_id.cloned()).await;
        }
        if let Some((code, message)) = self.failure.lock().clone() {
            return Err(ManualCompactionError::new(code, message).into());
        }
        let Some(mut result) = self.result.lock().clone() else {
            return Ok(None);
        };
        append_result(agent, &mut result, source_command_id)?;
        Ok(Some(result))
    }

    async fn compact_region(
        &self,
        _start: u64,
        _end: u64,
        _agent: &CompactionAgentContext,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<CompactionResult> {
        Ok(compaction_result())
    }
}

fn compaction_result() -> CompactionResult {
    CompactionResult {
        compaction_id: CompactionId::new("command-compact-test"),
        source_command_id: None,
        start_seq: 1,
        summary_seq: 2,
        end_seq: 3,
        summary: vec![ContentBlock::Text {
            text: "summary".to_owned(),
        }],
        shadowed_range: ShadowedRange { start: 1, end: 7 },
        shadowed_seqs: vec![1, 3, 7],
        shadowed_token_count: 42,
    }
}

fn append_result(
    agent: &ManualCompactAgentContext,
    result: &mut CompactionResult,
    source_command_id: Option<&seekdeep_commands::CommandId>,
) -> anyhow::Result<()> {
    result.source_command_id = source_command_id.cloned();
    let provenance = json!({
        "compactionId": result.compaction_id,
        "sourceCommandId": source_command_id,
        "turn": Value::Null,
    });
    agent
        .session
        .append("compaction/start", provenance, AppendOptions::default())?;
    agent.session.append(
        "compaction/summary",
        json!({
            "compactionId": result.compaction_id,
            "sourceCommandId": source_command_id,
            "summary": result.summary,
            "shadowedRange": result.shadowed_range,
            "shadowedSeqs": result.shadowed_seqs,
            "shadowedTokenCount": result.shadowed_token_count,
            "provider": "command-test",
            "model": "command-test",
        }),
        AppendOptions::default(),
    )?;
    agent.session.append(
        "compaction/end",
        json!({
            "compactionId": result.compaction_id,
            "sourceCommandId": source_command_id,
            "turn": Value::Null,
        }),
        AppendOptions::default(),
    )?;
    Ok(())
}

struct Harness {
    commands: Arc<CommandRuntime>,
    engine: Arc<StubEngine>,
    agent: Arc<Agent>,
    plugin: Arc<PluginFiber>,
}

fn build_agent(id: &str) -> anyhow::Result<Arc<Agent>> {
    let session = Session::create(&SessionId::new(id), None, None)?;
    let inbox = Arc::new(Inbox::new(
        session.clone(),
        Arc::new(NoopInboxNotifications),
    )?);
    let agent = Arc::new(Agent::new(
        session.id().clone(),
        AgentOptions::default(),
        session,
        inbox,
        Context::new(),
        ScopeKey::new(),
    ));
    agent.install_controller(Arc::new(IdleController))?;
    Ok(agent)
}

impl Harness {
    async fn new() -> anyhow::Result<Self> {
        let context = Context::new();
        let commands = seekdeep_commands::install(&context)?;
        let engine = StubEngine::new();
        CompactionService::new(engine.clone()).provide(&context)?;
        let plugin = context.plugin(
            seekdeep_command_compact::index::plugin(),
            serde_json::Value::Null,
        )?;
        plugin.await_settled().await?;
        let agent = build_agent("command-compact")?;
        Ok(Self {
            commands,
            engine,
            agent,
            plugin,
        })
    }

    async fn run(&self, suffix: &str, signal: AbortSignal) -> anyhow::Result<CommandExecution> {
        self.commands
            .execute(self.agent.clone(), &format!("/compact{suffix}"), signal)
            .await?
            .ok_or_else(|| anyhow::anyhow!("compact command missing"))
    }
}

fn assert_lifecycle(harness: &Harness, raw_input: &str, result: &CommandResult) {
    let events = harness.agent.session().events();
    let lifecycle = events
        .iter()
        .filter(|event| matches!(event.event_type.as_str(), "command/run" | "command/done"))
        .rev()
        .take(2)
        .collect::<Vec<_>>();
    let done = lifecycle[0];
    let run = lifecycle[1];
    assert_eq!(run.event_type, "command/run");
    assert_eq!(done.event_type, "command/done");
    assert_eq!(run.data["name"], "compact");
    assert_eq!(run.data["args"], raw_input);
    assert_eq!(run.data["source"], json!({"kind":"user"}));
    assert_eq!(done.data["commandId"], run.data["commandId"]);
    assert_eq!(done.data["kind"], result.kind());
    assert_eq!(harness.agent.session().surface_nodes(), Vec::<u64>::new());
    assert_eq!(harness.agent.session().derive_messages(), Vec::new());
}

#[tokio::test]
async fn registers_one_argument_free_command_and_disposes_it() -> anyhow::Result<()> {
    let harness = Harness::new().await?;
    assert_eq!(seekdeep_command_compact::index::NAME, "command-compact");
    assert_eq!(
        seekdeep_command_compact::index::INJECT,
        ["commands", "compaction"]
    );
    assert_eq!(
        harness.commands.list(&harness.agent),
        [seekdeep_commands::CommandDescriptor {
            name: "compact".to_owned(),
            description: "Compact older conversation history".to_owned(),
            input: None,
        }]
    );
    harness.plugin.dispose().await?;
    assert!(harness.commands.find(&harness.agent, "compact").is_none());
    Ok(())
}

#[tokio::test]
async fn success_reports_accounting_and_forwards_exact_agent_signal_and_command_id()
-> anyhow::Result<()> {
    let harness = Harness::new().await?;
    let signal = AbortSignal::default();
    let execution = harness.run("", signal.clone()).await?;
    assert_eq!(
        execution.result,
        CommandResult::success_linked(Some("Compacted 3 history items (~42 tokens)."), 2,)
    );
    assert_lifecycle(&harness, "", &execution.result);
    let calls = harness.engine.calls.lock();
    assert_eq!(calls.len(), 1);
    assert!(Arc::ptr_eq(&calls[0].0, harness.agent.session()));
    assert_eq!(calls[0].2.as_ref(), Some(&execution.command_id));
    signal.abort();
    assert!(calls[0].1.is_aborted());
    Ok(())
}

#[tokio::test]
async fn no_history_and_arguments_return_direct_results_without_extra_backend_calls()
-> anyhow::Result<()> {
    let harness = Harness::new().await?;
    *harness.engine.result.lock() = None;
    let empty = harness.run("", AbortSignal::default()).await?;
    assert_eq!(
        empty.result,
        CommandResult::success(Some("No compactable history yet."))
    );
    assert_lifecycle(&harness, "", &empty.result);
    let rejected = harness.run(" now", AbortSignal::default()).await?;
    assert_eq!(
        rejected.result,
        CommandResult::error("Usage: /compact (no arguments)")
    );
    assert_lifecycle(&harness, " now", &rejected.result);
    assert_eq!(harness.engine.calls.lock().len(), 1);
    Ok(())
}

#[tokio::test]
async fn every_expected_failure_maps_to_the_exact_direct_error() -> anyhow::Result<()> {
    for (code, expected) in [
        (
            ManualCompactionErrorCode::Busy,
            "Compaction is unavailable because this process has an active compaction, or the agent is not idle.",
        ),
        (
            ManualCompactionErrorCode::Cancelled,
            "Compaction cancelled.",
        ),
        (
            ManualCompactionErrorCode::Changed,
            "The history selected for compaction changed before it could be replaced. The conversation is unchanged; the attempt is recorded in the session log.",
        ),
        (
            ManualCompactionErrorCode::Summary,
            "Compaction could not produce a useful summary. The conversation is unchanged; the attempt is recorded in the session log.",
        ),
        (
            ManualCompactionErrorCode::Commit,
            "Compaction did not finish cleanly; some session history may have changed. Inspect the current session state before retrying.",
        ),
        (
            ManualCompactionErrorCode::Persistence,
            "Compaction finished, but the session could not be saved.",
        ),
    ] {
        let harness = Harness::new().await?;
        *harness.engine.failure.lock() = Some((code, "backend detail".to_owned()));
        let execution = harness.run("", AbortSignal::default()).await?;
        assert_eq!(execution.result, CommandResult::error(expected));
        assert_lifecycle(&harness, "", &execution.result);
    }
    Ok(())
}

#[tokio::test]
async fn caller_cancellation_and_unexpected_failures_propagate_and_pair_done_events()
-> anyhow::Result<()> {
    let cancelled = Harness::new().await?;
    let signal = AbortSignal::default();
    *cancelled.engine.operation.lock() = Some(Arc::new(
        |_: ManualCompactAgentContext,
         signal: AbortSignal,
         _: Option<seekdeep_commands::CommandId>| {
            Box::pin(async move {
                signal.abort_with_error(
                    Arc::new(std::io::Error::other("operator cancelled")),
                    json!("operator cancelled"),
                );
                tokio::task::yield_now().await;
                Err(
                    ManualCompactionError::new(ManualCompactionErrorCode::Summary, "late failure")
                        .into(),
                )
            })
        },
    ));
    let error = cancelled.run("", signal).await.unwrap_err();
    assert!(error.to_string().contains("operator cancelled"));
    assert_lifecycle(&cancelled, "", &CommandResult::error("operator cancelled"));

    let unexpected = Harness::new().await?;
    *unexpected.engine.operation.lock() = Some(Arc::new(|_, _, _| {
        Box::pin(async { anyhow::bail!("unexpected backend bug") })
    }));
    let error = unexpected
        .run("", AbortSignal::default())
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "unexpected backend bug");
    assert_lifecycle(
        &unexpected,
        "",
        &CommandResult::error("unexpected backend bug"),
    );
    Ok(())
}

#[tokio::test]
async fn disposal_unregisters_immediately_then_drains_aborted_handler_through_flush()
-> anyhow::Result<()> {
    let harness = Harness::new().await?;
    let (started_send, started) = tokio::sync::oneshot::channel();
    let (allow_close_send, allow_close) = tokio::sync::oneshot::channel();
    let (closed_notice_send, closed_notice) = tokio::sync::oneshot::channel();
    let (allow_flush_send, allow_flush) = tokio::sync::oneshot::channel();
    let (flushed_notice_send, flushed_notice) = tokio::sync::oneshot::channel();
    let gates = Arc::new(Mutex::new(Some((
        started_send,
        allow_close,
        closed_notice_send,
        allow_flush,
        flushed_notice_send,
    ))));
    *harness.engine.operation.lock() = Some(Arc::new({
        let gates = gates.clone();
        move |_, _, _| {
            let (started, allow_close, closed_notice, allow_flush, flushed_notice) =
                gates.lock().take().expect("operation once");
            Box::pin(async move {
                started.send(()).ok();
                allow_close.await?;
                closed_notice.send(()).ok();
                allow_flush.await?;
                flushed_notice.send(()).ok();
                anyhow::bail!("operator cancelled")
            })
        }
    }));

    let signal = AbortSignal::default();
    let execution = tokio::spawn({
        let commands = harness.commands.clone();
        let agent = harness.agent.clone();
        let signal = signal.clone();
        async move { commands.execute(agent, "/compact", signal).await }
    });
    started.await?;
    signal.abort_with_error(
        Arc::new(std::io::Error::other("operator cancelled")),
        json!("operator cancelled"),
    );
    assert!(
        execution
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("operator cancelled")
    );

    let disposal = tokio::spawn({
        let plugin = harness.plugin.clone();
        async move { plugin.dispose().await }
    });
    tokio::task::yield_now().await;
    assert!(harness.commands.find(&harness.agent, "compact").is_none());
    assert!(!disposal.is_finished());
    allow_close_send.send(()).ok();
    closed_notice.await?;
    tokio::task::yield_now().await;
    assert!(!disposal.is_finished());
    allow_flush_send.send(()).ok();
    flushed_notice.await?;
    disposal.await.unwrap()?;
    Ok(())
}

#[tokio::test]
async fn declarative_loader_discovers_and_executes_compact_through_command_plane()
-> anyhow::Result<()> {
    let context = Context::new();
    let engine = StubEngine::new();
    let catalog = seekdeep_loader::PluginCatalog::new();
    catalog.register_named(
        "commands",
        seekdeep_cordis::Plugin::new("commands", std::iter::empty::<&str>(), |context, _| {
            Box::pin(async move {
                seekdeep_commands::install(&context)?;
                Ok(())
            })
        }),
    )?;
    catalog.register_named(
        "compaction",
        seekdeep_cordis::Plugin::new("compaction", std::iter::empty::<&str>(), {
            let engine = engine.clone();
            move |context, _| {
                let engine = engine.clone();
                Box::pin(async move {
                    CompactionService::new(engine).provide(&context)?;
                    Ok(())
                })
            }
        }),
    )?;
    catalog.register_named("command-compact", seekdeep_command_compact::index::plugin())?;
    let composition = catalog
        .load_yaml(
            &context,
            "- id: commands\n  name: commands\n- id: compaction\n  name: compaction\n- id: command\n  name: command-compact\n",
        )
        .await?;
    let commands = context
        .get(seekdeep_commands::COMMANDS)
        .ok_or_else(|| anyhow::anyhow!("commands missing"))?;
    let agent = build_agent("loader-command-compact")?;
    assert!(
        commands
            .list(&agent)
            .iter()
            .any(|command| command.name == "compact")
    );
    let execution = commands
        .execute(agent.clone(), "/compact", AbortSignal::default())
        .await?
        .ok_or_else(|| anyhow::anyhow!("compact command missing"))?;
    assert_eq!(
        execution.result,
        CommandResult::success_linked(Some("Compacted 3 history items (~42 tokens)."), 2,)
    );
    assert_eq!(
        agent
            .session()
            .events()
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        [
            "command/run",
            "compaction/start",
            "compaction/summary",
            "compaction/end",
            "command/done",
        ]
    );
    composition.dispose().await?;
    context.fiber().dispose().await
}
