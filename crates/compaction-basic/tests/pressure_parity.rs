//! Routed pressure, retention, and provider-confirmed overflow parity.

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use futures::{FutureExt as _, stream};
use parking_lot::Mutex;
use seekdeep_agent::{
    Agent, AgentEvents, AgentOptions, Inbox, NoopInboxNotifications, PreStepDecision,
    RequestErrorAction,
};
use seekdeep_agent_loop::{AgentPreStepEvent, AgentRequestErrorEvent};
use seekdeep_compaction::{
    service::{
        COMPACTION, CompactionAgentContext, CompactionEngine as _, CompactionRoutingOptions,
        CompactionTrigger,
    },
    tool_pairing::{tool_pairing_balanced_after, tool_pairing_balanced_before},
};
use seekdeep_compaction_basic::{
    BasicCompactionConfig, BasicCompactionEngine,
    region::{RegionSummarize, select_compactable_range},
    summarizer::{SummarizationInput, SummaryResult},
};
use seekdeep_compaction_tool_result_pruner::{
    PRUNE_MARKER, ToolResultPruneConfig, ToolResultPruner,
};
use seekdeep_cordis::Context;
use seekdeep_core::session::{AppendOptions, Session, SessionId, SurfaceOp};
use seekdeep_llm::{
    AbortSignal, AdapterStream, CONTEXT_WINDOW_EXCEEDED_CODE, CallId, ContentBlock, FinishReason,
    GenerateOptions, LlmAdapter, LlmFailure, LlmModelContext, LlmResolvedModelInfo, LlmRuntime,
    Message, MessageSource, ModelId, ProviderId, StreamChunk, TokenUsage,
};
use seekdeep_scope::ScopeKey;
use seekdeep_token_meter::{TokenMeterConfig, TokenMeterInstallation};
use serde_json::json;

const MODEL: &str = "test-model";

#[derive(Debug)]
struct ContextAdapter {
    windows: BTreeMap<String, Option<u64>>,
    signals: Arc<Mutex<Vec<Option<AbortSignal>>>>,
}

#[async_trait]
impl LlmAdapter for ContextAdapter {
    async fn resolve_model(
        &self,
        provider: &str,
        model: &str,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<LlmResolvedModelInfo> {
        self.signals.lock().push(signal.cloned());
        Ok(LlmResolvedModelInfo {
            provider: ProviderId::new(provider),
            id: ModelId::new(model),
            name: model.to_owned(),
            description: None,
            input_modalities: None,
            context: self
                .windows
                .get(provider)
                .copied()
                .flatten()
                .map(|context_window| LlmModelContext { context_window }),
            default_max_tokens: None,
            reasoning: None,
        })
    }

    fn stream(&self, _options: GenerateOptions) -> AdapterStream {
        AdapterStream::new(stream::iter([Ok(StreamChunk::Finish {
            reason: FinishReason::Stop,
            replay_state: None,
        })]))
    }
}

struct SummaryState {
    calls: Mutex<Vec<SummarizationInput>>,
    signals: Mutex<Vec<Option<AbortSignal>>>,
    summary: Mutex<Vec<ContentBlock>>,
    raw_output: Mutex<Option<Vec<ContentBlock>>>,
    usage: Mutex<Option<TokenUsage>>,
    error: Mutex<Option<String>>,
    mutate: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl Default for SummaryState {
    fn default() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            signals: Mutex::new(Vec::new()),
            summary: Mutex::new(vec![ContentBlock::Text {
                text: "small checkpoint".to_owned(),
            }]),
            raw_output: Mutex::new(None),
            usage: Mutex::new(None),
            error: Mutex::new(None),
            mutate: Mutex::new(None),
        }
    }
}

struct Harness {
    context: Context,
    engine: Arc<BasicCompactionEngine>,
    summary: Arc<SummaryState>,
    adapter_signals: Arc<Mutex<Vec<Option<AbortSignal>>>>,
    meter: TokenMeterInstallation,
    _config: BasicCompactionConfig,
}

impl Harness {
    fn new(config: BasicCompactionConfig, windows: &[(&str, Option<u64>)]) -> Self {
        let context = Context::new();
        let llm = LlmRuntime::install(&context).unwrap();
        let adapter_signals = Arc::new(Mutex::new(Vec::new()));
        let routes = windows
            .iter()
            .map(|(provider, _)| (*provider).to_owned())
            .collect::<Vec<_>>();
        llm.register_adapter(
            &routes,
            Arc::new(ContextAdapter {
                windows: windows
                    .iter()
                    .map(|(provider, window)| ((*provider).to_owned(), *window))
                    .collect(),
                signals: adapter_signals.clone(),
            }),
        )
        .unwrap();
        let meter = seekdeep_token_meter::install(&context, TokenMeterConfig::default()).unwrap();
        let summary = Arc::new(SummaryState::default());
        let state = summary.clone();
        let summarize: RegionSummarize = Arc::new(move |input, _, _, signal| {
            let state = state.clone();
            async move {
                state.calls.lock().push(input);
                state.signals.lock().push(signal);
                if let Some(mutate) = state.mutate.lock().clone() {
                    mutate();
                }
                if let Some(error) = state.error.lock().clone() {
                    anyhow::bail!(error);
                }
                let summary = state.summary.lock().clone();
                let raw_output = state
                    .raw_output
                    .lock()
                    .clone()
                    .unwrap_or_else(|| summary.clone());
                Ok(SummaryResult {
                    raw_output,
                    summary,
                    llm_stream_call: false,
                    provider: "summary-provider".to_owned(),
                    model: "summary-model".to_owned(),
                    max_tokens: Some(123),
                    usage: state.usage.lock().clone(),
                })
            }
            .boxed()
        });
        let engine = BasicCompactionEngine::new_with_summarizer(&context, &config, summarize)
            .expect("compaction engine");
        Self {
            context,
            engine,
            summary,
            adapter_signals,
            meter,
            _config: config,
        }
    }

    fn install_pruner(&self) -> Arc<ToolResultPruner> {
        ToolResultPruner::new(
            &self.context,
            &ToolResultPruneConfig {
                threshold_chars: Some(100),
                head_chars: Some(20),
                tail_chars: Some(10),
            },
        )
        .unwrap()
    }

    async fn dispose(self) {
        self.context.fiber().dispose().await.unwrap();
    }
}

