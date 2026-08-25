//! CBR-001 and context-overflow recovery through the real durable agent loop.

use std::sync::Arc;

use async_trait::async_trait;
use futures::{FutureExt as _, stream};
use parking_lot::Mutex;
use seekdeep_agent::{AgentOptions, AgentRegistry};
use seekdeep_agent_loop::{AgentLoopServices, DefaultAgentDriver, LoopAgent};
use seekdeep_compaction::tool_pairing::{
    tool_pairing_balanced_after, tool_pairing_balanced_before,
};
use seekdeep_compaction_basic::{
    BasicCompactionConfig, BasicCompactionEngine, region::RegionSummarize,
    summarizer::SummaryResult,
};
use seekdeep_cordis::{Context, EventOptions, EventReply};
use seekdeep_core::session::{AppendOptions, Session, SessionEvent, SessionId, SurfaceOp};
use seekdeep_llm::{
    AbortSignal, AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LlmAdapter,
    LlmCallConfig, LlmError, LlmFailure, LlmModelContext, LlmResolvedModelInfo, LlmRuntime,
    Message, MessageSource, ModelId, ProviderId, ResolvedRetryPolicy, StreamChunk, UserMessage,
    resolve_retry_policy,
};
use seekdeep_llm_retry::{RetryConfig, install as install_retry};
use seekdeep_system_prompt::{SystemPrompt, SystemPromptConfig};
use seekdeep_token_meter::{TokenMeterConfig, TokenMeterInstallation};
use seekdeep_tools::{
    ToolDefinition, ToolOutputDefinition, ToolRuntime, ToolRuntimeConfig,
    assert_supported_json_schema,
};
use serde_json::{Map, Value, json};

fn user(text: &str) -> UserMessage {
    UserMessage::new(
        vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        MessageSource::user(),
    )
}

fn text_success(text: &str, reason: FinishReason) -> Vec<StreamChunk> {
    vec![
        StreamChunk::BlockStart {
            index: 0,
            block_type: "text".to_owned(),
        },
        StreamChunk::TextDelta {
            index: 0,
            text: text.to_owned(),
        },
        StreamChunk::BlockEnd {
            index: 0,
            block: ContentBlock::Text {
                text: text.to_owned(),
            },
        },
        StreamChunk::Finish {
            reason,
            replay_state: None,
        },
    ]
}

#[derive(Debug)]
struct StepwiseToolAdapter {
    calls: Mutex<usize>,
    tool_steps: usize,
}

