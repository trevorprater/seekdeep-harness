//! Standalone manual-compaction transaction and failure-classification parity.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::{FutureExt as _, future::BoxFuture, stream};
use parking_lot::Mutex;
use seekdeep_agent::{AgentOptions, AgentStatus};
use seekdeep_agent_loop::{AgentLoopServices, DefaultAgentDriver, LoopAgent};
use seekdeep_commands::CommandId;
use seekdeep_compaction::{
    is_compact_checkpoint_source,
    service::{
        CompactionAgentContext, CompactionEngine as _, CompactionRoutingOptions, MaintenanceTask,
        ManualCompactAgentContext, ManualCompactionError, ManualCompactionErrorCode,
    },
};
use seekdeep_compaction_basic::{
    BasicCompactionConfig, BasicCompactionEngine, BasicCompactionInternals, CompactionAbortError,
    ManualFlush,
    region::{CompactionAppend, RegionSummarize},
    summarizer::{SummarizationInput, SummaryResult},
};
use seekdeep_cordis::{Context, EventOptions, EventReply};
use seekdeep_core::{
    session::{AppendOptions, Session, SessionEvent, SessionId, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::{
    AbortSignal, AdapterStream, ContentBlock, FinishReason, GenerateOptions, LlmAdapter,
    LlmModelContext, LlmResolvedModelInfo, LlmRuntime, Message, MessageSource, ModelId, ProviderId,
    StreamChunk, TokenUsage, UserMessage,
};
use seekdeep_system_prompt::{SystemPrompt, SystemPromptConfig};
use seekdeep_token_meter::{TokenMeterConfig, TokenMeterInstallation};
use seekdeep_tools::{ToolRuntime, ToolRuntimeConfig};
use serde_json::{Value, json};

const MODEL: &str = "mock";
const PROMPT: &str = "older conversation history ";

struct SummaryState {
    calls: Mutex<Vec<SummarizationInput>>,
    summary: Mutex<Vec<ContentBlock>>,
    raw_output: Mutex<Option<Vec<ContentBlock>>>,
    usage: Mutex<Option<TokenUsage>>,
    error: Mutex<Option<String>>,
    mutate: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    gate: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
    entered: Arc<tokio::sync::Semaphore>,
}

impl Default for SummaryState {
    fn default() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            summary: Mutex::new(vec![ContentBlock::Text {
                text: "checkpoint".to_owned(),
            }]),
            raw_output: Mutex::new(None),
            usage: Mutex::new(None),
            error: Mutex::new(None),
            mutate: Mutex::new(None),
            gate: Mutex::new(None),
            entered: Arc::new(tokio::sync::Semaphore::new(0)),
        }
    }
}

fn summarizer(state: &Arc<SummaryState>) -> RegionSummarize {
    let shared = state.clone();
    Arc::new(move |input, _, _, _| {
        let state = shared.clone();
        async move {
            state.calls.lock().push(input);
            if let Some(mutate) = state.mutate.lock().clone() {
                mutate();
            }
            state.entered.add_permits(1);
            let gate = state.gate.lock().clone();
            if let Some(gate) = gate {
                gate.acquire().await.unwrap().forget();
            }
            if let Some(error) = state.error.lock().clone() {
                anyhow::bail!(error);
            }
            let summary = state.summary.lock().clone();
            Ok(SummaryResult {
                summary,
                raw_output: state.raw_output.lock().clone().unwrap_or_default(),
                llm_stream_call: false,
                provider: "summary-provider".to_owned(),
                model: "summary-model".to_owned(),
                max_tokens: None,
                usage: state.usage.lock().clone(),
            })
        }
        .boxed()
    })
}

struct Harness {
    context: Context,
    store: Arc<SessionStore>,
    engine: Arc<BasicCompactionEngine>,
    state: Arc<SummaryState>,
    flushes: Arc<AtomicUsize>,
    _meter: TokenMeterInstallation,
}

impl Harness {
    fn new() -> Self {
        Self::with_internals(None, None)
    }