fn append_surface(session: &Session, event_type: &str, data: serde_json::Value) {
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

fn conversation(turns: u64, text: &str) -> Arc<Session> {
    let session =
        Session::create(&SessionId::new(format!("conversation-{turns}")), None, None).unwrap();
    for turn in 1..=turns {
        session
            .append(
                "turn/start",
                json!({"turn": turn}),
                AppendOptions::default(),
            )
            .unwrap();
        append_surface(
            &session,
            "user/message",
            serde_json::to_value(Message::user(
                vec![ContentBlock::Text {
                    text: format!("{} user {turn}", text.trim()),
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
        if turn == 1 {
            append_header(&session, MODEL, MODEL, None, "initial");
        }
        append_surface(
            &session,
            "assistant/message",
            json!({
                "turn": turn,
                "step": 1,
                "message": Message::assistant(
                    vec![ContentBlock::Text {
                        text: format!("{} assistant {turn}", text.trim()),
                    }],
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
    session
        .append(
            "turn/start",
            json!({"turn": turns + 1}),
            AppendOptions::default(),
        )
        .unwrap();
    session
}

fn append_tool_exchange(session: &Session, turn: u64, call: &str, repeat: usize) {
    let call_id = CallId::new(call);
    append_surface(
        session,
        "assistant/message",
        json!({
            "turn": turn,
            "step": 1,
            "message": Message::assistant(
                vec![
                    ContentBlock::Text { text: format!("calling {turn} ").repeat(repeat) },
                    ContentBlock::ToolCall {
                        id: call_id.clone(),
                        name: "read".to_owned(),
                        arguments: "{}".to_owned(),
                    }
                ],
                MODEL,
                MODEL,
            )
        }),
    );
    session
        .append(
            "tool/call",
            json!({
                "turn": turn,
                "step": 1,
                "callId": call_id,
                "name": "read",
                "arguments": "{}"
            }),
            AppendOptions::default(),
        )
        .unwrap();
    append_surface(
        session,
        "tool/result",
        json!({
            "turn": turn,
            "step": 1,
            "message": Message::tool_result(
                &call_id,
                vec![ContentBlock::Text {
                    text: format!("result {turn} ").repeat(repeat),
                }],
                false,
            )
        }),
    );
}

fn tool_conversation(turns: u64, repeat: usize) -> Arc<Session> {
    let session = Session::create(&SessionId::new(format!("tools-{turns}")), None, None).unwrap();
    for turn in 1..=turns {
        session
            .append(
                "turn/start",
                json!({"turn": turn}),
                AppendOptions::default(),
            )
            .unwrap();
        append_surface(
            &session,
            "user/message",
            serde_json::to_value(Message::user(
                vec![ContentBlock::Text {
                    text: format!("request {turn} ").repeat(repeat),
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
        if turn == 1 {
            append_header(&session, MODEL, MODEL, None, "initial");
        }
        append_tool_exchange(&session, turn, &format!("call-{turn}"), repeat);
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
    session
        .append(
            "turn/start",
            json!({"turn": turns + 1}),
            AppendOptions::default(),
        )
        .unwrap();
    session
}

fn single_open_tool_pair() -> Arc<Session> {
    let session = Session::create(&SessionId::new("single-tool-pair"), None, None).unwrap();
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .unwrap();
    session
        .append(
            "step/start",
            json!({"turn": 1, "step": 1}),
            AppendOptions::default(),
        )
        .unwrap();
    append_header(&session, MODEL, MODEL, None, "initial");
    append_tool_exchange(&session, 1, "single-call", 1);
    session
        .append(
            "step/end",
            json!({"turn": 1, "step": 1}),
            AppendOptions::default(),
        )
        .unwrap();
    session
}

fn oversized_tool_result(chars: usize, compactable_prompt: bool) -> Arc<Session> {
    let session = Session::create(
        &SessionId::new(format!("oversized-tool-{chars}")),
        None,
        None,
    )
    .unwrap();
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .unwrap();
    if compactable_prompt {
        append_surface(
            &session,
            "user/message",
            serde_json::to_value(Message::user(
                vec![ContentBlock::Text {
                    text: "older history ".repeat(200),
                }],
                MessageSource::user(),
            ))
            .unwrap(),
        );
    }
    session
        .append(
            "step/start",
            json!({"turn": 1, "step": 1}),
            AppendOptions::default(),
        )
        .unwrap();
    append_header(&session, MODEL, MODEL, None, "initial");
    let call_id = CallId::new("oversized");
    append_surface(
        &session,
        "assistant/message",
        json!({
            "turn": 1,
            "step": 1,
            "message": Message::assistant(
                vec![ContentBlock::ToolCall {
                    id: call_id.clone(),
                    name: "bash".to_owned(),
                    arguments: "{}".to_owned(),
                }],
                MODEL,
                MODEL,
            )
        }),
    );
    session
        .append(
            "tool/call",
            json!({
                "turn": 1,
                "step": 1,
                "callId": call_id,
                "name": "bash",
                "arguments": "{}"
            }),
            AppendOptions::default(),
        )
        .unwrap();
    append_surface(
        &session,
        "tool/result",
        json!({
            "turn": 1,
            "step": 1,
            "message": Message::tool_result(
                &call_id,
                vec![ContentBlock::Text { text: "X".repeat(chars) }],
                false,
            ),
            "meta": {"presentation": "preserved"}
        }),
    );
    session
        .append(
            "step/end",
            json!({"turn": 1, "step": 1}),
            AppendOptions::default(),
        )
        .unwrap();
    session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .unwrap();
    session
        .append("turn/start", json!({"turn": 2}), AppendOptions::default())
        .unwrap();
    session
}

fn summarized_text(input: &SummarizationInput) -> String {
    fn blocks(input: &[ContentBlock], output: &mut Vec<String>) {
        for block in input {
            match block {
                ContentBlock::Text { text } => output.push(text.clone()),
                ContentBlock::ToolResult { content, .. } => blocks(content, output),
                _ => {}
            }
        }
    }
    let mut output = Vec::new();
    for message in &input.messages {
        blocks(message.content(), &mut output);
    }
    output.join("\n")
}

fn append_header(
    session: &Session,
    provider: &str,
    model: &str,
    system: Option<String>,
    reason: &str,
) {
    let mut header = json!({"config": {"provider": provider, "model": model}});
    if let Some(system) = system {
        header["system"] = json!(system);
    }
    session
        .append(
            "request/header",
            json!({"header": header, "reason": reason}),
            AppendOptions::default(),
        )
        .unwrap();
}

fn agent(
    session: Arc<Session>,
    provider: Option<&str>,
    model: Option<&str>,
) -> CompactionAgentContext {
    CompactionAgentContext {
        session,
        options: CompactionRoutingOptions {
            provider: provider.map(str::to_owned),
            model: model.map(str::to_owned),
        },
    }
}

fn live_agent(
    context: &Context,
    session: Arc<Session>,
    provider: Option<&str>,
    model: Option<&str>,
) -> Arc<Agent> {
    let inbox = Arc::new(
        Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).expect("agent inbox"),
    );
    Arc::new(Agent::new(
        session.id().clone(),
        AgentOptions {
            provider: provider.map(ProviderId::new),
            model: model.map(ModelId::new),
            ..AgentOptions::default()
        },
        session,
        inbox,
        context.clone(),
        ScopeKey::new(),
    ))
}

async fn pre_step(
    context: &Context,
    owner: &Arc<Agent>,
    signal: AbortSignal,
) -> anyhow::Result<PreStepDecision> {
    AgentEvents::new(context.clone(), owner.clone())
        .waterfall(
            "agent/pre-step",
            AgentPreStepEvent {
                messages: Vec::new(),
                turn: 1,
                step: 1,
                signal,
            },
            || async {
                Ok(PreStepDecision::Enter {
                    messages: Vec::new(),
                })
            },
        )
        .await
}

async fn recover(
    context: &Context,
    owner: &Arc<Agent>,
    code: &str,
    signal: AbortSignal,
) -> anyhow::Result<RequestErrorAction> {
    AgentEvents::new(context.clone(), owner.clone())
        .waterfall(
            "agent/request-error",
            AgentRequestErrorEvent {
                turn: 1,
                step: 1,
                provider: ProviderId::new("test"),
                failure: LlmFailure {
                    message: "provider failure".to_owned(),
                    code: code.to_owned(),
                    status: None,
                    provider_retry_after_ms: None,
                    request_id: None,
                },
                retry_policy: None,
                signal,
            },
            || async { Ok(RequestErrorAction::Terminal) },
        )
        .await
}

async fn recover_with_counter(
    context: &Context,
    owner: &Arc<Agent>,
    code: &str,
    signal: AbortSignal,
    calls: Arc<AtomicUsize>,
) -> anyhow::Result<RequestErrorAction> {
    AgentEvents::new(context.clone(), owner.clone())
        .waterfall(
            "agent/request-error",
            AgentRequestErrorEvent {
                turn: 1,
                step: 1,
                provider: ProviderId::new("test"),
                failure: LlmFailure {
                    message: "provider failure".to_owned(),
                    code: code.to_owned(),
                    status: None,
                    provider_retry_after_ms: None,
                    request_id: None,
                },
                retry_policy: None,
                signal,
            },
            move || async move {
                calls.fetch_add(1, Ordering::AcqRel);
                Ok(RequestErrorAction::Terminal)
            },
        )
        .await
}

fn pressure_config() -> BasicCompactionConfig {
    BasicCompactionConfig {
        auto: Some(false),
        threshold_ratio: Some(0.5),
        retain_tokens: Some(180),
        ..BasicCompactionConfig::default()
    }
}

#[tokio::test]
async fn skips_headerless_session_instead_of_using_agent_options() {
    let harness = Harness::new(pressure_config(), &[(MODEL, Some(1000))]);
    let session = Session::create(&SessionId::new("headerless"), None, None).unwrap();
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .unwrap();
    let result = harness
        .engine
        .compact_if_needed(
            &agent(session, Some(MODEL), Some(MODEL)),
            CompactionTrigger::Pressure,
            &AbortSignal::default(),
        )
        .await
        .unwrap();
    assert!(result.is_none());
    assert!(harness.summary.calls.lock().is_empty());
    harness.dispose().await;
}

#[tokio::test]
async fn meters_unlisted_model_when_provider_supplies_context_metadata() {
    let harness = Harness::new(pressure_config(), &[("unlisted-provider", Some(1000))]);
    let session = conversation(4, &"fixture ".repeat(40));
    append_header(
        &session,
        "unlisted-provider",
        "unlisted-model",
        None,
        "resume",
    );
    let result = harness
        .engine
        .compact_if_needed(
            &agent(session, Some(MODEL), Some(MODEL)),
            CompactionTrigger::Pressure,
            &AbortSignal::default(),
        )
        .await
        .unwrap();
    assert!(result.is_some());
    harness.dispose().await;
}

#[tokio::test]
async fn forwards_turn_cancellation_to_model_metadata_resolution() {
    let harness = Harness::new(pressure_config(), &[(MODEL, Some(1000))]);
    let signal = AbortSignal::default();
    let result = harness
        .engine
        .compact_if_needed(
            &agent(
                conversation(4, &"fixture ".repeat(40)),
                Some(MODEL),
                Some(MODEL),
            ),
            CompactionTrigger::Pressure,
            &signal,
        )
        .await
        .unwrap();
    assert!(result.is_some());
    signal.abort();
    assert!(
        harness.adapter_signals.lock()[0]
            .as_ref()
            .is_some_and(AbortSignal::is_aborted)
    );
    harness.dispose().await;
}

#[tokio::test]
async fn re_resolves_capacity_after_same_model_provider_switch() {
    let harness = Harness::new(
        BasicCompactionConfig {
            auto: Some(false),
            threshold_ratio: Some(0.5),
            retain_ratio: Some(0.1),
            ..BasicCompactionConfig::default()
        },
        &[("large", Some(10_000)), ("small", Some(1000))],
    );
    let session = conversation(4, &"fixture ".repeat(40));
    append_header(&session, "large", "shared-id", None, "resume");
    let signal = AbortSignal::default();
    assert!(
        harness
            .engine
            .compact_if_needed(
                &agent(session.clone(), Some(MODEL), Some(MODEL)),
                CompactionTrigger::Pressure,
                &signal,
            )
            .await
            .unwrap()
            .is_none()
    );
    append_header(&session, "small", "shared-id", None, "change");
    assert!(
        harness
            .engine
            .compact_if_needed(
                &agent(session, Some(MODEL), Some(MODEL)),
                CompactionTrigger::Pressure,
                &signal,
            )
            .await
            .unwrap()
            .is_some()
    );
    harness.dispose().await;
}

#[tokio::test]
async fn requires_capacity_only_for_proactive_pressure() {
    let harness = Harness::new(pressure_config(), &[("unknown-context", None)]);
    let session = conversation(4, &"fixture ".repeat(40));
    append_header(&session, "unknown-context", "model", None, "resume");
    let context = agent(session.clone(), Some(MODEL), Some(MODEL));
    let signal = AbortSignal::default();
    let error = harness
        .engine
        .compact_if_needed(&context, CompactionTrigger::Pressure, &signal)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("no context capacity for unknown-context/model")
    );
    assert!(
        harness
            .engine
            .compact_if_needed(&context, CompactionTrigger::ContextOverflow, &signal)
            .await
            .unwrap()
            .is_some()
    );
    harness.dispose().await;
}

#[tokio::test]
async fn forced_overflow_declines_one_indivisible_tool_pair() {
    let harness = Harness::new(pressure_config(), &[(MODEL, Some(1000))]);
    let session = single_open_tool_pair();
    let generation = session.replace_generation();
    let result = harness
        .engine
        .compact_if_needed(
            &agent(session.clone(), Some(MODEL), Some(MODEL)),
            CompactionTrigger::ContextOverflow,
            &AbortSignal::default(),
        )
        .await
        .unwrap();
    assert!(result.is_none());
    assert_eq!(session.replace_generation(), generation);
    assert!(
        session
            .events()
            .iter()
            .all(|event| event.event_type != "compaction/start")
    );
    harness.dispose().await;
}

#[tokio::test]
async fn below_threshold_is_inert_and_priced_head_compacts_above_threshold() {
    let harness = Harness::new(pressure_config(), &[(MODEL, Some(1000))]);
    let signal = AbortSignal::default();
    assert!(
        harness
            .engine
            .compact_if_needed(
                &agent(
                    conversation(2, &"fixture ".repeat(40)),
                    Some(MODEL),
                    Some(MODEL),
                ),
                CompactionTrigger::Pressure,
                &signal,
            )
            .await
            .unwrap()
            .is_none()
    );
    let session = conversation(4, &"fixture ".repeat(40));
    let before = session.surface_nodes().len();
    let result = harness
        .engine
        .compact_if_needed(
            &agent(session.clone(), Some(MODEL), Some(MODEL)),
            CompactionTrigger::Pressure,
            &signal,
        )
        .await
        .unwrap()
        .expect("compacted");
    assert!(result.shadowed_seqs.len() > 2);
    assert!(session.surface_nodes().len() < before);
    harness.dispose().await;
}

#[tokio::test]
async fn durable_request_envelope_counts_without_entering_surface() {
    let harness = Harness::new(
        BasicCompactionConfig {
            auto: Some(false),
            threshold_ratio: Some(0.9),
            retain_tokens: Some(50),
            ..BasicCompactionConfig::default()
        },
        &[(MODEL, Some(1000))],
    );
    let session = conversation(2, &"x".repeat(600));
    let signal = AbortSignal::default();
    assert!(
        harness
            .engine
            .compact_if_needed(
                &agent(session.clone(), Some(MODEL), Some(MODEL)),
                CompactionTrigger::Pressure,
                &signal,
            )
            .await
            .unwrap()
            .is_none()
    );
    append_header(&session, MODEL, MODEL, Some("s".repeat(2000)), "resume");
    assert!(
        harness
            .engine
            .compact_if_needed(
                &agent(session, Some(MODEL), Some(MODEL)),
                CompactionTrigger::Pressure,
                &signal,
            )
            .await
            .unwrap()
            .is_some()
    );
    harness.dispose().await;
}

#[tokio::test]
async fn high_envelope_pressure_without_compactable_surface_declines() {
    let harness = Harness::new(pressure_config(), &[(MODEL, Some(1000))]);
    let signal = AbortSignal::default();
    for (id, session) in [
        (
            "empty",
            Session::create(&SessionId::new("empty"), None, None).unwrap(),
        ),
        ("retained", conversation(1, &"fixture ".repeat(40))),
    ] {
        if id == "empty" {
            session
                .append("turn/start", json!({"turn": 1}), AppendOptions::default())
                .unwrap();
        }
        append_header(&session, MODEL, MODEL, Some("x".repeat(100_000)), "resume");
        assert!(
            harness
                .engine
                .compact_if_needed(
                    &agent(session, Some(MODEL), Some(MODEL)),
                    CompactionTrigger::Pressure,
                    &signal,
                )
                .await
                .unwrap()
                .is_none()
        );
    }
    harness.dispose().await;
}

#[tokio::test]
async fn bounded_retry_reports_checkpoint_still_above_threshold() {
    let harness = Harness::new(
        BasicCompactionConfig {
            auto: Some(false),
            compaction_retries: Some(0),
            threshold_ratio: Some(0.3),
            retain_tokens: Some(180),
            ..BasicCompactionConfig::default()
        },
        &[(MODEL, Some(1000))],
    );
    *harness.summary.summary.lock() = (0..7)
        .map(|index| ContentBlock::Text {
            text: format!("summary {index}"),
        })
        .collect();
    let error = harness
        .engine
        .compact_if_needed(
            &agent(
                conversation(4, &"fixture ".repeat(40)),
                Some(MODEL),
                Some(MODEL),
            ),
            CompactionTrigger::Pressure,
            &AbortSignal::default(),
        )
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("still above threshold after 1 compaction attempts")
    );
    harness.dispose().await;
}

#[tokio::test]
async fn retention_cut_rounds_headward_to_preserve_tool_pairing() {
    let harness = Harness::new(
        BasicCompactionConfig {
            auto: Some(false),
            threshold_ratio: Some(0.8),
            retain_tokens: Some(80),
            ..BasicCompactionConfig::default()
        },
        &[(MODEL, Some(4000))],
    );
    let session = tool_conversation(3, 300);
    assert!(
        harness
            .engine
            .compact_if_needed(
                &agent(session.clone(), Some(MODEL), Some(MODEL)),
                CompactionTrigger::Pressure,
                &AbortSignal::default(),
            )
            .await
            .unwrap()
            .is_some()
    );
    let mut calls = Vec::new();
    for message in session.derive_messages() {
        for block in message.content() {
            match block {
                ContentBlock::ToolCall { id, .. } => {
                    calls.push(id.as_str().to_owned());
                }
                ContentBlock::ToolResult { tool_call_id, .. } => {
                    assert!(calls.iter().any(|call| call == tool_call_id.as_str()));
                }
                _ => {}
            }
        }
    }
    harness.dispose().await;
}

#[test]
fn rejects_priced_surface_that_is_not_current_positional_surface() {
    let context = Context::new();
    let meter = seekdeep_token_meter::install(&context, TokenMeterConfig::default()).unwrap();
    let session = conversation(2, &"fixture ".repeat(40));
    let mut measurement = meter.measure(&session, None).unwrap();
    measurement.nodes.remove(0);
    let error = select_compactable_range(&session, &measurement, 1).unwrap_err();
    assert!(error.to_string().contains("does not match"));
}

#[test]
fn rounding_declines_when_it_would_consume_the_only_tool_pair() {
    let context = Context::new();
    let meter = seekdeep_token_meter::install(&context, TokenMeterConfig::default()).unwrap();
    let session = single_open_tool_pair();
    let measurement = meter.measure(&session, None).unwrap();
    assert!(
        select_compactable_range(&session, &measurement, 1)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn below_pressure_does_not_prune_opportunistically() {
    let harness = Harness::new(
        BasicCompactionConfig {
            auto: Some(false),
            threshold_ratio: Some(0.8),
            retain_tokens: Some(100),
            ..BasicCompactionConfig::default()
        },
        &[(MODEL, Some(10_000))],
    );
    harness.install_pruner();
    let session = oversized_tool_result(3000, false);
    assert!(
        harness
            .engine
            .compact_if_needed(
                &agent(session.clone(), Some(MODEL), Some(MODEL)),
                CompactionTrigger::Pressure,
                &AbortSignal::default(),
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(harness.summary.calls.lock().is_empty());
    assert_eq!(session.replace_generation(), 0);
    harness.dispose().await;
}

#[tokio::test]
async fn pruning_alone_can_clear_pressure_without_summarization() {
    let harness = Harness::new(
        BasicCompactionConfig {
            auto: Some(false),
            threshold_ratio: Some(0.5),
            retain_tokens: Some(50),
            ..BasicCompactionConfig::default()
        },
        &[(MODEL, Some(1000))],
    );
    harness.install_pruner();
    let session = oversized_tool_result(3000, false);
    assert!(harness.meter.measure(&session, None).unwrap().total_tokens >= 500);
    assert!(
        harness
            .engine
            .compact_if_needed(
                &agent(session.clone(), Some(MODEL), Some(MODEL)),
                CompactionTrigger::Pressure,
                &AbortSignal::default(),
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(harness.meter.measure(&session, None).unwrap().total_tokens < 500);
    assert!(harness.summary.calls.lock().is_empty());
    assert_eq!(session.replace_generation(), 1);
    harness.dispose().await;
}

#[tokio::test]
async fn insufficient_pruning_summarizes_the_pruned_surface() {
    let harness = Harness::new(
        BasicCompactionConfig {
            auto: Some(false),
            threshold_ratio: Some(0.5),
            retain_tokens: Some(50),
            ..BasicCompactionConfig::default()
        },
        &[(MODEL, Some(2000))],
    );
    harness.install_pruner();
    let session = tool_conversation(3, 300);
    assert!(
        harness
            .engine
            .compact_if_needed(
                &agent(session, Some(MODEL), Some(MODEL)),
                CompactionTrigger::Pressure,
                &AbortSignal::default(),
            )
            .await
            .unwrap()
            .is_some()
    );
    {
        let calls = harness.summary.calls.lock();
        assert_eq!(calls.len(), 1);
        let text = summarized_text(&calls[0]);
        assert!(text.contains(PRUNE_MARKER.trim()));
        assert!(!text.contains(&"result 1 ".repeat(300)));
    }
    harness.dispose().await;
}

#[tokio::test]
async fn absent_pruner_preserves_original_tool_result_behavior() {
    let harness = Harness::new(
        BasicCompactionConfig {
            auto: Some(false),
            threshold_ratio: Some(0.5),
            retain_tokens: Some(50),
            ..BasicCompactionConfig::default()
        },
        &[(MODEL, Some(2000))],
    );
    let session = oversized_tool_result(3000, true);
    assert!(
        harness
            .engine
            .compact_if_needed(
                &agent(session.clone(), Some(MODEL), Some(MODEL)),
                CompactionTrigger::Pressure,
                &AbortSignal::default(),
            )
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(harness.summary.calls.lock().len(), 1);
    let tool_results = session
        .events()
        .into_iter()
        .filter(|event| event.event_type == "tool/result")
        .collect::<Vec<_>>();
    assert_eq!(tool_results.len(), 1);
    assert_eq!(
        tool_results[0]
            .data
            .pointer("/message/content/0/content/0/text")
            .and_then(serde_json::Value::as_str),
        Some("X".repeat(3000).as_str())
    );
    assert!(matches!(
        tool_results[0].surface_op,
        Some(SurfaceOp::Marker(_))
    ));
    harness.dispose().await;
}

fn region_harness() -> Harness {
    Harness::new(
        BasicCompactionConfig {
            auto: Some(false),
            ..BasicCompactionConfig::default()
        },
        &[(MODEL, Some(1000))],
    )
}

#[tokio::test]
async fn region_lands_framed_replayable_checkpoint_with_exact_provenance() {
    let harness = region_harness();
    let raw_output = vec![
        ContentBlock::Reasoning {
            text: "private compact thought".to_owned(),
        },
        ContentBlock::Text {
            text: "small checkpoint".to_owned(),
        },
    ];
    *harness.summary.raw_output.lock() = Some(raw_output.clone());
    *harness.summary.usage.lock() = Some(TokenUsage {
        input_tokens: 40,
        output_tokens: 5,
        cache_read_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
    });
    let session = conversation(3, &"fixture ".repeat(40));
    let before = session.surface_nodes();
    let signal = AbortSignal::default();
    let result = harness
        .engine
        .compact_region(
            before[0],
            before[3],
            &agent(session.clone(), Some(MODEL), Some(MODEL)),
            Some(&signal),
        )
        .await
        .unwrap();
    assert_eq!(result.shadowed_seqs, before[..4]);
    assert!(result.shadowed_token_count > 0);
    signal.abort();
    assert!(
        harness.summary.signals.lock()[0]
            .as_ref()
            .is_some_and(AbortSignal::is_aborted)
    );
    assert!(summarized_text(&harness.summary.calls.lock()[0]).contains("fixture user 1"));
    let summary = session
        .events()
        .into_iter()
        .rev()
        .find(|event| event.event_type == "compaction/summary")
        .unwrap();
    assert_eq!(summary.data["shadowedSeqs"], json!(result.shadowed_seqs));
    assert_eq!(
        summary.data["shadowedTokenCount"],
        result.shadowed_token_count
    );
    assert_eq!(summary.data["provider"], "summary-provider");
    assert_eq!(summary.data["model"], "summary-model");
    assert_eq!(summary.data["maxTokens"], 123);
    assert_eq!(summary.data["rawOutput"], json!(raw_output));
    assert_eq!(summary.data["usage"]["inputTokens"], 40);
    assert!(summary.data.get("llmStreamCall").is_none());
    let derived = session.derive_messages();
    let ContentBlock::Text { text } = &derived[0].content()[0] else {
        panic!("checkpoint preamble must be text");
    };
    assert!(text.contains("<compacted-summary>"));
    assert_eq!(
        derived[0].content().last(),
        Some(&ContentBlock::Text {
            text: "</compacted-summary>".to_owned()
        })
    );
    let replay = Session::create(&SessionId::new("replay"), Some(session.events()), None).unwrap();
    assert_eq!(replay.derive_messages(), session.derive_messages());
    harness.dispose().await;
}

#[tokio::test]
async fn region_replays_latest_routed_header_into_summarizer_input() {
    let harness = region_harness();
    let session = conversation(3, &"fixture ".repeat(40));
    session
        .append(
            "request/header",
            json!({
                "header": {
                    "config": {"provider": MODEL, "model": MODEL},
                    "system": "CONVERSATION SYSTEM",
                    "tools": [{"name": "do_thing", "description": "d", "parameters": {"type": "object"}}]
                },
                "reason": "resume"
            }),
            AppendOptions::default(),
        )
        .unwrap();
    let nodes = session.surface_nodes();
    harness
        .engine
        .compact_region(
            nodes[0],
            nodes[1],
            &agent(session, Some(MODEL), Some(MODEL)),
            Some(&AbortSignal::default()),
        )
        .await
        .unwrap();
    {
        let calls = harness.summary.calls.lock();
        assert_eq!(calls[0].system.as_deref(), Some("CONVERSATION SYSTEM"));
        assert_eq!(calls[0].tools.as_ref().unwrap()[0].name, "do_thing");
        assert!(summarized_text(&calls[0]).contains("fixture user 1"));
    }
    harness.dispose().await;
}

#[tokio::test]
async fn region_rejects_missing_reversed_and_unbalanced_boundaries() {
    let harness = region_harness();
    let plain = conversation(2, &"fixture ".repeat(40));
    let nodes = plain.surface_nodes();
    for (start, end, expected) in [
        (9001, nodes[1], "start seq 9001 not found"),
        (nodes[0], 9002, "end seq 9002 not found"),
        (nodes[2], nodes[1], "is after end"),
    ] {
        let error = harness
            .engine
            .compact_region(
                start,
                end,
                &agent(plain.clone(), Some(MODEL), Some(MODEL)),
                None,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }
    let tools = tool_conversation(3, 1);
    let nodes = tools.surface_nodes();
    for (start, end, expected) in [
        (nodes[2], nodes[4], "not a balanced boundary"),
        (nodes[0], nodes[1], "not a balanced boundary"),
    ] {
        let error = harness
            .engine
            .compact_region(
                start,
                end,
                &agent(tools.clone(), Some(MODEL), Some(MODEL)),
                None,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }
    harness.dispose().await;
}

#[tokio::test]
async fn region_requires_open_turn_and_idle_compaction_bracket() {
    let harness = region_harness();
    let closed = conversation(1, &"fixture ".repeat(40));
    closed
        .append(
            "turn/end",
            json!({"turn": 2, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .unwrap();
    let nodes = closed.surface_nodes();
    let error = harness
        .engine
        .compact_region(
            nodes[0],
            nodes[1],
            &agent(closed, Some(MODEL), Some(MODEL)),
            None,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no open turn"));

    let locked = conversation(1, &"fixture ".repeat(40));
    locked
        .append(
            "compaction/start",
            json!({"compactionId": "locked-compaction", "turn": 2}),
            AppendOptions::default(),
        )
        .unwrap();
    let nodes = locked.surface_nodes();
    let error = harness
        .engine
        .compact_region(
            nodes[0],
            nodes[1],
            &agent(locked, Some(MODEL), Some(MODEL)),
            None,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("already in progress"));
    harness.dispose().await;
}

#[tokio::test]
async fn region_rejects_session_with_no_turn_boundary() {
    let harness = region_harness();
    let session = Session::create(&SessionId::new("turnless"), None, None).unwrap();
    append_surface(
        &session,
        "user/message",
        serde_json::to_value(Message::user(
            vec![ContentBlock::Text {
                text: "orphan".to_owned(),
            }],
            MessageSource::user(),
        ))
        .unwrap(),
    );
    let node = session.surface_nodes()[0];
    let error = harness
        .engine
        .compact_region(node, node, &agent(session, Some(MODEL), Some(MODEL)), None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no open turn"));
    harness.dispose().await;
}

#[tokio::test]
async fn region_records_summary_failure_without_mutating_surface() {
    let harness = region_harness();
    *harness.summary.error.lock() = Some("summary unavailable".to_owned());
    let session = conversation(2, &"fixture ".repeat(40));
    let before = session.surface_nodes();
    let error = harness
        .engine
        .compact_region(
            before[0],
            before[2],
            &agent(session.clone(), Some(MODEL), Some(MODEL)),
            None,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("summary unavailable"));
    assert_eq!(session.surface_nodes(), before);
    let end = session
        .events()
        .into_iter()
        .rev()
        .find(|event| event.event_type == "compaction/end")
        .unwrap();
    assert!(
        end.data["error"]
            .to_string()
            .contains("summary unavailable")
    );
    harness.dispose().await;
}

#[tokio::test]
async fn region_tolerates_log_only_mutation_but_rejects_surface_mutation() {
    let log_harness = region_harness();
    let log_session = conversation(2, &"fixture ".repeat(40));
    let mutate_session = log_session.clone();
    *log_harness.summary.mutate.lock() = Some(Arc::new(move || {
        append_header(&mutate_session, MODEL, MODEL, None, "change");
    }));
    let nodes = log_session.surface_nodes();
    let result = log_harness
        .engine
        .compact_region(
            nodes[0],
            nodes[2],
            &agent(log_session, Some(MODEL), Some(MODEL)),
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.shadowed_seqs, nodes[..3]);
    log_harness.dispose().await;

    let surface_harness = region_harness();
    let surface_session = conversation(2, &"fixture ".repeat(40));
    let mutate_session = surface_session.clone();
    *surface_harness.summary.mutate.lock() = Some(Arc::new(move || {
        append_surface(
            &mutate_session,
            "user/message",
            serde_json::to_value(Message::user(
                vec![ContentBlock::Text {
                    text: "concurrent surface mutation".to_owned(),
                }],
                MessageSource::plugin("test"),
            ))
            .unwrap(),
        );
    }));
    let nodes = surface_session.surface_nodes();
    let error = surface_harness
        .engine
        .compact_region(
            nodes[0],
            nodes[2],
            &agent(surface_session.clone(), Some(MODEL), Some(MODEL)),
            None,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("session surface changed"));
    assert!(
        surface_session
            .events()
            .iter()
            .all(|event| event.event_type != "compaction/summary")
    );
    surface_harness.dispose().await;
}

#[tokio::test]
async fn region_rejects_non_shrinking_framed_summary() {
    let harness = region_harness();
    *harness.summary.summary.lock() = (0..100)
        .map(|index| ContentBlock::Text {
            text: format!("verbose {index}"),
        })
        .collect();
    let session = conversation(2, &"fixture ".repeat(40));
    let nodes = session.surface_nodes();
    let error = harness
        .engine
        .compact_region(
            nodes[0],
            nodes[2],
            &agent(session.clone(), Some(MODEL), Some(MODEL)),
            None,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("summary is not smaller"));
    assert!(
        session
            .events()
            .iter()
            .all(|event| event.event_type != "compaction/summary")
    );
    harness.dispose().await;
}

#[tokio::test]
async fn custom_summarizer_compacts_without_conversation_model() {
    let harness = region_harness();
    let session = Session::create(&SessionId::new("model-less-region"), None, None).unwrap();
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .unwrap();
    append_surface(
        &session,
        "user/message",
        serde_json::to_value(Message::user(
            vec![ContentBlock::Text {
                text: "history ".repeat(100),
            }],
            MessageSource::user(),
        ))
        .unwrap(),
    );
    session
        .append(
            "step/start",
            json!({"turn": 1, "step": 1}),
            AppendOptions::default(),
        )
        .unwrap();
    append_surface(
        &session,
        "assistant/message",
        json!({
            "turn": 1,
            "step": 1,
            "message": Message::assistant(
                vec![ContentBlock::Text { text: "answer ".repeat(100) }],
                "historical",
                "historical",
            )
        }),
    );
    session
        .append(
            "step/end",
            json!({"turn": 1, "step": 1}),
            AppendOptions::default(),
        )
        .unwrap();
    let nodes = session.surface_nodes();
    let result = harness
        .engine
        .compact_region(nodes[0], nodes[1], &agent(session, None, None), None)
        .await
        .unwrap();
    assert_eq!(result.shadowed_seqs, nodes);
    harness.dispose().await;
}

#[tokio::test]
async fn automatic_pre_step_compacts_pressure_and_skips_aborted_or_small_steps() {
    let harness = Harness::new(
        BasicCompactionConfig {
            threshold_ratio: Some(0.5),
            retain_tokens: Some(180),
            ..BasicCompactionConfig::default()
        },
        &[(MODEL, Some(1000))],
    );
    let pressured = conversation(4, &"fixture ".repeat(40));
    let owner = live_agent(
        &harness.context,
        pressured.clone(),
        Some("unconfigured-agent-fallback"),
        Some("unconfigured-agent-fallback"),
    );
    assert_eq!(
        pre_step(&harness.context, &owner, AbortSignal::default())
            .await
            .unwrap(),
        PreStepDecision::Enter {
            messages: Vec::new()
        }
    );
    assert!(
        pressured
            .events()
            .iter()
            .any(|event| event.event_type == "compaction/summary")
    );

    let small = conversation(1, &"fixture ".repeat(40));
    let small_owner = live_agent(&harness.context, small.clone(), Some(MODEL), Some(MODEL));
    pre_step(&harness.context, &small_owner, AbortSignal::default())
        .await
        .unwrap();
    assert!(
        small
            .events()
            .iter()
            .all(|event| event.event_type != "compaction/start")
    );

    let aborted = conversation(4, &"fixture ".repeat(40));
    let aborted_owner = live_agent(&harness.context, aborted.clone(), Some(MODEL), Some(MODEL));
    let signal = AbortSignal::default();
    signal.abort();
    pre_step(&harness.context, &aborted_owner, signal)
        .await
        .unwrap();
    assert!(
        aborted
            .events()
            .iter()
            .all(|event| event.event_type != "compaction/start")
    );
    assert_eq!(harness.summary.calls.lock().len(), 1);
    harness.dispose().await;
}

#[tokio::test]
async fn canonical_overflow_forces_replacement_below_normal_pressure() {
    let harness = Harness::new(
        BasicCompactionConfig {
            threshold_ratio: Some(1.0),
            retain_tokens: Some(900),
            ..BasicCompactionConfig::default()
        },
        &[(MODEL, Some(10_000))],
    );
    let session = conversation(3, &"fixture ".repeat(40));
    assert!(harness.meter.measure(&session, None).unwrap().total_tokens < 10_000);
    let retained = *session.surface_nodes().last().unwrap();
    let generation = session.replace_generation();
    let owner = live_agent(&harness.context, session.clone(), Some(MODEL), Some(MODEL));
    assert_eq!(
        recover(
            &harness.context,
            &owner,
            CONTEXT_WINDOW_EXCEEDED_CODE,
            AbortSignal::default(),
        )
        .await
        .unwrap(),
        RequestErrorAction::Retry
    );
    assert_eq!(session.replace_generation(), generation + 1);
    assert!(session.surface_nodes().contains(&retained));
    assert!(
        session
            .events()
            .iter()
            .any(|event| event.event_type == "compaction/summary")
    );
    harness.dispose().await;
}

#[tokio::test]
async fn overflow_retry_accepts_prune_only_and_prune_then_summary_progress() {
    let prune_only = Harness::new(
        BasicCompactionConfig {
            threshold_ratio: Some(1.0),
            retain_tokens: Some(900),
            ..BasicCompactionConfig::default()
        },
        &[(MODEL, Some(10_000))],
    );
    prune_only.install_pruner();
    let session = oversized_tool_result(3000, false);
    let owner = live_agent(
        &prune_only.context,
        session.clone(),
        Some(MODEL),
        Some(MODEL),
    );
    assert_eq!(
        recover(
            &prune_only.context,
            &owner,
            CONTEXT_WINDOW_EXCEEDED_CODE,
            AbortSignal::default(),
        )
        .await
        .unwrap(),
        RequestErrorAction::Retry
    );
    assert_eq!(session.replace_generation(), 1);
    assert!(prune_only.summary.calls.lock().is_empty());
    assert!(
        session
            .events()
            .iter()
            .all(|event| event.event_type != "compaction/summary")
    );
    prune_only.dispose().await;

    let summarized = Harness::new(
        BasicCompactionConfig {
            threshold_ratio: Some(1.0),
            retain_tokens: Some(900),
            ..BasicCompactionConfig::default()
        },
        &[(MODEL, Some(10_000))],
    );
    summarized.install_pruner();
    let session = tool_conversation(3, 300);
    let owner = live_agent(
        &summarized.context,
        session.clone(),
        Some(MODEL),
        Some(MODEL),
    );
    assert_eq!(
        recover(
            &summarized.context,
            &owner,
            CONTEXT_WINDOW_EXCEEDED_CODE,
            AbortSignal::default(),
        )
        .await
        .unwrap(),
        RequestErrorAction::Retry
    );
    assert!(
        session
            .events()
            .iter()
            .any(|event| event.event_type == "compaction/summary")
    );
    {
        let calls = summarized.summary.calls.lock();
        assert_eq!(calls.len(), 1);
        assert!(summarized_text(&calls[0]).contains(PRUNE_MARKER.trim()));
    }
    summarized.dispose().await;
}

#[tokio::test]
async fn durable_prune_authorizes_retry_when_later_summary_fails_unless_cancelled() {
    let harness = Harness::new(
        BasicCompactionConfig {
            threshold_ratio: Some(1.0),
            retain_tokens: Some(900),
            ..BasicCompactionConfig::default()
        },
        &[(MODEL, Some(10_000))],
    );
    harness.install_pruner();
    *harness.summary.error.lock() = Some("summary unavailable after prune".to_owned());
    let session = oversized_tool_result(3000, true);
    let owner = live_agent(&harness.context, session.clone(), Some(MODEL), Some(MODEL));
    assert_eq!(
        recover(
            &harness.context,
            &owner,
            CONTEXT_WINDOW_EXCEEDED_CODE,
            AbortSignal::default(),
        )
        .await
        .unwrap(),
        RequestErrorAction::Retry
    );
    assert_eq!(session.replace_generation(), 1);
    assert_eq!(
        session
            .events()
            .iter()
            .filter(|event| event.event_type == "tool/result")
            .count(),
        2
    );
    harness.dispose().await;

    let cancelled = Harness::new(
        BasicCompactionConfig {
            threshold_ratio: Some(1.0),
            retain_tokens: Some(900),
            ..BasicCompactionConfig::default()
        },
        &[(MODEL, Some(10_000))],
    );
    cancelled.install_pruner();
    let signal = AbortSignal::default();
    let cancel_signal = signal.clone();
    *cancelled.summary.mutate.lock() = Some(Arc::new(move || cancel_signal.abort()));
    *cancelled.summary.error.lock() = Some("summary cancelled after prune".to_owned());
    let session = oversized_tool_result(3000, true);
    let owner = live_agent(
        &cancelled.context,
        session.clone(),
        Some(MODEL),
        Some(MODEL),
    );
    assert_eq!(
        recover(
            &cancelled.context,
            &owner,
            CONTEXT_WINDOW_EXCEEDED_CODE,
            signal,
        )
        .await
        .unwrap(),
        RequestErrorAction::Terminal
    );
    assert_eq!(session.replace_generation(), 1);
    cancelled.dispose().await;
}

#[tokio::test]
async fn overflow_retry_caps_are_independent_and_exact_target_overrides_win() {
    let harness = Harness::new(
        BasicCompactionConfig {
            max_overflow_retries: Some(1),
            ..BasicCompactionConfig::default()
        },
        &[(MODEL, Some(1000))],
    );
    let owner = live_agent(
        &harness.context,
        conversation(3, &"fixture ".repeat(40)),
        Some(MODEL),
        Some(MODEL),
    );
    assert_eq!(
        recover(
            &harness.context,
            &owner,
            "RATE_LIMIT",
            AbortSignal::default(),
        )
        .await
        .unwrap(),
        RequestErrorAction::Terminal
    );
    assert_eq!(
        recover(
            &harness.context,
            &owner,
            CONTEXT_WINDOW_EXCEEDED_CODE,
            AbortSignal::default(),
        )
        .await
        .unwrap(),
        RequestErrorAction::Retry
    );
    let calls = harness.summary.calls.lock().len();
    assert_eq!(
        recover(
            &harness.context,
            &owner,
            CONTEXT_WINDOW_EXCEEDED_CODE,
            AbortSignal::default(),
        )
        .await
        .unwrap(),
        RequestErrorAction::Terminal
    );
    assert_eq!(harness.summary.calls.lock().len(), calls);
    harness.dispose().await;

    let overridden = Harness::new(
        BasicCompactionConfig {
            max_overflow_retries: Some(2),
            model_policies: vec![seekdeep_compaction_basic::ModelCompactPolicyConfig {
                provider: MODEL.to_owned(),
                model: MODEL.to_owned(),
                max_overflow_retries: Some(1),
                ..seekdeep_compaction_basic::ModelCompactPolicyConfig::default()
            }],
            ..BasicCompactionConfig::default()
        },
        &[(MODEL, Some(1000))],
    );
    let owner = live_agent(
        &overridden.context,
        conversation(3, &"fixture ".repeat(40)),
        Some(MODEL),
        Some(MODEL),
    );
    assert_eq!(
        recover(
            &overridden.context,
            &owner,
            CONTEXT_WINDOW_EXCEEDED_CODE,
            AbortSignal::default(),
        )
        .await
        .unwrap(),
        RequestErrorAction::Retry
    );
    assert_eq!(
        recover(
            &overridden.context,
            &owner,
            CONTEXT_WINDOW_EXCEEDED_CODE,
            AbortSignal::default(),
        )
        .await
        .unwrap(),
        RequestErrorAction::Terminal
    );
    overridden.dispose().await;
}

#[tokio::test]
async fn zero_overflow_budget_keeps_pressure_listener_and_auto_false_removes_both() {
    let zero = Harness::new(
        BasicCompactionConfig {
            max_overflow_retries: Some(0),
            threshold_ratio: Some(0.5),
            retain_tokens: Some(180),
            ..BasicCompactionConfig::default()
        },
        &[(MODEL, Some(1000))],
    );
    let session = conversation(4, &"fixture ".repeat(40));
    let owner = live_agent(&zero.context, session.clone(), Some(MODEL), Some(MODEL));
    pre_step(&zero.context, &owner, AbortSignal::default())
        .await
        .unwrap();
    let summaries = session
        .events()
        .iter()
        .filter(|event| event.event_type == "compaction/summary")
        .count();
    assert_eq!(summaries, 1);
    assert_eq!(
        recover(
            &zero.context,
            &owner,
            CONTEXT_WINDOW_EXCEEDED_CODE,
            AbortSignal::default(),
        )
        .await
        .unwrap(),
        RequestErrorAction::Terminal
    );
    assert_eq!(
        session
            .events()
            .iter()
            .filter(|event| event.event_type == "compaction/summary")
            .count(),
        summaries
    );
    zero.dispose().await;

    let off = Harness::new(
        BasicCompactionConfig {
            auto: Some(false),
            threshold_ratio: Some(0.5),
            retain_tokens: Some(180),
            ..BasicCompactionConfig::default()
        },
        &[(MODEL, Some(1000))],
    );
    let session = conversation(4, &"fixture ".repeat(40));
    let owner = live_agent(&off.context, session.clone(), Some(MODEL), Some(MODEL));
    pre_step(&off.context, &owner, AbortSignal::default())
        .await
        .unwrap();
    assert!(
        session
            .events()
            .iter()
            .all(|event| event.event_type != "compaction/start")
    );
    assert_eq!(
        recover(
            &off.context,
            &owner,
            CONTEXT_WINDOW_EXCEEDED_CODE,
            AbortSignal::default(),
        )
        .await
        .unwrap(),
        RequestErrorAction::Terminal
    );
    off.dispose().await;
}

#[tokio::test]
async fn overflow_uses_durable_unlisted_route_and_headerless_or_failed_recovery_delegates_once() {
    let unlisted = Harness::new(BasicCompactionConfig::default(), &[(MODEL, Some(1000))]);
    let session = conversation(2, &"fixture ".repeat(40));
    append_header(
        &session,
        "unknown-routed-provider",
        "unknown-routed-model",
        None,
        "resume",
    );
    let owner = live_agent(&unlisted.context, session, Some(MODEL), Some(MODEL));
    assert_eq!(
        recover(
            &unlisted.context,
            &owner,
            CONTEXT_WINDOW_EXCEEDED_CODE,
            AbortSignal::default(),
        )
        .await
        .unwrap(),
        RequestErrorAction::Retry
    );
    unlisted.dispose().await;

    let headerless = Harness::new(BasicCompactionConfig::default(), &[(MODEL, Some(1000))]);
    let session = Session::create(&SessionId::new("headerless-overflow"), None, None).unwrap();
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .unwrap();
    let owner = live_agent(&headerless.context, session, Some(MODEL), Some(MODEL));
    let calls = Arc::new(AtomicUsize::new(0));
    assert_eq!(
        recover_with_counter(
            &headerless.context,
            &owner,
            CONTEXT_WINDOW_EXCEEDED_CODE,
            AbortSignal::default(),
            calls.clone(),
        )
        .await
        .unwrap(),
        RequestErrorAction::Terminal
    );
    assert_eq!(calls.load(Ordering::Acquire), 1);
    headerless.dispose().await;

    let failed = Harness::new(BasicCompactionConfig::default(), &[(MODEL, Some(1000))]);
    *failed.summary.error.lock() = Some("summary unavailable".to_owned());
    let session = conversation(3, &"fixture ".repeat(40));
    let generation = session.replace_generation();
    let owner = live_agent(&failed.context, session.clone(), Some(MODEL), Some(MODEL));
    let calls = Arc::new(AtomicUsize::new(0));
    assert_eq!(
        recover_with_counter(
            &failed.context,
            &owner,
            CONTEXT_WINDOW_EXCEEDED_CODE,
            AbortSignal::default(),
            calls.clone(),
        )
        .await
        .unwrap(),
        RequestErrorAction::Terminal
    );
    assert_eq!(calls.load(Ordering::Acquire), 1);
    assert_eq!(session.replace_generation(), generation);
    failed.dispose().await;
}

#[tokio::test]
async fn forced_overflow_preserves_newest_tool_pair_and_cancellation_prevents_retry() {
    let harness = Harness::new(BasicCompactionConfig::default(), &[(MODEL, Some(1000))]);
    let session = tool_conversation(3, 300);
    let nodes = session.surface_nodes();
    let newest_assistant = nodes[nodes.len() - 2];
    let newest_result = nodes[nodes.len() - 1];
    let owner = live_agent(&harness.context, session.clone(), Some(MODEL), Some(MODEL));
    assert_eq!(
        recover(
            &harness.context,
            &owner,
            CONTEXT_WINDOW_EXCEEDED_CODE,
            AbortSignal::default(),
        )
        .await
        .unwrap(),
        RequestErrorAction::Retry
    );
    assert!(session.surface_nodes().contains(&newest_assistant));
    assert!(session.surface_nodes().contains(&newest_result));
    assert!(tool_pairing_balanced_before(&session, newest_assistant).unwrap());
    assert!(tool_pairing_balanced_after(&session, newest_result).unwrap());
    harness.dispose().await;

    let cancelled = Harness::new(BasicCompactionConfig::default(), &[(MODEL, Some(1000))]);
    let signal = AbortSignal::default();
    let cancel = signal.clone();
    *cancelled.summary.mutate.lock() = Some(Arc::new(move || cancel.abort()));
    let session = conversation(3, &"fixture ".repeat(40));
    let generation = session.replace_generation();
    let owner = live_agent(
        &cancelled.context,
        session.clone(),
        Some(MODEL),
        Some(MODEL),
    );
    assert_eq!(
        recover(
            &cancelled.context,
            &owner,
            CONTEXT_WINDOW_EXCEEDED_CODE,
            signal,
        )
        .await
        .unwrap(),
        RequestErrorAction::Terminal
    );
    assert_eq!(session.replace_generation(), generation + 1);
    cancelled.dispose().await;
}

#[tokio::test]
async fn overflow_without_replacement_delegates_downstream_exactly_once() {
    let harness = Harness::new(BasicCompactionConfig::default(), &[(MODEL, Some(1000))]);
    let session = single_open_tool_pair();
    let generation = session.replace_generation();
    let owner = live_agent(&harness.context, session.clone(), Some(MODEL), Some(MODEL));
    let calls = Arc::new(AtomicUsize::new(0));
    assert_eq!(
        recover_with_counter(
            &harness.context,
            &owner,
            CONTEXT_WINDOW_EXCEEDED_CODE,
            AbortSignal::default(),
            calls.clone(),
        )
        .await
        .unwrap(),
        RequestErrorAction::Terminal
    );
    assert_eq!(calls.load(Ordering::Acquire), 1);
    assert_eq!(session.replace_generation(), generation);
    harness.dispose().await;
}

#[tokio::test]
async fn disposing_plugin_withdraws_service_and_both_automatic_listeners() {
    let context = Context::new();
    let llm = LlmRuntime::install(&context).unwrap();
    llm.register_adapter(
        &[MODEL.to_owned()],
        Arc::new(ContextAdapter {
            windows: BTreeMap::from([(MODEL.to_owned(), Some(1000))]),
            signals: Arc::new(Mutex::new(Vec::new())),
        }),
    )
    .unwrap();
    let _meter = seekdeep_token_meter::install(&context, TokenMeterConfig::default()).unwrap();
    let sessions = context
        .plugin(seekdeep_core::session_store::plugin(), json!({}))
        .unwrap();
    sessions.await_settled().await.unwrap();
    let compact = context
        .plugin(
            seekdeep_compaction_basic::plugin(),
            json!({"thresholdRatio": 0.5, "retainTokens": 180}),
        )
        .unwrap();
    compact.await_settled().await.unwrap();
    assert!(context.get(COMPACTION).is_some());
    compact.dispose().await.unwrap();
    assert!(context.get(COMPACTION).is_none());

    let session = conversation(4, &"fixture ".repeat(40));
    let owner = live_agent(&context, session.clone(), Some(MODEL), Some(MODEL));
    pre_step(&context, &owner, AbortSignal::default())
        .await
        .unwrap();
    assert!(
        session
            .events()
            .iter()
            .all(|event| event.event_type != "compaction/start")
    );
    assert_eq!(
        recover(
            &context,
            &owner,
            CONTEXT_WINDOW_EXCEEDED_CODE,
            AbortSignal::default(),
        )
        .await
        .unwrap(),
        RequestErrorAction::Terminal
    );
    context.fiber().dispose().await.unwrap();
}