#[async_trait]
impl LlmAdapter for StepwiseToolAdapter {
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
                context_window: 400,
            }),
            default_max_tokens: None,
            reasoning: None,
        })
    }

    fn stream(&self, _options: GenerateOptions) -> AdapterStream {
        let call = {
            let mut calls = self.calls.lock();
            let call = *calls;
            *calls += 1;
            call
        };
        if call >= self.tool_steps {
            return AdapterStream::new(stream::iter(
                text_success("all done", FinishReason::Stop)
                    .into_iter()
                    .map(Ok),
            ));
        }
        let id = CallId::new(format!("c{call}"));
        let arguments = format!(r#"{{"i":{call}}}"#);
        AdapterStream::new(stream::iter(
            vec![
                StreamChunk::BlockStart {
                    index: 0,
                    block_type: "text".to_owned(),
                },
                StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::Text {
                        text: format!("step {call}"),
                    },
                },
                StreamChunk::BlockStart {
                    index: 1,
                    block_type: "tool-call".to_owned(),
                },
                StreamChunk::BlockEnd {
                    index: 1,
                    block: ContentBlock::ToolCall {
                        id,
                        name: "work".to_owned(),
                        arguments,
                    },
                },
                StreamChunk::Finish {
                    reason: FinishReason::ToolCalls,
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
    services: AgentLoopServices,
    _meter: TokenMeterInstallation,
    _compact: Arc<BasicCompactionEngine>,
}

impl LoopHarness {
    fn stepwise(tool_steps: usize) -> Self {
        let context = Context::new();
        let llm = LlmRuntime::install(&context).unwrap();
        llm.register_adapter(
            &["mock".to_owned()],
            Arc::new(StepwiseToolAdapter {
                calls: Mutex::new(0),
                tool_steps,
            }),
        )
        .unwrap();
        let system_prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
        let tools = ToolRuntime::new_with_system_prompt(
            &context,
            &system_prompt,
            ToolRuntimeConfig::default(),
        )
        .unwrap();
        register_work_tool(&context, &tools);
        let meter = seekdeep_token_meter::install(&context, TokenMeterConfig::default()).unwrap();
        let summarize: RegionSummarize = Arc::new(|_, _, _, _| {
            async {
                let summary = vec![ContentBlock::Text {
                    text: "CHECKPOINT SUMMARY".to_owned(),
                }];
                Ok(SummaryResult {
                    summary: summary.clone(),
                    raw_output: summary,
                    llm_stream_call: false,
                    provider: "mock".to_owned(),
                    model: "stub".to_owned(),
                    max_tokens: None,
                    usage: None,
                })
            }
            .boxed()
        });
        let compact = BasicCompactionEngine::new_with_summarizer(
            &context,
            &BasicCompactionConfig {
                threshold_ratio: Some(0.5),
                retain_tokens: Some(50),
                max_tokens: Some(8192),
                compaction_retries: Some(1),
                ..BasicCompactionConfig::default()
            },
            summarize,
        )
        .unwrap();
        Self {
            context,
            services: AgentLoopServices {
                llm,
                system_prompt,
                tools,
                max_parallel_tool_calls: 10,
            },
            _meter: meter,
            _compact: compact,
        }
    }

    fn agent(&self, id: &str, provider: &str, model: &str) -> (LoopAgent, Arc<DefaultAgentDriver>) {
        let session = Session::create(&SessionId::new(id), None, None).unwrap();
        LoopAgent::new_default(
            &self.context,
            &session,
            AgentOptions {
                provider: Some(provider.into()),
                model: Some(model.into()),
                ..AgentOptions::default()
            },
            None,
            self.services.clone(),
        )
        .unwrap()
    }
}

fn register_work_tool(context: &Context, tools: &Arc<ToolRuntime>) {
    tools
        .register(
            context,
            ToolDefinition::new(
                "work",
                "does work",
                Map::from_iter([("i".to_owned(), json!({"type": "number"}))]),
                ToolOutputDefinition::new(
                    Arc::new(assert_supported_json_schema(json!({"type": "string"})).unwrap()),
                    Arc::new(|_, value| {
                        Ok(vec![ContentBlock::Text {
                            text: value.as_str().unwrap_or_default().to_owned(),
                        }])
                    }),
                ),
                Arc::new(|_, _| Box::pin(async { Ok(Value::String("work result".to_owned())) })),
            ),
        )
        .unwrap();
}

fn reroute_agent_requests(context: &Context) {
    context
        .events()
        .on_waterfall(
            context,
            "agent/request",
            move |_, _, next| {
                Box::pin(async move {
                    let reply = next.run().await?;
                    let mut config = reply
                        .downcast::<LlmCallConfig>()
                        .map(|config| (*config).clone())
                        .ok_or_else(|| anyhow::anyhow!("agent/request did not return config"))?;
                    config.provider = ProviderId::new("mock");
                    config.model = ModelId::new("mock");
                    Ok(EventReply::Value(Arc::new(config)))
                })
            },
            EventOptions::default(),
        )
        .unwrap();
}

async fn run_agent(loop_agent: &LoopAgent, prompt: &str) {
    loop_agent.agent.followup(user(prompt)).unwrap();
    loop_agent.agent.when_idle().unwrap().await.unwrap();
}

#[tokio::test]
async fn routed_post_step_pressure_uses_durable_route_and_finishes_completed() {
    let harness = LoopHarness::stepwise(8);
    reroute_agent_requests(&harness.context);
    let (agent, _driver) = harness.agent(
        "routed-pressure",
        "unconfigured-agent-fallback",
        "unconfigured-agent-fallback",
    );
    run_agent(&agent, "do a routed multi-step task").await;

    assert_eq!(
        agent
            .agent
            .session()
            .request_header()
            .unwrap()
            .config
            .model
            .as_str(),
        "mock"
    );
    let events = agent.agent.session().events();
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "compaction/summary")
    );
    assert_eq!(events.last().unwrap().event_type, "turn/end");
    assert_eq!(events.last().unwrap().data["reason"]["kind"], "completed");
    harness.context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn pressure_runs_between_completed_tool_step_and_next_step() {
    let harness = LoopHarness::stepwise(8);
    let (agent, _driver) = harness.agent("post-step-order", "mock", "mock");
    run_agent(&agent, "do tool work").await;
    let events = agent.agent.session().events();
    let compact_start = events
        .iter()
        .find(|event| event.event_type == "compaction/start")
        .unwrap();
    let result = events
        .iter()
        .rev()
        .find(|event| event.event_type == "tool/result" && event.seq < compact_start.seq)
        .unwrap();
    let step = result.data["step"].as_u64().unwrap();
    let preceding_end = events
        .iter()
        .find(|event| {
            event.event_type == "step/end" && event.data["step"] == step && event.seq > result.seq
        })
        .unwrap();
    let next_start = events
        .iter()
        .find(|event| {
            event.event_type == "step/start"
                && event.data["step"] == step + 1
                && event.seq > compact_start.seq
        })
        .unwrap();
    assert!(result.seq < compact_start.seq);
    assert!(preceding_end.seq < compact_start.seq);
    assert!(compact_start.seq < next_start.seq);
    harness.context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn every_live_head_checkpoint_is_tool_balanced_on_both_sides() {
    let harness = LoopHarness::stepwise(8);
    let (agent, _driver) = harness.agent("cbr-001", "mock", "mock");
    run_agent(&agent, "do a long multi-step task").await;
    let session = agent.agent.session();
    let checkpoints = session
        .events()
        .into_iter()
        .filter(|event| {
            event.event_type == "user/message"
                && matches!(event.surface_op, Some(SurfaceOp::Replace(_)))
        })
        .collect::<Vec<_>>();
    assert!(!checkpoints.is_empty());
    let nodes = session.surface_nodes();
    for checkpoint in checkpoints {
        if !nodes.contains(&checkpoint.seq) {
            continue;
        }
        assert!(tool_pairing_balanced_before(session, checkpoint.seq).unwrap());
        assert!(tool_pairing_balanced_after(session, checkpoint.seq).unwrap());
    }
    harness.context.fiber().dispose().await.unwrap();
}

#[derive(Clone, Debug)]
struct RequestSnapshot {
    messages: Vec<Message>,
}

#[derive(Clone, Copy, Debug)]
enum OverflowDelivery {
    Thrown,
    InBand,
}

struct OverflowRecoveryAdapter {
    conversation: Mutex<Vec<RequestSnapshot>>,
    summary: Mutex<Vec<RequestSnapshot>>,
    delivery: OverflowDelivery,
    transient_after_overflow: bool,
    retry_policy: ResolvedRetryPolicy,
}

impl OverflowRecoveryAdapter {
    fn new(delivery: OverflowDelivery, transient_after_overflow: bool) -> Arc<Self> {
        Arc::new(Self {
            conversation: Mutex::new(Vec::new()),
            summary: Mutex::new(Vec::new()),
            delivery,
            transient_after_overflow,
            retry_policy: resolve_retry_policy(
                Some(&json!({
                    "mode": "normal",
                    "maxRetries": 1,
                    "retryableCodes": ["SERVER"],
                    "backoff": {
                        "initialDelayMs": 1,
                        "maxDelayMs": 1,
                        "jitterRatio": 0
                    }
                })),
                "compaction test provider retryPolicy",
            )
            .unwrap(),
        })
    }

    fn is_summary(options: &GenerateOptions) -> bool {
        options.messages.last().is_some_and(|message| {
            message.content().iter().any(|block| {
                matches!(block, ContentBlock::Text { text } if text.contains("acting as a compaction engine"))
            })
        })
    }
}

#[async_trait]
impl LlmAdapter for OverflowRecoveryAdapter {
    fn provider_retry_policy(&self, _provider: &str) -> Option<ResolvedRetryPolicy> {
        Some(self.retry_policy.clone())
    }

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
                context_window: 128,
            }),
            default_max_tokens: None,
            reasoning: None,
        })
    }

    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        let snapshot = RequestSnapshot {
            messages: options.messages.clone(),
        };
        if Self::is_summary(&options) {
            self.summary.lock().push(snapshot);
            return AdapterStream::new(stream::iter(
                text_success("RECOVERY CHECKPOINT", FinishReason::Stop)
                    .into_iter()
                    .map(Ok),
            ));
        }
        let request = {
            let mut requests = self.conversation.lock();
            requests.push(snapshot);
            requests.len()
        };
        if request == 1 {
            return match self.delivery {
                OverflowDelivery::Thrown => {
                    AdapterStream::new(stream::iter([Err(anyhow::Error::new(LlmError::simple(
                        "request too large for model context",
                        seekdeep_llm::CONTEXT_WINDOW_EXCEEDED_CODE,
                    )))]))
                }
                OverflowDelivery::InBand => {
                    AdapterStream::new(stream::iter([Ok(StreamChunk::Finish {
                        reason: FinishReason::Error {
                            failure: LlmFailure {
                                message: "request too large for model context".to_owned(),
                                code: seekdeep_llm::CONTEXT_WINDOW_EXCEEDED_CODE.to_owned(),
                                status: None,
                                provider_retry_after_ms: None,
                                request_id: None,
                            },
                        },
                        replay_state: None,
                    })]))
                }
            };
        }
        if self.transient_after_overflow && request == 2 {
            return AdapterStream::new(stream::iter([Err(anyhow::Error::new(LlmError::simple(
                "temporary provider outage",
                "SERVER",
            )))]));
        }
        AdapterStream::new(stream::iter(
            text_success("recovered", FinishReason::Stop)
                .into_iter()
                .map(Ok),
        ))
    }
}

