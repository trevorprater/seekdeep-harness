//! Exact production, failure-containment, and scheduling parity for tmux context.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
};

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_agent::{
    Agent, AgentEvents, AgentOptions, Inbox, NoopInboxNotifications, PreStepDecision,
};
use seekdeep_agent_loop::AgentPreStepEvent;
use seekdeep_cordis::Context;
use seekdeep_core::session::{AppendOptions, Session, SessionId, SurfaceOp};
use seekdeep_llm::{AbortSignal, ContentBlock, MessageSource, UserMessage};
use seekdeep_scope::ScopeKey;
use seekdeep_shell::{
    CollectedOutput, ShellExecRequest, ShellExecSpec, ShellExecutor, ShellProcessHandle,
    ShellRunResult, ShellService,
};
use seekdeep_tmux_context::{TmuxContextConfig, apply_with_environment};
use serde_json::json;

const PID: u32 = 42_424;
const LAYOUT: &str =
    "d517,270x71,0,0{135x71,0,0,87,134x71,136,0[134x35,136,0,90,134x35,136,36,93]}";

fn tmux_line(window_name: &str, pane_id: &str) -> String {
    ["0", "1", window_name, "2", pane_id, "1", "0", LAYOUT].join("\\t")
}

fn result(stdout: impl Into<String>, exit_code: i32) -> ShellRunResult {
    ShellRunResult {
        exit_code: Some(exit_code),
        signal: None,
        timed_out: false,
        aborted: false,
        timeout_ms: 60_000,
        stdout: CollectedOutput {
            text: stdout.into(),
            truncated: false,
            spill_path: None,
        },
        stderr: CollectedOutput::default(),
        sandbox: None,
    }
}

#[derive(Debug)]
struct FakeShell {
    commands: Mutex<Vec<String>>,
    result: Mutex<ShellRunResult>,
    resolve_error: Mutex<Option<String>>,
    run_error: Mutex<Option<String>>,
}

impl FakeShell {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            commands: Mutex::new(Vec::new()),
            result: Mutex::new(result(format!("{}\n", tmux_line("node", "%90")), 0)),
            resolve_error: Mutex::new(None),
            run_error: Mutex::new(None),
        })
    }
}

#[async_trait]
impl ShellExecutor for FakeShell {
    fn resolve(&self, request: ShellExecRequest) -> anyhow::Result<ShellExecSpec> {
        if let Some(error) = self.resolve_error.lock().clone() {
            anyhow::bail!(error);
        }
        Ok(ShellExecSpec {
            command: request.command,
            workdir: request.workdir.unwrap_or_else(|| PathBuf::from("/work")),
            timeout_ms: request.timeout_ms.unwrap_or(60_000),
            stdout_max_bytes: request.stdout_max_bytes.unwrap_or(64_000),
            signal: request.signal,
            stdin: request.stdin,
            env: request.env,
            seekdeep_env: request.seekdeep_env,
            sandbox_policy: request.sandbox_policy,
        })
    }

    async fn run(&self, spec: ShellExecSpec) -> anyhow::Result<ShellRunResult> {
        self.commands.lock().push(spec.command);
        if let Some(error) = self.run_error.lock().clone() {
            anyhow::bail!(error);
        }
        Ok(self.result.lock().clone())
    }

    fn start(&self, _spec: ShellExecSpec) -> anyhow::Result<ShellProcessHandle> {
        anyhow::bail!("tmux-context must never start a background job")
    }
}

struct Harness {
    context: Context,
    shell: Option<Arc<FakeShell>>,
    warnings: Arc<Mutex<Vec<String>>>,
    now: Arc<AtomicI64>,
}

fn mount(config: &TmuxContextConfig, with_shell: bool) -> Harness {
    let context = Context::new();
    let shell = with_shell.then(FakeShell::new);
    if let Some(shell) = &shell {
        ShellService::new(shell.clone())
            .provide(&context)
            .expect("shell service");
    }
    let warnings = Arc::new(Mutex::new(Vec::new()));
    let warning_sink = warnings.clone();
    let now = Arc::new(AtomicI64::new(1_000));
    apply_with_environment(
        &context,
        config,
        {
            let now = now.clone();
            Arc::new(move || now.load(Ordering::Acquire))
        },
        PID,
        Arc::new(move |warning| warning_sink.lock().push(warning)),
    )
    .expect("tmux context");
    Harness {
        context,
        shell,
        warnings,
        now,
    }
}

fn user(text: &str) -> UserMessage {
    UserMessage::new(
        vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        MessageSource::user(),
    )
}

