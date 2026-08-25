//! Real-process ACP run, permission, cancellation, rollback, config, and quiescence parity.

#![cfg(unix)]

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use seekdeep_acp::PermissionPolicy;
use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionHeader, SessionId};
use seekdeep_llm::{AbortSignal, ContentBlock};
use seekdeep_scope::ScopeKey;
use seekdeep_subagent::{SubagentRuntime, SubagentStartRequest, SubagentStopReason};
use seekdeep_subagent_acp::{
    AcpRunSpec, Config, DEFAULT_DISPOSE_EOF_GRACE_MS, DEFAULT_DISPOSE_GRACE_MS, INJECT, NAME,
    apply, plugin, start_acp_run,
};
use seekdeep_subprocess::SUBPROCESS;
use seekdeep_subprocess_local::LocalSubprocessRuntime;
use serde_json::json;

const FIXTURE: &str = env!("CARGO_BIN_EXE_seekdeep-acp-server-fixture");

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
            label: None,
            prompt: vec![ContentBlock::Text {
                text: "do the task".to_owned(),
            }],
            parent: parent(
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

    fn spec(&self, mode: &str, permission: PermissionPolicy) -> AcpRunSpec {
        AcpRunSpec {
            command: FIXTURE.to_owned(),
            args: Vec::new(),
            cwd: self.workspace.path().to_string_lossy().into_owned(),
            permission,
            env: BTreeMap::from([("SEEKDEEP_ACP_FIXTURE_MODE".to_owned(), mode.to_owned())]),
            dispose_eof_grace_ms: 300.0,
            dispose_grace_ms: 300.0,
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

fn parent(context: &Context, cwd: Option<&str>, id: &str) -> Arc<Agent> {
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

fn text(result: &seekdeep_subagent::SubagentResult) -> String {
    result
        .output
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn real_child_maps_terminal_reasons_streams_only_messages_and_flushes_on_eof() {
    for (stop, expected) in [
        ("end_turn", SubagentStopReason::Completed),
        ("max_tokens", SubagentStopReason::MaxTokens),
        ("refusal", SubagentStopReason::Refusal),
        ("cancelled", SubagentStopReason::Aborted),
        ("max_turn_requests", SubagentStopReason::Error),
        ("future-stop", SubagentStopReason::Error),
    ] {
        let harness = Harness::new();
        let flush = harness.workspace.path().join("flushed");
        let mut spec = harness.spec("normal", PermissionPolicy::Reject);
        spec.env
            .insert("SEEKDEEP_ACP_FIXTURE_STOP".to_owned(), stop.to_owned());
        spec.env.insert(
            "SEEKDEEP_ACP_FIXTURE_TEXT".to_owned(),
            "final answer".to_owned(),
        );
        spec.env
            .insert("SEEKDEEP_ACP_FIXTURE_THOUGHT".to_owned(), "1".to_owned());
        spec.env.insert(
            "SEEKDEEP_ACP_FIXTURE_FLUSH".to_owned(),
            flush.to_string_lossy().into_owned(),
        );
        let run = start_acp_run(harness.request(AbortSignal::default()), spec)
            .await
            .unwrap();
        assert!(run.local_agent().is_none());
        let result = run.result().await.unwrap();
        assert_eq!(result.stop_reason, expected, "{stop}");
        assert_eq!(text(&result), "final answer");
        run.dispose().await.unwrap();
        run.dispose().await.unwrap();
        assert!(flush.exists());
        harness.quiescent().await;
        harness.context.fiber().dispose().await.unwrap();
    }
}

#[tokio::test]
async fn permission_policy_selects_allow_or_cancel_without_human_interaction() {
    for (permission, no_allow, expected) in [
        (PermissionPolicy::Reject, false, SubagentStopReason::Aborted),
        (
            PermissionPolicy::Allow,
            false,
            SubagentStopReason::Completed,
        ),
        (PermissionPolicy::Allow, true, SubagentStopReason::Aborted),
    ] {
        let harness = Harness::new();
        let record = harness.workspace.path().join("record.jsonl");
        let mut spec = harness.spec("normal", permission);
        spec.env
            .insert("SEEKDEEP_ACP_FIXTURE_PERMISSION".to_owned(), "1".to_owned());
        if no_allow {
            spec.env
                .insert("SEEKDEEP_ACP_FIXTURE_NO_ALLOW".to_owned(), "1".to_owned());
        }
        spec.env.insert(
            "SEEKDEEP_ACP_FIXTURE_RECORD".to_owned(),
            record.to_string_lossy().into_owned(),
        );
        let run = start_acp_run(harness.request(AbortSignal::default()), spec)
            .await
            .unwrap();
        assert_eq!(run.result().await.unwrap().stop_reason, expected);
        run.dispose().await.unwrap();
        let records = std::fs::read_to_string(record).unwrap();
        assert!(records.contains("permission-1"));
        if expected == SubagentStopReason::Completed {
            assert!(records.contains("\"optionId\":\"allow\""));
        } else {
            assert!(records.contains("\"outcome\":\"cancelled\""));
        }
        harness.quiescent().await;
        harness.context.fiber().dispose().await.unwrap();
    }
}

#[tokio::test]
async fn signal_cancellation_settles_even_when_child_ignores_cancel_and_dispose_reaps() {
    for mode in ["hang", "ignore-cancel"] {
        let harness = Harness::new();
        let ready = harness.workspace.path().join("ready");
        let signal = AbortSignal::default();
        let mut spec = harness.spec(mode, PermissionPolicy::Reject);
        spec.env.insert(
            "SEEKDEEP_ACP_FIXTURE_READY".to_owned(),
            ready.to_string_lossy().into_owned(),
        );
        let run = start_acp_run(harness.request(signal.clone()), spec)
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
        let result = tokio::time::timeout(Duration::from_secs(3), run.result())
            .await
            .expect("cancel result timeout")
            .unwrap();
        assert_eq!(result.stop_reason, SubagentStopReason::Aborted);
        run.dispose().await.unwrap();
        harness.quiescent().await;
        harness.context.fiber().dispose().await.unwrap();
    }
}

#[tokio::test]
async fn startup_failures_and_preabort_reject_after_private_process_cleanup() {
    let harness = Harness::new();
    let sentinel = harness.workspace.path().join("spawned");
    let signal = AbortSignal::default();
    signal.abort();
    let mut spec = harness.spec("normal", PermissionPolicy::Reject);
    spec.env.insert(
        "SEEKDEEP_ACP_FIXTURE_SPAWNED".to_owned(),
        sentinel.to_string_lossy().into_owned(),
    );
    assert!(
        start_acp_run(harness.request(signal), spec)
            .await
            .err()
            .expect("preabort must reject")
            .to_string()
            .contains("aborted before")
    );
    assert!(!sentinel.exists());

    let error = start_acp_run(
        harness.request(AbortSignal::default()),
        harness.spec("missing-session", PermissionPolicy::Reject),
    )
    .await
    .err()
    .expect("missing session id must reject");
    assert!(error.to_string().contains("without a session id"));
    harness.quiescent().await;

    let missing = AcpRunSpec {
        command: harness
            .workspace
            .path()
            .join("missing")
            .to_string_lossy()
            .into_owned(),
        ..harness.spec("normal", PermissionPolicy::Reject)
    };
    assert!(
        start_acp_run(harness.request(AbortSignal::default()), missing)
            .await
            .is_err()
    );
    harness.quiescent().await;

    let ready = harness.workspace.path().join("new-ready");
    let go = harness.workspace.path().join("new-go");
    let signal = AbortSignal::default();
    let mut spec = harness.spec("normal", PermissionPolicy::Reject);
    spec.dispose_eof_grace_ms = 1_000.0;
    spec.env.insert(
        "SEEKDEEP_ACP_FIXTURE_NEW_READY".to_owned(),
        ready.to_string_lossy().into_owned(),
    );
    spec.env.insert(
        "SEEKDEEP_ACP_FIXTURE_NEW_GO".to_owned(),
        go.to_string_lossy().into_owned(),
    );
    let pending_signal = signal.clone();
    let pending = tokio::spawn(start_acp_run(harness.request(pending_signal), spec));
    for _ in 0..300 {
        if ready.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(ready.exists());
    signal.abort_with_reason(json!("cancel during session/new"));
    std::fs::write(go, b"go\n").unwrap();
    let error = pending
        .await
        .unwrap()
        .err()
        .expect("mid-start cancellation must reject");
    assert!(error.to_string().contains("aborted before"));
    harness.quiescent().await;
    harness.context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn post_publication_failure_is_logged_once_and_sink_panics_are_contained() {
    let harness = Harness::new();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = Arc::clone(&seen);
    let mut spec = harness.spec("crash-prompt", PermissionPolicy::Reject);
    spec.on_error = Some(Arc::new(move |error, reason| {
        observed.lock().unwrap().push((error.to_string(), reason));
        panic!("sink failure is contained");
    }));
    let run = start_acp_run(harness.request(AbortSignal::default()), spec)
        .await
        .unwrap();
    let result = run.result().await.unwrap();
    assert_eq!(result.stop_reason, SubagentStopReason::Error);
    assert_eq!(seen.lock().unwrap().len(), 1);
    run.dispose().await.unwrap();
    harness.quiescent().await;
    harness.context.fiber().dispose().await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn provider_config_cwd_registration_hmr_and_failure_diagnostics_are_exact() {
    assert_eq!(NAME, "subagent-acp");
    assert_eq!(INJECT, ["subagents", "subprocess"]);
    let defaults = Config::default();
    assert_eq!(defaults.provider_name, "acp");
    assert_eq!(defaults.permission, PermissionPolicy::Reject);
    assert_eq!(
        defaults.dispose_eof_grace_ms.to_bits(),
        DEFAULT_DISPOSE_EOF_GRACE_MS.to_bits()
    );
    assert_eq!(
        defaults.dispose_grace_ms.to_bits(),
        DEFAULT_DISPOSE_GRACE_MS.to_bits()
    );

    let context = Context::new();
    let subagents = SubagentRuntime::install(&context).unwrap();
    let runtime = LocalSubprocessRuntime::install(&context).unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let fiber = context
        .plugin(
            plugin(),
            json!({
                "providerName":"remote",
                "command":FIXTURE,
                "cwd":workspace.path().to_string_lossy(),
                "permission":"reject",
                "env":{
                    "SEEKDEEP_ACP_FIXTURE_MODE":"normal",
                    "SEEKDEEP_ACP_FIXTURE_TEXT":"provider answer"
                },
                "disposeEofGraceMs":300,
                "disposeGraceMs":300
            }),
        )
        .unwrap();
    fiber.await_settled().await.unwrap();
    let provider = subagents.get_provider("remote").unwrap();
    assert_eq!(
        provider.capabilities(),
        &seekdeep_subagent::no_start_capabilities()
    );
    assert!(!provider.inherits_parent_context());
    let run = subagents
        .start(
            "remote",
            SubagentStartRequest {
                label: None,
                prompt: vec![ContentBlock::Text { text: "x".into() }],
                parent: parent(&context, None, "no-parent-cwd"),
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
    assert_eq!(text(&run.result().await.unwrap()), "provider answer");
    run.dispose().await.unwrap();
    for _ in 0..100 {
        if runtime.live_process_count() == 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(runtime.live_process_count(), 0);
    fiber.dispose().await.unwrap();
    assert!(subagents.get_provider("remote").is_none());

    apply(
        &context,
        Config {
            command: FIXTURE.to_owned(),
            ..Config::default()
        },
    )
    .unwrap();
    let error = subagents
        .start(
            "acp",
            SubagentStartRequest {
                label: None,
                prompt: vec![ContentBlock::Text { text: "x".into() }],
                parent: parent(&context, None, "missing-cwd"),
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
                    command: "true".to_owned(),
                    dispose_eof_grace_ms: invalid,
                    ..Config::default()
                }
            )
            .is_err()
        );
    }
}