fn overflow_seed() -> Vec<SessionEvent> {
    let session = Session::create(&SessionId::new("overflow-seed"), None, None).unwrap();
    for turn in 1..=2 {
        let sentinel = if turn == 1 {
            "OLD HISTORY SENTINEL"
        } else {
            "RECENT HISTORY"
        };
        session
            .append(
                "turn/start",
                json!({"turn": turn}),
                AppendOptions::default(),
            )
            .unwrap();
        session
            .append(
                "user/message",
                serde_json::to_value(Message::user(
                    vec![ContentBlock::Text {
                        text: format!("{sentinel} {}", "old context ".repeat(200)),
                    }],
                    MessageSource::user(),
                ))
                .unwrap(),
                AppendOptions {
                    surface_op: Some(SurfaceOp::append()),
                    ..AppendOptions::default()
                },
            )
            .unwrap();
        session
            .append(
                "step/start",
                json!({"turn": turn, "step": 1}),
                AppendOptions::default(),
            )
            .unwrap();
        session
            .append(
                "assistant/message",
                json!({
                    "turn": turn,
                    "step": 1,
                    "message": Message::assistant(
                        vec![ContentBlock::Text {
                            text: format!("historical response {turn} {}", "detail ".repeat(200)),
                        }],
                        "mock",
                        "mock",
                    )
                }),
                AppendOptions {
                    surface_op: Some(SurfaceOp::append()),
                    ..AppendOptions::default()
                },
            )
            .unwrap();
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
    session.events()
}

struct OverflowHarness {
    context: Context,
    adapter: Arc<OverflowRecoveryAdapter>,
    services: AgentLoopServices,
    _meter: TokenMeterInstallation,
    _compact: Arc<BasicCompactionEngine>,
    _retry: Option<Arc<seekdeep_cordis::PluginFiber>>,
}

impl OverflowHarness {
    async fn new(delivery: OverflowDelivery, transient: bool) -> Self {
        let context = Context::new();
        let retry = if transient {
            let agents = Arc::new(AgentRegistry::new(context.clone()));
            agents.provide(&context).unwrap();
            let retry = install_retry(&context, RetryConfig::default()).unwrap();
            retry.await_settled().await.unwrap();
            Some(retry)
        } else {
            None
        };
        let llm = LlmRuntime::install(&context).unwrap();
        let adapter = OverflowRecoveryAdapter::new(delivery, transient);
        llm.register_adapter(&["mock".to_owned()], adapter.clone())
            .unwrap();
        let system_prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
        let tools = ToolRuntime::new_with_system_prompt(
            &context,
            &system_prompt,
            ToolRuntimeConfig::default(),
        )
        .unwrap();
        let meter = seekdeep_token_meter::install(&context, TokenMeterConfig::default()).unwrap();
        let compact = BasicCompactionEngine::new(
            &context,
            &BasicCompactionConfig {
                threshold_ratio: Some(1.0),
                retain_tokens: Some(100),
                max_tokens: Some(64),
                compaction_retries: Some(0),
                max_overflow_retries: Some(1),
                ..BasicCompactionConfig::default()
            },
        )
        .unwrap();
        Self {
            context,
            adapter,
            services: AgentLoopServices {
                llm,
                system_prompt,
                tools,
                max_parallel_tool_calls: 10,
            },
            _meter: meter,
            _compact: compact,
            _retry: retry,
        }
    }

    fn agent(&self, id: &str, reroute: bool) -> (LoopAgent, Arc<DefaultAgentDriver>) {
        if reroute {
            reroute_agent_requests(&self.context);
        }
        let session = Session::create(&SessionId::new(id), Some(overflow_seed()), None).unwrap();
        LoopAgent::new_default(
            &self.context,
            &session,
            AgentOptions {
                provider: Some(if reroute {
                    "unconfigured-agent-fallback".into()
                } else {
                    "mock".into()
                }),
                model: Some(if reroute {
                    "unconfigured-agent-fallback".into()
                } else {
                    "mock".into()
                }),
                ..AgentOptions::default()
            },
            None,
            self.services.clone(),
        )
        .unwrap()
    }
}

fn messages_text(messages: &[Message]) -> String {
    messages
        .iter()
        .flat_map(Message::content)
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn assert_overflow_recovery(delivery: OverflowDelivery) {
    let harness = OverflowHarness::new(delivery, false).await;
    let (agent, _driver) = harness.agent("overflow", true);
    run_agent(&agent, "continue from history").await;
    assert_eq!(harness.adapter.conversation.lock().len(), 2);
    assert_eq!(harness.adapter.summary.lock().len(), 1);
    {
        let summary = harness.adapter.summary.lock();
        let instruction = messages_text(&summary[0].messages);
        assert!(instruction.contains("Write concise English engineering prose."));
        assert!(instruction.contains("numeric values, function signatures, and syntax fragments."));
    }
    {
        let conversation = harness.adapter.conversation.lock();
        assert!(messages_text(&conversation[0].messages).contains("OLD HISTORY SENTINEL"));
        let retry = messages_text(&conversation[1].messages);
        assert!(retry.contains("RECOVERY CHECKPOINT"));
        assert!(!retry.contains("OLD HISTORY SENTINEL"));
    }
    let events = agent.agent.session().events();
    let step_start = events
        .iter()
        .find(|event| {
            event.event_type == "step/start" && event.data["turn"] == 3 && event.data["step"] == 1
        })
        .unwrap();
    let step_end = events
        .iter()
        .find(|event| {
            event.event_type == "step/end" && event.data["turn"] == 3 && event.data["step"] == 1
        })
        .unwrap();
    let compaction = events
        .iter()
        .filter(|event| event.event_type.starts_with("compaction/"))
        .collect::<Vec<_>>();
    assert_eq!(
        compaction
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        ["compaction/start", "compaction/summary", "compaction/end"]
    );
    assert!(
        compaction
            .iter()
            .all(|event| event.seq > step_start.seq && event.seq < step_end.seq)
    );
    assert_eq!(events.last().unwrap().event_type, "turn/end");
    assert_eq!(events.last().unwrap().data["reason"]["kind"], "completed");
    harness.context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn thrown_and_in_band_overflow_compact_inside_one_retried_step() {
    assert_overflow_recovery(OverflowDelivery::Thrown).await;
    assert_overflow_recovery(OverflowDelivery::InBand).await;
}

#[tokio::test]
async fn overflow_and_transient_retry_budgets_remain_independent() {
    let harness = OverflowHarness::new(OverflowDelivery::Thrown, true).await;
    let (agent, _driver) = harness.agent("alternating-recovery", false);
    run_agent(&agent, "continue from history").await;
    assert_eq!(harness.adapter.conversation.lock().len(), 3);
    assert_eq!(harness.adapter.summary.lock().len(), 1);
    let retry = agent
        .agent
        .session()
        .events()
        .into_iter()
        .filter(|event| event.event_type == "llm/retry")
        .collect::<Vec<_>>();
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].data["turn"], 3);
    assert_eq!(retry[0].data["step"], 1);
    assert_eq!(retry[0].data["retry"], 1);
    assert_eq!(retry[0].data["failure"]["code"], "SERVER");
    let events = agent.agent.session().events();
    assert_eq!(events.last().unwrap().event_type, "turn/end");
    assert_eq!(events.last().unwrap().data["reason"]["kind"], "completed");
    harness.context.fiber().dispose().await.unwrap();
}