fn new_session(id: &str) -> Arc<Session> {
    Session::create(&SessionId::new(id), None, None).expect("session")
}

fn open_turn(session: &Session, turn: u64) {
    session
        .append(
            "turn/start",
            json!({"turn": turn}),
            AppendOptions::default(),
        )
        .expect("turn");
    session
        .append(
            "user/message",
            serde_json::to_value(user(&format!("turn {turn}"))).expect("message"),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .expect("message");
}

fn agent(context: &Context, session: Arc<Session>, id: &str) -> Arc<Agent> {
    let inbox =
        Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).expect("inbox"));
    Arc::new(Agent::new(
        SessionId::new(id),
        AgentOptions::default(),
        session,
        inbox,
        context.clone(),
        ScopeKey::new(),
    ))
}

async fn fire(
    context: &Context,
    subject: &Arc<Agent>,
    turn: u64,
    step: u64,
    signal: AbortSignal,
) -> anyhow::Result<()> {
    let decision = AgentEvents::new(context.clone(), subject.clone())
        .waterfall(
            "agent/pre-step",
            AgentPreStepEvent {
                messages: Vec::new(),
                turn,
                step,
                signal,
            },
            || async {
                Ok(PreStepDecision::Enter {
                    messages: Vec::new(),
                })
            },
        )
        .await?;
    if let PreStepDecision::Enter { messages } = decision {
        for message in messages {
            subject.session().append(
                "user/message",
                serde_json::to_value(message)?,
                AppendOptions {
                    surface_op: Some(SurfaceOp::append()),
                    ..AppendOptions::default()
                },
            )?;
        }
    }
    Ok(())
}

fn context_events(session: &Session) -> Vec<seekdeep_core::session::SessionEvent> {
    session
        .events()
        .into_iter()
        .filter(|event| {
            event.event_type == "user/message"
                && event.data["source"]["kind"] == "plugin"
                && event.data["source"]["plugin"] == "tmux-context"
        })
        .collect()
}