    fn with_internals(append: Option<CompactionAppend>, manual_flush: Option<ManualFlush>) -> Self {
        let context = Context::new();
        let store = SessionStore::install(&context).unwrap();
        let meter = seekdeep_token_meter::install(&context, TokenMeterConfig::default()).unwrap();
        let flushes = Arc::new(AtomicUsize::new(0));
        let observed = flushes.clone();
        context
            .events()
            .on_sync(
                &context,
                "session/flush",
                move |_, _| {
                    observed.fetch_add(1, Ordering::AcqRel);
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )
            .unwrap();
        let state = Arc::new(SummaryState::default());
        let engine = BasicCompactionEngine::new_with_internals(
            &context,
            &BasicCompactionConfig {
                auto: Some(false),
                ..BasicCompactionConfig::default()
            },
            BasicCompactionInternals {
                summarize: Some(summarizer(&state)),
                append,
                manual_flush,
                measure: None,
            },
        )
        .unwrap();
        Self {
            context,
            store,
            engine,
            state,
            flushes,
            _meter: meter,
        }
    }

    fn create(&self, id: &str, seed: Option<Vec<SessionEvent>>) -> Arc<Session> {
        self.store
            .create(
                &self.context,
                Some(SessionId::new(id)),
                CreateSessionOptions {
                    seed,
                    ..CreateSessionOptions::default()
                },
            )
            .unwrap()
    }

    fn closed(&self, id: &str, turns: u64, last_turn: u64) -> Arc<Session> {
        let session = self.create(id, None);
        append_closed_history(&session, turns, last_turn);
        session
    }

    async fn dispose(self) {
        self.context.fiber().dispose().await.unwrap();
    }
}

fn append_surface(session: &Session, event_type: &str, data: Value) {
    session
        .append(
            event_type,
            data,
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .unwrap();
}

fn append_closed_history(session: &Session, turns: u64, last_turn: u64) {
    for index in 1..=turns {
        let turn = if index == turns { last_turn } else { index };
        session
            .append(
                "turn/start",
                json!({"turn": turn}),
                AppendOptions::default(),
            )
            .unwrap();
        append_surface(
            session,
            "user/message",
            serde_json::to_value(Message::user(
                vec![ContentBlock::Text {
                    text: format!("{} {turn}", PROMPT.repeat(60)),
                }],
                MessageSource::user(),
            ))
            .unwrap(),
        );
        session
            .append(
                "step/start",
                json!({"turn": turn, "step": 1}),
                AppendOptions::default(),
            )
            .unwrap();
        if index == 1 {
            session
                .append(
                    "request/header",
                    json!({
                        "header": {"config": {"provider": MODEL, "model": MODEL}},
                        "reason": "initial"
                    }),
                    AppendOptions::default(),
                )
                .unwrap();
        }
        append_surface(
            session,
            "assistant/message",
            json!({
                "turn": turn,
                "step": 1,
                "message": Message::assistant(
                    vec![ContentBlock::Text { text: format!("answer {turn}") }],
                    MODEL,
                    MODEL,
                )
            }),
        );
        session
            .append(
                "step/end",
                json!({"turn": turn, "step": 1}),
                AppendOptions::default(),
            )
            .unwrap();
        session
            .append(
                "turn/end",
                json!({"turn": turn, "reason": {"kind": "completed"}}),
                AppendOptions::default(),
            )
            .unwrap();
    }
}

fn detached_closed(id: &str, turns: u64, last_turn: u64) -> Arc<Session> {
    let session = Session::create(&SessionId::new(id), None, None).unwrap();
    append_closed_history(&session, turns, last_turn);
    session
}

fn manual_context(
    session: Arc<Session>,
    available: bool,
    maintenance_signal: AbortSignal,
    releases: Arc<AtomicUsize>,
) -> ManualCompactAgentContext {
    let run_maintenance = Arc::new(move |task: MaintenanceTask| {
        let signal = maintenance_signal.clone();
        let releases = releases.clone();
        if !available {
            return async { anyhow::bail!("agent already has active work") }.boxed();
        }
        async move {
            let result = task(signal).await;
            releases.fetch_add(1, Ordering::AcqRel);
            result
        }
        .boxed()
    });
    ManualCompactAgentContext {
        session,
        options: CompactionRoutingOptions {
            provider: Some(MODEL.to_owned()),
            model: Some(MODEL.to_owned()),
        },
        run_maintenance,
    }
}

fn manual_error(error: &anyhow::Error) -> &ManualCompactionError {
    error
        .downcast_ref::<ManualCompactionError>()
        .expect("classified manual compaction error")
}

fn compact_events(session: &Session) -> Vec<SessionEvent> {
    session
        .events()
        .into_iter()
        .filter(|event| event.event_type.starts_with("compaction/"))
        .collect()
}

#[tokio::test]
async fn uncompactable_history_returns_none_without_a_bracket() {
    let harness = Harness::new();
    let session = harness.create("manual-empty", None);
    let releases = Arc::new(AtomicUsize::new(0));
    let result = harness
        .engine
        .compact_now(
            &manual_context(
                session.clone(),
                true,
                AbortSignal::default(),
                releases.clone(),
            ),
            &AbortSignal::default(),
            None,
        )
        .await
        .unwrap();
    assert!(result.is_none());
    assert_eq!(releases.load(Ordering::Acquire), 1);
    assert!(harness.state.calls.lock().is_empty());
    assert!(compact_events(&session).is_empty());
    harness.dispose().await;
}

#[tokio::test]
async fn standalone_bracket_preserves_command_identity_turns_and_checkpoint_source() {
    let harness = Harness::new();
    let session = harness.closed("manual-success", 2, 7);
    let command = CommandId::new("manual-compact-command");
    let result = harness
        .engine
        .compact_now(
            &manual_context(
                session.clone(),
                true,
                AbortSignal::default(),
                Arc::new(AtomicUsize::new(0)),
            ),
            &AbortSignal::default(),
            Some(&command),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.source_command_id.as_ref(), Some(&command));
    assert_eq!(harness.flushes.load(Ordering::Acquire), 1);
    let events = session.events();
    assert_eq!(
        events
            .iter()
            .rfind(|event| event.event_type == "turn/start")
            .unwrap()
            .data["turn"],
        7
    );
    let start = events
        .iter()
        .rev()
        .find(|event| event.event_type == "compaction/start")
        .unwrap();
    let summary = events
        .iter()
        .rev()
        .find(|event| event.event_type == "compaction/summary")
        .unwrap();
    let end = events
        .iter()
        .rev()
        .find(|event| event.event_type == "compaction/end")
        .unwrap();
    assert_eq!(start.data["turn"], Value::Null);
    assert_eq!(start.data["compactionId"], result.compaction_id.as_str());
    assert_eq!(start.data["sourceCommandId"], command.as_str());
    assert_eq!(summary.data["sourceCommandId"], command.as_str());
    assert_eq!(end.data["turn"], Value::Null);
    let checkpoint = events
        .iter()
        .filter(|event| event.event_type == "user/message")
        .filter_map(|event| serde_json::from_value::<Message>(event.data.clone()).ok())
        .find(|message| is_compact_checkpoint_source(message.source()))
        .unwrap();
    assert_eq!(
        checkpoint.source().fields["compactionId"],
        result.compaction_id.as_str()
    );
    assert_eq!(
        checkpoint.source().fields["sourceCommandId"],
        command.as_str()
    );
    harness.dispose().await;
}

#[tokio::test]
async fn live_lock_open_turn_and_unavailable_admission_are_busy() {
    let harness = Harness::new();
    let locked = harness.closed("manual-locked", 2, 2);
    locked
        .append(
            "compaction/start",
            json!({"compactionId": "live-manual-compaction", "turn": null}),
            AppendOptions::default(),
        )
        .unwrap();
    let error = harness
        .engine
        .compact_now(
            &manual_context(
                locked,
                true,
                AbortSignal::default(),
                Arc::new(AtomicUsize::new(0)),
            ),
            &AbortSignal::default(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(manual_error(&error).code, ManualCompactionErrorCode::Busy);
    assert!(
        manual_error(&error)
            .message
            .contains("lock is already active")
    );

    let open = harness.closed("manual-open", 2, 2);
    open.append("turn/start", json!({"turn": 3}), AppendOptions::default())
        .unwrap();
    let error = harness
        .engine
        .compact_now(
            &manual_context(
                open,
                true,
                AbortSignal::default(),
                Arc::new(AtomicUsize::new(0)),
            ),
            &AbortSignal::default(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(manual_error(&error).code, ManualCompactionErrorCode::Busy);
    assert!(manual_error(&error).message.contains("open turn"));

    let unavailable = harness.closed("manual-unavailable", 2, 2);
    let error = harness
        .engine
        .compact_now(
            &manual_context(
                unavailable,
                false,
                AbortSignal::default(),
                Arc::new(AtomicUsize::new(0)),
            ),
            &AbortSignal::default(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(manual_error(&error).code, ManualCompactionErrorCode::Busy);
    harness.dispose().await;
}

#[tokio::test]
async fn stale_inherited_lock_before_end_seed_is_ignored_even_after_turn_repair() {
    for repaired in [false, true] {
        let harness = Harness::new();
        let original = detached_closed("stale-original", 2, 2);
        original
            .append(
                "compaction/start",
                json!({"compactionId": "stale-manual-compaction", "turn": null}),
                AppendOptions::default(),
            )
            .unwrap();
        if repaired {
            original
                .append("turn/start", json!({"turn": 3}), AppendOptions::default())
                .unwrap();
            original
                .append(
                    "turn/end",
                    json!({"turn": 3, "reason": {"kind": "interrupted"}}),
                    AppendOptions::default(),
                )
                .unwrap();
        }
        let session = harness.create(
            if repaired {
                "stale-repaired"
            } else {
                "stale-seed"
            },
            Some(original.events()),
        );
        let result = harness
            .engine
            .compact_now(
                &manual_context(
                    session,
                    true,
                    AbortSignal::default(),
                    Arc::new(AtomicUsize::new(0)),
                ),
                &AbortSignal::default(),
                None,
            )
            .await
            .unwrap();
        assert!(result.is_some());
        assert_eq!(harness.state.calls.lock().len(), 1);
        harness.dispose().await;
    }
}

#[tokio::test]
async fn summarizer_failure_is_classified_and_releases_admission() {
    let harness = Harness::new();
    *harness.state.error.lock() = Some("summarizer unavailable".to_owned());
    let session = harness.closed("manual-summary-failure", 2, 2);
    let before = session.surface_nodes();
    let releases = Arc::new(AtomicUsize::new(0));
    let error = harness
        .engine
        .compact_now(
            &manual_context(
                session.clone(),
                true,
                AbortSignal::default(),
                releases.clone(),
            ),
            &AbortSignal::default(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        manual_error(&error).code,
        ManualCompactionErrorCode::Summary
    );
    assert_eq!(
        manual_error(&error)
            .cause
            .as_ref()
            .map(ToString::to_string)
            .as_deref(),
        Some("summarizer unavailable")
    );
    assert_eq!(session.surface_nodes(), before);
    assert_eq!(releases.load(Ordering::Acquire), 1);
    assert_eq!(
        compact_events(&session)
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        ["compaction/start", "compaction/end"]
    );
    harness.dispose().await;
}

#[tokio::test]
async fn selected_head_or_middle_replacement_is_changed_and_flushes_error_close() {
    for middle in [false, true] {
        let harness = Harness::new();
        let session = harness.closed(
            if middle {
                "manual-middle"
            } else {
                "manual-head"
            },
            if middle { 3 } else { 2 },
            if middle { 3 } else { 2 },
        );
        let target = if middle {
            session.surface_nodes()[1]
        } else {
            session.surface_nodes()[0]
        };
        let mutate = session.clone();
        *harness.state.mutate.lock() = Some(Arc::new(move || {
            mutate
                .append(
                    "user/message",
                    serde_json::to_value(Message::user(
                        vec![ContentBlock::Text {
                            text: "competing replacement".to_owned(),
                        }],
                        MessageSource::plugin("rival"),
                    ))
                    .unwrap(),
                    AppendOptions {
                        surface_op: Some(SurfaceOp::replace(target, target)),
                        source_event_seqs: Some(vec![target]),
                        ..AppendOptions::default()
                    },
                )
                .unwrap();
        }));
        let releases = Arc::new(AtomicUsize::new(0));
        let error = harness
            .engine
            .compact_now(
                &manual_context(
                    session.clone(),
                    true,
                    AbortSignal::default(),
                    releases.clone(),
                ),
                &AbortSignal::default(),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            manual_error(&error).code,
            ManualCompactionErrorCode::Changed
        );
        assert_eq!(releases.load(Ordering::Acquire), 1);
        assert_eq!(harness.flushes.load(Ordering::Acquire), 1);
        assert_eq!(
            compact_events(&session)
                .iter()
                .map(|event| event.event_type.as_str())
                .collect::<Vec<_>>(),
            ["compaction/start", "compaction/end"]
        );
        harness.dispose().await;
    }
}

#[tokio::test]
async fn selected_span_pricing_change_is_classified_as_changed() {
    let context = Context::new();
    let store = SessionStore::install(&context).unwrap();
    let meter = seekdeep_token_meter::install(&context, TokenMeterConfig::default()).unwrap();
    let meter_service: Arc<seekdeep_token_meter::TokenMeter> = (*meter).clone();
    let measurements = Arc::new(AtomicUsize::new(0));
    let observed = measurements.clone();
    let measure: seekdeep_compaction_basic::region::RegionMeasure = Arc::new(move |session| {
        let mut measurement = meter_service.measure(session, None)?;
        if observed.fetch_add(1, Ordering::AcqRel) == 1
            && let Some(head) = measurement.nodes.first_mut()
        {
            head.tokens += 1;
        }
        Ok(measurement)
    });
    let state = Arc::new(SummaryState::default());
    let engine = BasicCompactionEngine::new_with_internals(
        &context,
        &BasicCompactionConfig {
            auto: Some(false),
            ..BasicCompactionConfig::default()
        },
        BasicCompactionInternals {
            summarize: Some(summarizer(&state)),
            measure: Some(measure),
            ..BasicCompactionInternals::default()
        },
    )
    .unwrap();
    let session = store
        .create(
            &context,
            Some(SessionId::new("manual-price-change")),
            CreateSessionOptions::default(),
        )
        .unwrap();
    append_closed_history(&session, 2, 2);
    let error = engine
        .compact_now(
            &manual_context(
                session,
                true,
                AbortSignal::default(),
                Arc::new(AtomicUsize::new(0)),
            ),
            &AbortSignal::default(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        manual_error(&error).code,
        ManualCompactionErrorCode::Changed
    );
    assert!(measurements.load(Ordering::Acquire) >= 2);
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn turnless_history_compacts_without_creating_a_turn() {
    let harness = Harness::new();
    let session = harness.create("manual-turnless", None);
    for text in [PROMPT.repeat(60), "recent tail".to_owned()] {
        append_surface(
            &session,
            "user/message",
            serde_json::to_value(Message::user(
                vec![ContentBlock::Text { text }],
                MessageSource::user(),
            ))
            .unwrap(),
        );
    }
    let result = harness
        .engine
        .compact_now(
            &manual_context(
                session.clone(),
                true,
                AbortSignal::default(),
                Arc::new(AtomicUsize::new(0)),
            ),
            &AbortSignal::default(),
            None,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        session
            .events()
            .iter()
            .all(|event| event.event_type != "turn/start")
    );
    let start = compact_events(&session)
        .into_iter()
        .find(|event| event.event_type == "compaction/start")
        .unwrap();
    assert_eq!(start.data["turn"], Value::Null);
    assert_eq!(start.data["compactionId"], result.compaction_id.as_str());
    harness.dispose().await;
}

#[tokio::test]
async fn caller_and_agent_cancellation_preserve_reason_and_classification() {
    let caller = Harness::new();
    let signal = AbortSignal::default();
    let reason = json!({"kind": "cancelled", "source": "caller"});
    let cancel = signal.clone();
    let expected = reason.clone();
    *caller.state.mutate.lock() = Some(Arc::new(move || {
        cancel.abort_with_reason(expected.clone());
    }));
    *caller.state.error.lock() = Some("summarizer aborted".to_owned());
    let session = caller.closed("manual-caller-cancel", 2, 2);
    let error = caller
        .engine
        .compact_now(
            &manual_context(
                session.clone(),
                true,
                AbortSignal::default(),
                Arc::new(AtomicUsize::new(0)),
            ),
            &signal,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<CompactionAbortError>().unwrap().reason,
        reason
    );
    assert!(
        session
            .events()
            .iter()
            .all(|event| event.event_type != "compaction/summary")
    );
    caller.dispose().await;

    let agent = Harness::new();
    let maintenance = AbortSignal::default();
    let cause = Arc::new(std::io::Error::other("agent cancelled maintenance"));
    let cancel = maintenance.clone();
    let shared = cause.clone();
    *agent.state.mutate.lock() = Some(Arc::new(move || {
        cancel.abort_with_error(shared.clone(), json!({"kind": "agent-cancelled"}));
    }));
    *agent.state.error.lock() = Some("summarizer observed cancellation".to_owned());
    let session = agent.closed("manual-agent-cancel", 2, 2);
    let error = agent
        .engine
        .compact_now(
            &manual_context(session, true, maintenance, Arc::new(AtomicUsize::new(0))),
            &AbortSignal::default(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        manual_error(&error).code,
        ManualCompactionErrorCode::Cancelled
    );
    assert_eq!(
        manual_error(&error)
            .cause
            .as_ref()
            .map(ToString::to_string)
            .as_deref(),
        Some("agent cancelled maintenance")
    );
    agent.dispose().await;
}

#[tokio::test]
async fn preaborted_signal_wins_before_admission_or_log_mutation() {
    for (id, compactable, available) in [
        ("preabort-busy", true, false),
        ("preabort-empty", false, true),
        ("preabort-compactable", true, true),
    ] {
        let harness = Harness::new();
        let session = if compactable {
            harness.closed(id, 2, 9)
        } else {
            harness.create(id, None)
        };
        let before = session.events();
        let releases = Arc::new(AtomicUsize::new(0));
        let signal = AbortSignal::default();
        let reason = json!({"kind": "cancelled", "case": id});
        signal.abort_with_reason(reason.clone());
        let error = harness
            .engine
            .compact_now(
                &manual_context(
                    session.clone(),
                    available,
                    AbortSignal::default(),
                    releases.clone(),
                ),
                &signal,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<CompactionAbortError>().unwrap().reason,
            reason
        );
        assert_eq!(releases.load(Ordering::Acquire), 0);
        assert!(harness.state.calls.lock().is_empty());
        assert_eq!(session.events(), before);
        harness.dispose().await;
    }
}

#[tokio::test]
async fn manual_summary_preserves_raw_output_usage_and_marker_duration() {
    let harness = Harness::new();
    let raw = vec![
        ContentBlock::Text {
            text: "checkpoint".to_owned(),
        },
        ContentBlock::Reasoning {
            text: "hidden reasoning".to_owned(),
        },
    ];
    *harness.state.raw_output.lock() = Some(raw.clone());
    *harness.state.usage.lock() = Some(TokenUsage {
        input_tokens: 40,
        output_tokens: 5,
        cache_read_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
    });
    let session = harness.closed("manual-output", 2, 2);
    harness
        .engine
        .compact_now(
            &manual_context(
                session.clone(),
                true,
                AbortSignal::default(),
                Arc::new(AtomicUsize::new(0)),
            ),
            &AbortSignal::default(),
            None,
        )
        .await
        .unwrap();
    let events = compact_events(&session);
    let start = events
        .iter()
        .find(|event| event.event_type == "compaction/start")
        .unwrap();
    let summary = events
        .iter()
        .find(|event| event.event_type == "compaction/summary")
        .unwrap();
    let end = events
        .iter()
        .find(|event| event.event_type == "compaction/end")
        .unwrap();
    assert_eq!(summary.data["rawOutput"], json!(raw));
    assert_eq!(summary.data["usage"]["inputTokens"], 40);
    assert!(end.time >= start.time);
    harness.dispose().await;
}

#[tokio::test]
async fn manual_and_explicit_region_operations_exclude_each_other() {
    let manual_first = Harness::new();
    let session = manual_first.closed("manual-first", 3, 3);
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    *manual_first.state.gate.lock() = Some(gate.clone());
    let context = manual_context(
        session.clone(),
        true,
        AbortSignal::default(),
        Arc::new(AtomicUsize::new(0)),
    );
    let engine = manual_first.engine.clone();
    let running = tokio::spawn(async move {
        engine
            .compact_now(&context, &AbortSignal::default(), None)
            .await
    });
    manual_first.state.entered.acquire().await.unwrap().forget();
    let nodes = session.surface_nodes();
    let error = manual_first
        .engine
        .compact_region(
            nodes[0],
            nodes[1],
            &CompactionAgentContext {
                session: session.clone(),
                options: CompactionRoutingOptions::default(),
            },
            None,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("lock is already active"));
    gate.add_permits(1);
    assert!(running.await.unwrap().unwrap().is_some());
    manual_first.dispose().await;

    let region_first = Harness::new();
    let session = region_first.closed("region-first", 3, 3);
    session
        .append("turn/start", json!({"turn": 4}), AppendOptions::default())
        .unwrap();
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    *region_first.state.gate.lock() = Some(gate.clone());
    let nodes = session.surface_nodes();
    let engine = region_first.engine.clone();
    let region_agent = CompactionAgentContext {
        session: session.clone(),
        options: CompactionRoutingOptions::default(),
    };
    let start = nodes[0];
    let end = nodes[1];
    let running =
        tokio::spawn(async move { engine.compact_region(start, end, &region_agent, None).await });
    region_first.state.entered.acquire().await.unwrap().forget();
    let error = region_first
        .engine
        .compact_now(
            &manual_context(
                session,
                true,
                AbortSignal::default(),
                Arc::new(AtomicUsize::new(0)),
            ),
            &AbortSignal::default(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(manual_error(&error).code, ManualCompactionErrorCode::Busy);
    gate.add_permits(1);
    assert!(running.await.unwrap().is_ok());
    region_first.dispose().await;
}

fn rejecting_append(event_type: &'static str, message: &'static str) -> CompactionAppend {
    Arc::new(move |session, current, data, options| {
        if current == event_type {
            return Err(seekdeep_core::session::SessionError::InvalidEvent(
                message.to_owned(),
            ));
        }
        session.append(current, data, options)
    })
}

#[tokio::test]
async fn commit_end_failure_is_classified_and_leaves_one_live_orphan() {
    let harness = Harness::with_internals(
        Some(rejecting_append("compaction/end", "boundary rejected")),
        None,
    );
    let session = harness.closed("manual-end-failure", 2, 2);
    let context = manual_context(
        session.clone(),
        true,
        AbortSignal::default(),
        Arc::new(AtomicUsize::new(0)),
    );
    let error = harness
        .engine
        .compact_now(&context, &AbortSignal::default(), None)
        .await
        .unwrap_err();
    assert_eq!(manual_error(&error).code, ManualCompactionErrorCode::Commit);
    assert_eq!(
        manual_error(&error)
            .cause
            .as_ref()
            .map(ToString::to_string)
            .as_deref(),
        Some("boundary rejected")
    );
    assert_eq!(harness.flushes.load(Ordering::Acquire), 0);
    let events = compact_events(&session);
    assert_eq!(
        events
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        ["compaction/start", "compaction/summary"]
    );
    let calls = harness.state.calls.lock().len();
    let error = harness
        .engine
        .compact_now(&context, &AbortSignal::default(), None)
        .await
        .unwrap_err();
    assert_eq!(manual_error(&error).code, ManualCompactionErrorCode::Busy);
    assert_eq!(harness.state.calls.lock().len(), calls);
    harness.dispose().await;
}

#[tokio::test]
async fn failing_error_close_becomes_commit_failure_without_flush() {
    let harness = Harness::with_internals(
        Some(rejecting_append(
            "compaction/end",
            "error boundary rejected",
        )),
        None,
    );
    *harness.state.error.lock() = Some("summary rejected".to_owned());
    let session = harness.closed("manual-error-close", 2, 2);
    let releases = Arc::new(AtomicUsize::new(0));
    let error = harness
        .engine
        .compact_now(
            &manual_context(
                session.clone(),
                true,
                AbortSignal::default(),
                releases.clone(),
            ),
            &AbortSignal::default(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(manual_error(&error).code, ManualCompactionErrorCode::Commit);
    assert_eq!(
        manual_error(&error)
            .cause
            .as_ref()
            .map(ToString::to_string)
            .as_deref(),
        Some("error boundary rejected")
    );
    assert_eq!(releases.load(Ordering::Acquire), 1);
    assert_eq!(harness.flushes.load(Ordering::Acquire), 0);
    assert_eq!(
        compact_events(&session)
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        ["compaction/start"]
    );
    harness.dispose().await;
}

#[tokio::test]
async fn commit_body_failure_closes_releases_and_outranks_flush_failure() {
    let flush_calls = Arc::new(AtomicUsize::new(0));
    let observed = flush_calls.clone();
    let flush: ManualFlush = Arc::new(move |_| {
        observed.fetch_add(1, Ordering::AcqRel);
        async { anyhow::bail!("disk full") }.boxed()
    });
    let harness = Harness::with_internals(
        Some(rejecting_append(
            "compaction/summary",
            "summary record rejected",
        )),
        Some(flush),
    );
    let session = harness.closed("manual-body-failure", 2, 2);
    let releases = Arc::new(AtomicUsize::new(0));
    let error = harness
        .engine
        .compact_now(
            &manual_context(
                session.clone(),
                true,
                AbortSignal::default(),
                releases.clone(),
            ),
            &AbortSignal::default(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(manual_error(&error).code, ManualCompactionErrorCode::Commit);
    assert_eq!(
        manual_error(&error)
            .cause
            .as_ref()
            .map(ToString::to_string)
            .as_deref(),
        Some("summary record rejected")
    );
    assert_eq!(releases.load(Ordering::Acquire), 1);
    assert_eq!(flush_calls.load(Ordering::Acquire), 1);
    let end = compact_events(&session)
        .into_iter()
        .find(|event| event.event_type == "compaction/end")
        .unwrap();
    assert!(
        end.data["error"]
            .to_string()
            .contains("summary record rejected")
    );
    assert_eq!(end.data["turn"], Value::Null);
    harness.dispose().await;
}

#[tokio::test]
async fn durability_failure_after_commit_is_persistence_with_cause() {
    let flush: ManualFlush = Arc::new(|_| async { anyhow::bail!("disk full") }.boxed());
    let harness = Harness::with_internals(None, Some(flush));
    let session = harness.closed("manual-flush-failure", 2, 2);
    let error = harness
        .engine
        .compact_now(
            &manual_context(
                session.clone(),
                true,
                AbortSignal::default(),
                Arc::new(AtomicUsize::new(0)),
            ),
            &AbortSignal::default(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        manual_error(&error).code,
        ManualCompactionErrorCode::Persistence
    );
    assert_eq!(
        manual_error(&error)
            .cause
            .as_ref()
            .map(ToString::to_string)
            .as_deref(),
        Some("disk full")
    );
    assert!(
        compact_events(&session)
            .iter()
            .any(|event| event.event_type == "compaction/summary")
    );
    harness.dispose().await;
}

#[tokio::test]
async fn cancellation_waits_for_admitted_flush_before_reason_and_release() {
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let flush_entered = entered.clone();
    let flush_release = release.clone();
    let flush: ManualFlush = Arc::new(move |_| {
        let entered = flush_entered.clone();
        let release = flush_release.clone();
        async move {
            entered.add_permits(1);
            release.acquire().await.unwrap().forget();
            Ok(())
        }
        .boxed()
    });
    let harness = Harness::with_internals(None, Some(flush));
    let session = harness.closed("manual-flush-cancel", 2, 2);
    let releases = Arc::new(AtomicUsize::new(0));
    let signal = AbortSignal::default();
    let context = manual_context(session, true, AbortSignal::default(), releases.clone());
    let engine = harness.engine.clone();
    let caller = signal.clone();
    let running = tokio::spawn(async move { engine.compact_now(&context, &caller, None).await });
    entered.acquire().await.unwrap().forget();
    let reason = json!({"kind": "cancelled", "during": "flush"});
    signal.abort_with_reason(reason.clone());
    tokio::task::yield_now().await;
    assert!(!running.is_finished());
    assert_eq!(releases.load(Ordering::Acquire), 0);
    release.add_permits(1);
    let error = running.await.unwrap().unwrap_err();
    assert_eq!(
        error.downcast_ref::<CompactionAbortError>().unwrap().reason,
        reason
    );
    assert_eq!(releases.load(Ordering::Acquire), 1);
    harness.dispose().await;
}

struct TextAdapter {
    requests: Mutex<Vec<Vec<Message>>>,
}

#[async_trait]
impl LlmAdapter for TextAdapter {
    async fn resolve_model(
        &self,
        provider: &str,
        model: &str,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<LlmResolvedModelInfo> {
        Ok(LlmResolvedModelInfo {
            provider: ProviderId::new(provider),
            id: ModelId::new(model),
            name: model.to_owned(),
            description: None,
            input_modalities: None,
            context: Some(LlmModelContext {
                context_window: 100_000,
            }),
            default_max_tokens: None,
            reasoning: None,
        })
    }

    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        self.requests.lock().push(options.messages);
        AdapterStream::new(stream::iter(
            vec![
                StreamChunk::BlockStart {
                    index: 0,
                    block_type: "text".to_owned(),
                },
                StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::Text {
                        text: "answer".to_owned(),
                    },
                },
                StreamChunk::Finish {
                    reason: FinishReason::Stop,
                    replay_state: None,
                },
            ]
            .into_iter()
            .map(Ok),
        ))
    }
}

struct LoopHarness {
    context: Context,
    agent: LoopAgent,
    _driver: Arc<DefaultAgentDriver>,
    engine: Arc<BasicCompactionEngine>,
    state: Arc<SummaryState>,
    adapter: Arc<TextAdapter>,
    log: Arc<Mutex<Vec<String>>>,
    _meter: TokenMeterInstallation,
}

fn install_loop_log(context: &Context) -> Arc<Mutex<Vec<String>>> {
    let log = Arc::new(Mutex::new(Vec::new()));
    let events = log.clone();
    context
        .events()
        .on_sync(
            context,
            "session/event",
            move |_, args| {
                let event = args
                    .get::<SessionEvent>(1)
                    .ok_or_else(|| anyhow::anyhow!("session/event lacks event"))?;
                let label = match event.event_type.as_str() {
                    "turn/start" => Some("turn/start".to_owned()),
                    "turn/end" => Some("turn/end".to_owned()),
                    "compaction/start" => Some(format!("compaction/start:{}", event.data["turn"])),
                    "compaction/summary" => Some("compaction/summary".to_owned()),
                    "compaction/end" => Some(format!("compaction/end:{}", event.data["turn"])),
                    "user/message" => Some("user/message".to_owned()),
                    _ => None,
                };
                if let Some(label) = label {
                    events.lock().push(label);
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    let flushes = log.clone();
    context
        .events()
        .on_sync(
            context,
            "session/flush",
            move |_, _| {
                flushes.lock().push("flush".to_owned());
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    log
}

impl LoopHarness {
    fn new() -> Self {
        let context = Context::new();
        let store = SessionStore::install(&context).unwrap();
        let llm = LlmRuntime::install(&context).unwrap();
        let adapter = Arc::new(TextAdapter {
            requests: Mutex::new(Vec::new()),
        });
        llm.register_adapter(&[MODEL.to_owned()], adapter.clone())
            .unwrap();
        let system_prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
        let tools = ToolRuntime::new_with_system_prompt(
            &context,
            &system_prompt,
            ToolRuntimeConfig::default(),
        )
        .unwrap();
        let meter = seekdeep_token_meter::install(&context, TokenMeterConfig::default()).unwrap();
        let state = Arc::new(SummaryState::default());
        let engine = BasicCompactionEngine::new_with_summarizer(
            &context,
            &BasicCompactionConfig {
                auto: Some(false),
                ..BasicCompactionConfig::default()
            },
            summarizer(&state),
        )
        .unwrap();
        let session = store
            .create(
                &context,
                Some(SessionId::new("manual-loop")),
                CreateSessionOptions::default(),
            )
            .unwrap();
        let (agent, driver) = LoopAgent::new_default(
            &context,
            &session,
            AgentOptions {
                provider: Some(MODEL.into()),
                model: Some(MODEL.into()),
                ..AgentOptions::default()
            },
            None,
            AgentLoopServices {
                llm,
                system_prompt,
                tools,
                max_parallel_tool_calls: 10,
            },
        )
        .unwrap();
        let log = install_loop_log(&context);
        Self {
            context,
            agent,
            _driver: driver,
            engine,
            state,
            adapter,
            log,
            _meter: meter,
        }
    }

    async fn seed(&self) {
        self.agent.agent.followup(user(&PROMPT.repeat(60))).unwrap();
        self.agent.agent.when_idle().unwrap().await.unwrap();
        self.log.lock().clear();
    }

    fn manual_context(&self) -> ManualCompactAgentContext {
        let agent = self.agent.agent.clone();
        let runner = Arc::new(move |task: MaintenanceTask| {
            let agent = agent.clone();
            Box::pin(async move {
                match agent.run_maintenance(task) {
                    Ok(future) => future.await?,
                    Err(error) => Err(anyhow::anyhow!(error.to_string())),
                }
            })
                as BoxFuture<'static, anyhow::Result<Option<seekdeep_compaction::CompactionResult>>>
        });
        ManualCompactAgentContext {
            session: self.agent.agent.session().clone(),
            options: CompactionRoutingOptions {
                provider: Some(MODEL.to_owned()),
                model: Some(MODEL.to_owned()),
            },
            run_maintenance: runner,
        }
    }

    async fn dispose(self) {
        self.context.fiber().dispose().await.unwrap();
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

fn derived_text(session: &Session) -> Vec<String> {
    session
        .derive_messages()
        .into_iter()
        .map(|message| {
            message
                .content()
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("")
        })
        .collect()
}

#[tokio::test]
async fn loop_holds_followup_until_manual_bracket_flushes_then_runs_from_checkpoint() {
    let harness = LoopHarness::new();
    harness.seed().await;
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    *harness.state.gate.lock() = Some(gate.clone());
    let engine = harness.engine.clone();
    let context = harness.manual_context();
    let running = tokio::spawn(async move {
        engine
            .compact_now(&context, &AbortSignal::default(), None)
            .await
    });
    harness.state.entered.acquire().await.unwrap().forget();
    assert_eq!(harness.log.lock().as_slice(), ["compaction/start:null"]);
    harness
        .agent
        .agent
        .followup(user("after compaction"))
        .unwrap();
    tokio::task::yield_now().await;
    assert_eq!(harness.agent.agent.status(), AgentStatus::Idle);
    assert_eq!(harness.adapter.requests.lock().len(), 1);
    gate.add_permits(1);
    assert!(running.await.unwrap().unwrap().is_some());
    harness.agent.agent.when_idle().unwrap().await.unwrap();
    let log = harness.log.lock().clone();
    let position = |needle: &str| log.iter().position(|entry| entry == needle).unwrap();
    assert!(position("compaction/start:null") < position("compaction/summary"));
    assert!(position("compaction/summary") < position("compaction/end:null"));
    assert!(position("compaction/end:null") < position("flush"));
    assert!(position("flush") < position("turn/start"));
    {
        let requests = harness.adapter.requests.lock();
        assert_eq!(requests.len(), 2);
        let second = requests[1]
            .iter()
            .map(|message| {
                message
                    .content()
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>();
        assert!(second[0].contains("checkpoint"));
        assert_eq!(second.last().map(String::as_str), Some("after compaction"));
        assert!(second.iter().all(|text| !text.contains(PROMPT)));
    }
    harness.dispose().await;
}

#[tokio::test]
async fn loop_keeps_injected_context_pending_and_marker_listener_order_stable() {
    let harness = LoopHarness::new();
    harness.seed().await;
    let agent = harness.agent.agent.clone();
    *harness.state.mutate.lock() = Some(Arc::new(move || {
        agent
            .inject(UserMessage::new(
                vec![ContentBlock::Text {
                    text: "INJECTED CONTEXT".to_owned(),
                }],
                MessageSource::plugin("test"),
            ))
            .unwrap();
    }));
    assert!(
        harness
            .engine
            .compact_now(&harness.manual_context(), &AbortSignal::default(), None,)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        harness
            .agent
            .agent
            .inbox()
            .next_step()
            .iter()
            .any(|message| {
                message
                    .source()
                    .fields
                    .get("plugin")
                    .and_then(Value::as_str)
                    == Some("test")
            })
    );
    harness
        .agent
        .agent
        .followup(user("after compaction"))
        .unwrap();
    harness.agent.agent.when_idle().unwrap().await.unwrap();
    let text = derived_text(harness.agent.agent.session());
    assert!(text[0].contains("checkpoint"));
    assert_eq!(
        text.iter()
            .filter(|text| text.contains("INJECTED CONTEXT"))
            .count(),
        1
    );
    harness.dispose().await;
}

#[tokio::test]
async fn loop_marker_listeners_inject_reentrantly_without_reordering_bracket() {
    let harness = LoopHarness::new();
    harness.seed().await;
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let observed = attempts.clone();
    let agent = harness.agent.agent.clone();
    harness
        .context
        .events()
        .on_sync(
            &harness.context,
            "session/event",
            move |_, args| {
                let event = args
                    .get::<SessionEvent>(1)
                    .ok_or_else(|| anyhow::anyhow!("session/event lacks event"))?;
                if matches!(
                    event.event_type.as_str(),
                    "compaction/start" | "compaction/summary"
                ) {
                    observed.lock().push(event.event_type.clone());
                    agent
                        .inject(UserMessage::new(
                            vec![ContentBlock::Text {
                                text: format!("from {}", event.event_type),
                            }],
                            MessageSource::plugin("listener"),
                        ))
                        .unwrap();
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    assert!(
        harness
            .engine
            .compact_now(&harness.manual_context(), &AbortSignal::default(), None,)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        attempts.lock().as_slice(),
        ["compaction/start", "compaction/summary"]
    );
    assert_eq!(
        harness
            .agent
            .agent
            .inbox()
            .next_step()
            .iter()
            .filter(|message| {
                message
                    .source()
                    .fields
                    .get("plugin")
                    .and_then(Value::as_str)
                    == Some("listener")
            })
            .count(),
        2
    );
    assert!(derived_text(harness.agent.agent.session())[0].contains("checkpoint"));
    assert_eq!(
        compact_events(harness.agent.agent.session())
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        ["compaction/start", "compaction/summary", "compaction/end"]
    );
    assert!(harness.agent.agent.session().events().iter().all(|event| {
        event.event_type != "user/message" || event.data["source"]["plugin"] != "listener"
    }));
    harness.dispose().await;
}

#[tokio::test]
async fn loop_reports_busy_for_queued_turn_and_recovers_after_summary_failure() {
    let busy = LoopHarness::new();
    busy.seed().await;
    busy.agent.agent.followup(user("first in line")).unwrap();
    let error = busy
        .engine
        .compact_now(&busy.manual_context(), &AbortSignal::default(), None)
        .await
        .unwrap_err();
    assert_eq!(manual_error(&error).code, ManualCompactionErrorCode::Busy);
    assert!(busy.state.calls.lock().is_empty());
    busy.agent.agent.when_idle().unwrap().await.unwrap();
    assert_eq!(busy.adapter.requests.lock().len(), 2);
    busy.dispose().await;

    let failed = LoopHarness::new();
    failed.seed().await;
    *failed.state.error.lock() = Some("summarizer unavailable".to_owned());
    let before = failed.agent.agent.session().surface_nodes();
    let error = failed
        .engine
        .compact_now(&failed.manual_context(), &AbortSignal::default(), None)
        .await
        .unwrap_err();
    assert_eq!(
        manual_error(&error).code,
        ManualCompactionErrorCode::Summary
    );
    assert_eq!(failed.agent.agent.session().surface_nodes(), before);
    failed
        .agent
        .agent
        .followup(user("runs after the failure"))
        .unwrap();
    failed.agent.agent.when_idle().unwrap().await.unwrap();
    assert_eq!(failed.adapter.requests.lock().len(), 2);
    failed.dispose().await;
}