fn context_texts(session: &Session) -> Vec<String> {
    context_events(session)
        .into_iter()
        .filter_map(|event| event.data["content"][0]["text"].as_str().map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn injects_exact_snapshot_and_queries_the_real_process_pane_tty() {
    let harness = mount(&TmuxContextConfig::default(), true);
    let session = new_session("first");
    open_turn(&session, 1);
    fire(
        &harness.context,
        &agent(&harness.context, session.clone(), "first"),
        1,
        1,
        AbortSignal::default(),
    )
    .await
    .expect("fire");
    let expected = format!(
        "tmux location (turn 1):\nsession 0, window 1 \"node\", pane 2 %90\nwindow active=1, pane active=0, layout {LAYOUT}"
    );
    assert_eq!(context_texts(&session), [expected.as_str()]);
    let event = context_events(&session).pop().expect("context");
    assert_eq!(event.surface_op, Some(SurfaceOp::append()));
    assert_eq!(event.data["source"]["form"], "snapshot");
    assert_eq!(event.data["source"]["sections"][0]["name"], "tmux-context");
    assert_eq!(event.data["source"]["sections"][0]["text"], expected);

    let commands = harness.shell.expect("shell").commands.lock().clone();
    assert_eq!(commands.len(), 1);
    assert!(commands[0].contains("[ -n \"$TMUX_PANE\" ]"));
    assert!(commands[0].contains("tmux display-message -t \"$TMUX_PANE\" -p"));
    assert!(commands[0].contains(&format!("ps -o tty= -p {PID}")));
    assert!(commands[0].contains("#{pane_tty}"));
    assert!(commands[0].contains("[ \"$pane_tty\" = \"/dev/$self_tty\" ]"));
}

#[tokio::test]
async fn later_steps_rejections_and_aborts_do_not_query() {
    let harness = mount(&TmuxContextConfig::default(), true);
    let session = new_session("guards");
    open_turn(&session, 1);
    let subject = agent(&harness.context, session.clone(), "guards");
    fire(&harness.context, &subject, 1, 2, AbortSignal::default())
        .await
        .expect("later step");
    let aborted = AbortSignal::default();
    aborted.abort();
    fire(&harness.context, &subject, 1, 1, aborted)
        .await
        .expect("aborted");
    assert!(
        harness
            .shell
            .as_ref()
            .expect("shell")
            .commands
            .lock()
            .is_empty()
    );
    assert!(context_texts(&session).is_empty());
    fire(&harness.context, &subject, 1, 1, AbortSignal::default())
        .await
        .expect("next eligible attempt");
    assert_eq!(context_texts(&session).len(), 1);

    let no_shell = mount(&TmuxContextConfig::default(), false);
    let no_shell_session = new_session("no-shell");
    open_turn(&no_shell_session, 1);
    fire(
        &no_shell.context,
        &agent(&no_shell.context, no_shell_session.clone(), "no-shell"),
        1,
        1,
        AbortSignal::default(),
    )
    .await
    .expect("no shell");
    assert!(context_texts(&no_shell_session).is_empty());
}

#[tokio::test]
async fn only_changed_state_reinjects_on_a_new_turn() {
    let harness = mount(&TmuxContextConfig::default(), true);
    let shell = harness.shell.clone().expect("shell");
    let session = new_session("change");
    let subject = agent(&harness.context, session.clone(), "change");
    open_turn(&session, 1);
    fire(&harness.context, &subject, 1, 1, AbortSignal::default())
        .await
        .expect("first");
    session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("end");
    open_turn(&session, 2);
    fire(&harness.context, &subject, 2, 1, AbortSignal::default())
        .await
        .expect("same");
    assert_eq!(context_texts(&session).len(), 1);
    session
        .append(
            "turn/end",
            json!({"turn": 2, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("end");
    *shell.result.lock() = result(format!("{}\n", tmux_line("shell", "%12")), 0);
    open_turn(&session, 3);
    fire(&harness.context, &subject, 3, 1, AbortSignal::default())
        .await
        .expect("changed");
    let texts = context_texts(&session);
    assert_eq!(texts.len(), 2);
    assert!(texts[1].contains("tmux location (turn 3):"));
    assert!(texts[1].contains("window 1 \"shell\", pane 2 %12"));
}

#[tokio::test]
async fn positive_interval_suppresses_queries_until_the_exact_threshold() {
    let harness = mount(
        &TmuxContextConfig {
            refresh_interval_ms: Some(10_000.0),
        },
        true,
    );
    let shell = harness.shell.clone().expect("shell");
    let session = new_session("interval");
    let subject = agent(&harness.context, session.clone(), "interval");
    open_turn(&session, 1);
    fire(&harness.context, &subject, 1, 1, AbortSignal::default())
        .await
        .expect("first");
    let last = context_events(&session)[0].time;
    *shell.result.lock() = result(format!("{}\n", tmux_line("node", "%99")), 0);
    harness.now.store(last + 9_999, Ordering::Release);
    open_turn(&session, 2);
    fire(&harness.context, &subject, 2, 1, AbortSignal::default())
        .await
        .expect("inside interval");
    assert_eq!(shell.commands.lock().len(), 1);
    assert_eq!(context_texts(&session).len(), 1);

    harness.now.store(last + 10_000, Ordering::Release);
    open_turn(&session, 3);
    fire(&harness.context, &subject, 3, 1, AbortSignal::default())
        .await
        .expect("threshold");
    assert_eq!(shell.commands.lock().len(), 2);
    assert_eq!(context_texts(&session).len(), 2);
}

#[tokio::test]
async fn shadowed_reading_still_schedules_a_resumed_session_from_the_raw_log() {
    let harness = mount(
        &TmuxContextConfig {
            refresh_interval_ms: Some(1_000.0),
        },
        true,
    );
    let original = new_session("shadow-source");
    open_turn(&original, 1);
    fire(
        &harness.context,
        &agent(&harness.context, original.clone(), "shadow-source"),
        1,
        1,
        AbortSignal::default(),
    )
    .await
    .expect("first reading");
    let events = original.events();
    let user_seq = events
        .iter()
        .find(|event| event.event_type == "user/message" && event.data["source"]["kind"] == "user")
        .expect("user event")
        .seq;
    let reading = events
        .iter()
        .find(|event| {
            event.event_type == "user/message" && event.data["source"]["plugin"] == "tmux-context"
        })
        .expect("tmux reading");
    original
        .append(
            "user/message",
            serde_json::to_value(UserMessage::new(
                vec![ContentBlock::Text {
                    text: "compacted history".to_owned(),
                }],
                MessageSource::plugin("compaction-basic"),
            ))
            .expect("compaction message"),
            AppendOptions {
                surface_op: Some(SurfaceOp::replace(user_seq, reading.seq)),
                source_event_seqs: Some(vec![user_seq, reading.seq]),
                ..AppendOptions::default()
            },
        )
        .expect("shadow history");
    assert!(
        original
            .derive_messages()
            .iter()
            .all(|message| { message.source().fields["plugin"] != "tmux-context" })
    );

    let resumed = Session::create(
        &SessionId::new("shadow-resumed"),
        Some(original.events()),
        None,
    )
    .expect("resume");
    open_turn(&resumed, 2);
    harness.now.store(reading.time + 999, Ordering::Release);
    fire(
        &harness.context,
        &agent(&harness.context, resumed.clone(), "shadow-resumed"),
        2,
        1,
        AbortSignal::default(),
    )
    .await
    .expect("within interval");
    assert_eq!(
        harness.shell.as_ref().expect("shell").commands.lock().len(),
        1
    );

    harness.now.store(reading.time + 1_000, Ordering::Release);
    fire(
        &harness.context,
        &agent(&harness.context, resumed, "shadow-resumed"),
        3,
        1,
        AbortSignal::default(),
    )
    .await
    .expect("at interval");
    assert_eq!(harness.shell.expect("shell").commands.lock().len(), 2);
}

#[tokio::test]
async fn malformed_prior_readings_are_treated_as_absent() {
    for content in [
        ContentBlock::Reasoning {
            text: "not a location".to_owned(),
        },
        ContentBlock::Text {
            text: "single line, no newline".to_owned(),
        },
    ] {
        let harness = mount(&TmuxContextConfig::default(), true);
        let session = new_session("prior-malformed");
        open_turn(&session, 1);
        session
            .append(
                "user/message",
                serde_json::to_value(UserMessage::new(
                    vec![content],
                    MessageSource::plugin("tmux-context"),
                ))
                .expect("prior"),
                AppendOptions {
                    surface_op: Some(SurfaceOp::append()),
                    ..AppendOptions::default()
                },
            )
            .expect("prior");
        fire(
            &harness.context,
            &agent(&harness.context, session.clone(), "prior-malformed"),
            1,
            1,
            AbortSignal::default(),
        )
        .await
        .expect("fresh injection");
        assert_eq!(harness.shell.expect("shell").commands.lock().len(), 1);
        assert!(
            context_texts(&session)
                .last()
                .is_some_and(|text| text.contains("turn 1"))
        );
    }
}

#[tokio::test]
async fn nonzero_and_malformed_query_results_are_silent_noops() {
    for output in [
        result("", 1),
        result("0\\t1\\tnode\n", 0),
        result(format!("{}\n", tmux_line("node", "")), 0),
    ] {
        let harness = mount(&TmuxContextConfig::default(), true);
        *harness.shell.as_ref().expect("shell").result.lock() = output;
        let session = new_session("bad-result");
        open_turn(&session, 1);
        fire(
            &harness.context,
            &agent(&harness.context, session.clone(), "bad-result"),
            1,
            1,
            AbortSignal::default(),
        )
        .await
        .expect("contained result");
        assert!(context_texts(&session).is_empty());
        assert!(harness.warnings.lock().is_empty());
    }
}

#[tokio::test]
async fn resolve_and_run_rejections_warn_and_do_not_fail_the_turn() {
    for (resolve, diagnostic) in [
        (true, "command denied by policy"),
        (false, "bash executor unavailable"),
    ] {
        let harness = mount(&TmuxContextConfig::default(), true);
        let shell = harness.shell.as_ref().expect("shell");
        if resolve {
            *shell.resolve_error.lock() = Some(diagnostic.to_owned());
        } else {
            *shell.run_error.lock() = Some(diagnostic.to_owned());
        }
        let session = new_session("rejected");
        open_turn(&session, 1);
        fire(
            &harness.context,
            &agent(&harness.context, session.clone(), "rejected"),
            1,
            1,
            AbortSignal::default(),
        )
        .await
        .expect("query failure contained");
        assert!(context_texts(&session).is_empty());
        let warning = harness.warnings.lock().pop().expect("warning");
        assert!(warning.contains(diagnostic));
        assert!(warning.contains("injecting no location this turn"));
    }
}

#[tokio::test]
async fn downstream_rejection_or_failure_prevents_query_and_injection() {
    let rejecting = mount(&TmuxContextConfig::default(), true);
    rejecting
        .context
        .events()
        .on_waterfall(
            &rejecting.context,
            "agent/pre-step",
            |_, _, _| {
                Box::pin(async {
                    Ok(seekdeep_cordis::EventReply::Value(Arc::new(
                        PreStepDecision::Reject,
                    )))
                })
            },
            seekdeep_cordis::EventOptions::default(),
        )
        .expect("reject listener");
    let rejected_session = new_session("downstream-reject");
    open_turn(&rejected_session, 1);
    fire(
        &rejecting.context,
        &agent(
            &rejecting.context,
            rejected_session.clone(),
            "downstream-reject",
        ),
        1,
        1,
        AbortSignal::default(),
    )
    .await
    .expect("rejection is a decision");
    assert!(context_texts(&rejected_session).is_empty());
    assert!(rejecting.shell.expect("shell").commands.lock().is_empty());

    let failing = mount(&TmuxContextConfig::default(), true);
    failing
        .context
        .events()
        .on_waterfall(
            &failing.context,
            "agent/pre-step",
            |_, _, _| Box::pin(async { Err(anyhow::anyhow!("later pre-step failure")) }),
            seekdeep_cordis::EventOptions::default(),
        )
        .expect("failure listener");
    let failed_session = new_session("downstream-failure");
    open_turn(&failed_session, 1);
    let error = fire(
        &failing.context,
        &agent(
            &failing.context,
            failed_session.clone(),
            "downstream-failure",
        ),
        1,
        1,
        AbortSignal::default(),
    )
    .await
    .expect_err("failure propagates");
    assert!(format!("{error:#}").contains("later pre-step failure"));
    assert!(context_texts(&failed_session).is_empty());
    assert!(failing.shell.expect("shell").commands.lock().is_empty());
}

#[tokio::test]
async fn invariant_companion_reserves_and_releases_package_ownership() {
    let context = Context::new();
    let registry = seekdeep_invariants::InvariantRegistry::install(
        &context,
        &seekdeep_invariants::InvariantConfig::default(),
    )
    .expect("registry");
    let registration =
        seekdeep_tmux_context::invariant::register_invariant(&registry).expect("registration");
    registration.await_ready().await.expect("ready");
    assert!(registry.is_registered("seekdeep-tmux-context"));
    registration.dispose().await.expect("dispose");
    assert!(!registry.is_registered("seekdeep-tmux-context"));
}

#[tokio::test]
async fn plugin_metadata_config_validation_and_listener_disposal_are_loader_ready() {
    let plugin = seekdeep_tmux_context::plugin();
    assert_eq!(plugin.name(), "tmux-context");
    assert_eq!(plugin.inject(), ["agents"]);

    let context = Context::new();
    let agents = Arc::new(seekdeep_agent::AgentRegistry::new(context.clone()));
    agents.provide(&context).expect("agents");
    let shell = FakeShell::new();
    ShellService::new(shell.clone())
        .provide(&context)
        .expect("shell");
    let fiber = context
        .plugin(plugin, serde_json::Value::Null)
        .expect("mount");
    fiber.await_settled().await.expect("default config");
    let active_session = new_session("plugin-active");
    open_turn(&active_session, 1);
    fire(
        fiber.context(),
        &agent(fiber.context(), active_session.clone(), "plugin-active"),
        1,
        1,
        AbortSignal::default(),
    )
    .await
    .expect("active plugin listener");
    assert_eq!(context_events(&active_session).len(), 1);
    assert_eq!(shell.commands.lock().len(), 1);

    fiber.dispose().await.expect("dispose");
    let disposed_session = new_session("plugin-disposed");
    open_turn(&disposed_session, 1);
    fire(
        &context,
        &agent(&context, disposed_session.clone(), "plugin-disposed"),
        1,
        1,
        AbortSignal::default(),
    )
    .await
    .expect("disposed plugin listener");
    assert!(context_events(&disposed_session).is_empty());
    assert_eq!(shell.commands.lock().len(), 1);

    let invalid = context
        .plugin(
            seekdeep_tmux_context::plugin(),
            json!({"refreshIntervalMs": null}),
        )
        .expect("mount invalid");
    let error = invalid.await_settled().await.expect_err("null interval");
    assert!(format!("{error:#}").contains("non-negative safe integer, got null"));
    assert_eq!(
        context.events().listener_count(&context, "agent/pre-step"),
        0
    );
}

#[test]
fn invalid_refresh_intervals_fail_before_listener_installation() {
    for interval in [-1.0, 1.5, 9_007_199_254_740_992.0, f64::INFINITY, f64::NAN] {
        let context = Context::new();
        let error = apply_with_environment(
            &context,
            &TmuxContextConfig {
                refresh_interval_ms: Some(interval),
            },
            Arc::new(|| 0),
            PID,
            Arc::new(drop),
        )
        .expect_err("invalid interval");
        assert!(format!("{error:#}").contains("non-negative safe integer"));
        assert_eq!(
            context.events().listener_count(&context, "agent/pre-step"),
            0
        );
    }
}
