//! Default one-shot summarizer request, routing, and output-safety parity.

use std::sync::Arc;

use futures::stream;
use parking_lot::Mutex;
use seekdeep_attachment::{AttachmentId, ImageAttachmentRef, ImageMediaType};
use seekdeep_compaction::service::{
    CompactionAgentContext, CompactionEngine as _, CompactionRoutingOptions,
};
use seekdeep_compaction_basic::summarizer::{
    SummarizationInput, SummaryConfig, Target, summarize_with_llm,
};
use seekdeep_compaction_basic::{
    BasicCompactionConfig, BasicCompactionEngine, ModelCompactPolicyConfig,
};
use seekdeep_cordis::Context;
use seekdeep_core::session::{AppendOptions, Session, SessionId, SurfaceOp};
use seekdeep_llm::{
    AbortSignal, AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LLM,
    LlmAdapter, LlmError, LlmFailure, LlmRequestPurpose, LlmRuntime, Message, MessageSource,
    StreamChunk, TokenUsage, ToolSchema,
};
use serde_json::{Map, json};

const MODEL: &str = "test-model";

struct ScriptedAdapter {
    blocks: Vec<ContentBlock>,
    finish: FinishReason,
    usage: Mutex<Option<TokenUsage>>,
    last_options: Mutex<Option<GenerateOptions>>,
}

impl ScriptedAdapter {
    fn new(blocks: Vec<ContentBlock>) -> Arc<Self> {
        Arc::new(Self {
            blocks,
            finish: FinishReason::Stop,
            usage: Mutex::new(None),
            last_options: Mutex::new(None),
        })
    }

    fn with_finish(finish: FinishReason) -> Arc<Self> {
        Arc::new(Self {
            blocks: Vec::new(),
            finish,
            usage: Mutex::new(None),
            last_options: Mutex::new(None),
        })
    }
}

#[async_trait::async_trait]
impl LlmAdapter for ScriptedAdapter {
    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        *self.last_options.lock() = Some(options);
        let mut chunks = Vec::new();
        for (index, block) in self.blocks.iter().enumerate() {
            let index = u64::try_from(index).unwrap();
            chunks.push(Ok(StreamChunk::BlockStart {
                index,
                block_type: block.block_type().to_owned(),
            }));
            match block {
                ContentBlock::Text { text } => {
                    chunks.push(Ok(StreamChunk::TextDelta {
                        index,
                        text: text.clone(),
                    }));
                }
                ContentBlock::Reasoning { text } => {
                    chunks.push(Ok(StreamChunk::ReasoningDelta {
                        index,
                        text: text.clone(),
                    }));
                }
                _ => {}
            }
            chunks.push(Ok(StreamChunk::BlockEnd {
                index,
                block: block.clone(),
            }));
        }
        if let Some(usage) = self.usage.lock().clone() {
            chunks.push(Ok(StreamChunk::Usage { usage }));
        }
        chunks.push(Ok(StreamChunk::Finish {
            reason: self.finish.clone(),
            replay_state: None,
        }));
        AdapterStream::new(stream::iter(chunks))
    }
}

struct Harness {
    context: Context,
}

impl Harness {
    fn new(provider: &str, adapter: &Arc<ScriptedAdapter>) -> Self {
        let context = Context::new();
        let llm = LlmRuntime::install(&context).unwrap();
        llm.register_adapter(&[provider.to_owned()], adapter.clone())
            .unwrap();
        Self { context }
    }

    async fn summarize(
        &self,
        input: &SummarizationInput,
        session: &Arc<Session>,
        config: SummaryConfig,
        fallback: Option<Target>,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<seekdeep_compaction_basic::summarizer::SummaryResult> {
        summarize_with_llm(&self.context, &config, input, session, fallback, signal).await
    }
}

fn session(id: &str) -> Arc<Session> {
    Session::create(&SessionId::new(id), None, None).unwrap()
}

fn prompt_input(text: &str) -> SummarizationInput {
    SummarizationInput {
        system: None,
        tools: None,
        messages: vec![Message::user(
            vec![ContentBlock::Text {
                text: text.to_owned(),
            }],
            MessageSource::plugin("test"),
        )],
    }
}

fn default_summary_config() -> SummaryConfig {
    SummaryConfig {
        summarization_provider: String::new(),
        summarization_model: String::new(),
        max_tokens: 8192,
    }
}

fn target(provider: &str, model: &str) -> Target {
    Target {
        provider: provider.to_owned(),
        model: model.to_owned(),
    }
}

fn failure(message: &str, code: &str) -> LlmFailure {
    LlmFailure {
        message: message.to_owned(),
        code: code.to_owned(),
        status: None,
        provider_retry_after_ms: None,
        request_id: None,
    }
}

fn image(id: char) -> ContentBlock {
    ContentBlock::Image {
        attachment: ImageAttachmentRef {
            attachment_id: AttachmentId::new(format!("sha256:{}", id.to_string().repeat(64))),
            media_type: ImageMediaType::Png,
            bytes: 1,
            width: 1,
            height: 1,
            name: None,
        },
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

fn region_session(id: &str, with_header: bool) -> Arc<Session> {
    let session = session(id);
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .unwrap();
    append_surface(
        &session,
        "user/message",
        serde_json::to_value(Message::user(
            vec![ContentBlock::Text {
                text: "warm prefix ".repeat(100),
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
    if with_header {
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
        &session,
        "assistant/message",
        json!({
            "turn": 1,
            "step": 1,
            "message": Message::assistant(
                vec![ContentBlock::Text { text: "answer ".repeat(100) }],
                MODEL,
                MODEL,
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
    session
}

#[tokio::test]
async fn configured_route_cap_signal_and_safe_text_projection_are_exact() {
    let raw = vec![
        ContentBlock::Reasoning {
            text: "private".to_owned(),
        },
        ContentBlock::Text {
            text: "public summary".to_owned(),
        },
        ContentBlock::ToolCall {
            id: CallId::new("unexpected"),
            name: "x".to_owned(),
            arguments: "{}".to_owned(),
        },
    ];
    let adapter = ScriptedAdapter::new(raw.clone());
    *adapter.usage.lock() = Some(TokenUsage {
        input_tokens: 12,
        output_tokens: 3,
        cache_read_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
    });
    let harness = Harness::new(MODEL, &adapter);
    let owner = session("configured-summary");
    let signal = AbortSignal::default();
    let output = harness
        .summarize(
            &prompt_input("transcript"),
            &owner,
            SummaryConfig {
                summarization_provider: MODEL.to_owned(),
                summarization_model: MODEL.to_owned(),
                max_tokens: 321,
            },
            Some(target("fallback", "fallback")),
            Some(signal.clone()),
        )
        .await
        .unwrap();
    assert_eq!(
        output.summary,
        [ContentBlock::Text {
            text: "public summary".to_owned()
        }]
    );
    assert_eq!(output.raw_output, raw);
    assert!(output.llm_stream_call);
    assert_eq!(output.provider, MODEL);
    assert_eq!(output.model, MODEL);
    assert_eq!(output.max_tokens, Some(321));
    assert_eq!(output.usage, *adapter.usage.lock());
    let options = adapter.last_options.lock();
    let options = options.as_ref().unwrap();
    assert_eq!(options.provider.as_str(), MODEL);
    assert_eq!(options.model.as_str(), MODEL);
    assert_eq!(options.max_tokens, Some(321));
    assert_eq!(options.session_id.as_ref(), Some(owner.id()));
    assert_eq!(options.purpose, Some(LlmRequestPurpose::Compaction));
    signal.abort();
    assert!(options.signal.as_ref().is_some_and(AbortSignal::is_aborted));
    let ContentBlock::Text { text } = &options.messages.last().unwrap().content()[0] else {
        panic!("instruction must be text");
    };
    assert!(text.contains("## Primary Request and Intent"));
}

#[tokio::test]
async fn replays_prefix_system_tools_and_appends_instruction_last() {
    let adapter = ScriptedAdapter::new(vec![ContentBlock::Text {
        text: "summary".to_owned(),
    }]);
    let harness = Harness::new(MODEL, &adapter);
    let prefix = Message::user(
        vec![
            ContentBlock::Text {
                text: "earlier turn".to_owned(),
            },
            image('a'),
        ],
        MessageSource::plugin("test"),
    );
    let mut parameters = Map::new();
    parameters.insert("type".to_owned(), json!("object"));
    let tools = vec![ToolSchema {
        name: "do_thing".to_owned(),
        description: "d".to_owned(),
        parameters,
    }];
    harness
        .summarize(
            &SummarizationInput {
                system: Some("REPLAYED SYSTEM".to_owned()),
                tools: Some(tools.clone()),
                messages: vec![prefix.clone()],
            },
            &session("prefix"),
            default_summary_config(),
            Some(target(MODEL, MODEL)),
            None,
        )
        .await
        .unwrap();
    let options = adapter.last_options.lock();
    let options = options.as_ref().unwrap();
    assert_eq!(options.system.as_deref(), Some("REPLAYED SYSTEM"));
    assert_eq!(options.tools.as_ref(), Some(&tools));
    assert_eq!(options.messages[0], prefix);
    let ContentBlock::Text { text } = &options.messages.last().unwrap().content()[0] else {
        panic!("instruction must be text");
    };
    assert!(text.contains("Write concise English engineering prose."));
    assert!(text.contains("numeric values, function signatures, and syntax fragments."));
    assert!(text.contains("## Primary Request and Intent"));
}

#[tokio::test]
async fn latest_durable_route_precedes_fallback_and_complete_fallback_is_used_when_headerless() {
    let routed = ScriptedAdapter::new(vec![ContentBlock::Text {
        text: "summary".to_owned(),
    }]);
    let harness = Harness::new("routed", &routed);
    let owner = session("routed-summary");
    owner
        .append(
            "request/header",
            json!({
                "header": {"config": {"provider": "routed", "model": "routed"}},
                "reason": "initial"
            }),
            AppendOptions::default(),
        )
        .unwrap();
    let output = harness
        .summarize(
            &prompt_input("history"),
            &owner,
            default_summary_config(),
            Some(target("fallback", "fallback")),
            None,
        )
        .await
        .unwrap();
    assert_eq!(output.provider, "routed");
    assert_eq!(output.model, "routed");

    let fallback = ScriptedAdapter::new(vec![ContentBlock::Text {
        text: "summary".to_owned(),
    }]);
    let fallback_harness = Harness::new(MODEL, &fallback);
    let output = fallback_harness
        .summarize(
            &prompt_input("history"),
            &session("headerless-summary"),
            default_summary_config(),
            Some(target(MODEL, MODEL)),
            None,
        )
        .await
        .unwrap();
    assert_eq!(output.provider, MODEL);
    assert_eq!(output.model, MODEL);
}

#[tokio::test]
async fn records_route_actually_dispatched_after_one_shot_middleware() {
    let initial = ScriptedAdapter::new(vec![ContentBlock::Text {
        text: "unused".to_owned(),
    }]);
    let harness = Harness::new(MODEL, &initial);
    let routed = ScriptedAdapter::new(vec![ContentBlock::Text {
        text: "routed summary".to_owned(),
    }]);
    let llm = harness.context.get(LLM).unwrap();
    llm.register_adapter(&["routed-summary-provider".to_owned()], routed.clone())
        .unwrap();
    llm.register_stream_middleware(
        &harness.context,
        Arc::new(|mut options, next| {
            options.provider = "routed-summary-provider".into();
            options.model = "routed-summary-model".into();
            next(options)
        }),
        false,
    )
    .unwrap();

    let output = harness
        .summarize(
            &prompt_input("history"),
            &session("middleware-route"),
            default_summary_config(),
            Some(target(MODEL, MODEL)),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        output.summary[0],
        ContentBlock::Text {
            text: "routed summary".to_owned()
        }
    );
    assert_eq!(output.provider, "routed-summary-provider");
    assert_eq!(output.model, "routed-summary-model");
    let options = routed.last_options.lock();
    assert_eq!(
        options.as_ref().unwrap().provider.as_str(),
        "routed-summary-provider"
    );
    assert_eq!(
        options.as_ref().unwrap().model.as_str(),
        "routed-summary-model"
    );
}

#[tokio::test]
async fn production_engine_applies_exact_routed_summary_policy() {
    let default_adapter = ScriptedAdapter::new(vec![ContentBlock::Text {
        text: "unused default summary".to_owned(),
    }]);
    let harness = Harness::new(MODEL, &default_adapter);
    let policy_adapter = ScriptedAdapter::new(vec![ContentBlock::Text {
        text: "policy summary".to_owned(),
    }]);
    let llm = harness.context.get(LLM).unwrap();
    llm.register_adapter(&["policy-summary".to_owned()], policy_adapter.clone())
        .unwrap();
    let _meter = seekdeep_token_meter::install(
        &harness.context,
        seekdeep_token_meter::TokenMeterConfig::default(),
    )
    .unwrap();
    let engine = BasicCompactionEngine::new(
        &harness.context,
        &BasicCompactionConfig {
            auto: Some(false),
            max_tokens: Some(111),
            model_policies: vec![ModelCompactPolicyConfig {
                provider: MODEL.to_owned(),
                model: MODEL.to_owned(),
                summarization_provider: Some("policy-summary".to_owned()),
                summarization_model: Some("policy-summary".to_owned()),
                max_tokens: Some(222),
                ..ModelCompactPolicyConfig::default()
            }],
            ..BasicCompactionConfig::default()
        },
    )
    .unwrap();
    let owner = region_session("policy-region", true);
    let nodes = owner.surface_nodes();
    engine
        .compact_region(
            nodes[0],
            nodes[1],
            &CompactionAgentContext {
                session: owner,
                options: CompactionRoutingOptions {
                    provider: Some("fallback".to_owned()),
                    model: Some("fallback".to_owned()),
                },
            },
            None,
        )
        .await
        .unwrap();
    let options = policy_adapter.last_options.lock();
    let options = options.as_ref().unwrap();
    assert_eq!(options.provider.as_str(), "policy-summary");
    assert_eq!(options.model.as_str(), "policy-summary");
    assert_eq!(options.max_tokens, Some(222));
    assert_eq!(
        options.messages[0].content()[0],
        ContentBlock::Text {
            text: "warm prefix ".repeat(100)
        }
    );
}

#[tokio::test]
async fn production_engine_rejects_every_incomplete_agent_fallback() {
    let adapter = ScriptedAdapter::new(vec![ContentBlock::Text {
        text: "unused".to_owned(),
    }]);
    let harness = Harness::new(MODEL, &adapter);
    let _meter = seekdeep_token_meter::install(
        &harness.context,
        seekdeep_token_meter::TokenMeterConfig::default(),
    )
    .unwrap();
    let engine = BasicCompactionEngine::new(
        &harness.context,
        &BasicCompactionConfig {
            auto: Some(false),
            ..BasicCompactionConfig::default()
        },
    )
    .unwrap();
    let owner = region_session("incomplete-fallback", false);
    let nodes = owner.surface_nodes();
    for options in [
        CompactionRoutingOptions {
            provider: Some(String::new()),
            model: Some(MODEL.to_owned()),
        },
        CompactionRoutingOptions {
            provider: Some(MODEL.to_owned()),
            model: None,
        },
        CompactionRoutingOptions {
            provider: Some(MODEL.to_owned()),
            model: Some(String::new()),
        },
    ] {
        let error = engine
            .compact_region(
                nodes[0],
                nodes[1],
                &CompactionAgentContext {
                    session: owner.clone(),
                    options,
                },
                None,
            )
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("no provider/model available for summarization")
        );
    }
}

#[tokio::test]
async fn missing_complete_target_fails_clearly() {
    let context = Context::new();
    LlmRuntime::install(&context).unwrap();
    let error = summarize_with_llm(
        &context,
        &default_summary_config(),
        &prompt_input("history"),
        &session("model-less"),
        None,
        None,
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("no provider/model available for summarization")
    );
}

#[tokio::test]
async fn terminal_finishes_preserve_message_and_stable_code() {
    for (finish, expected_message, expected_code) in [
        (
            FinishReason::Error {
                failure: failure("provider failed", "PROVIDER"),
            },
            "provider failed",
            "PROVIDER",
        ),
        (
            FinishReason::Error {
                failure: failure("opaque", "UNKNOWN"),
            },
            "opaque",
            "UNKNOWN",
        ),
        (
            FinishReason::Aborted {
                failure: failure("summarization aborted", "ABORTED"),
            },
            "aborted",
            "ABORTED",
        ),
        (FinishReason::MaxTokens, "token cap", "MAX_TOKENS"),
    ] {
        let adapter = ScriptedAdapter::with_finish(finish);
        let harness = Harness::new(MODEL, &adapter);
        let error = harness
            .summarize(
                &prompt_input("history"),
                &session("finish"),
                default_summary_config(),
                Some(target(MODEL, MODEL)),
                None,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains(expected_message), "{error}");
        let error = error.downcast_ref::<LlmError>().expect("LLM error");
        assert_eq!(error.code(), expected_code);
    }
}

#[tokio::test]
async fn rejects_empty_reasoning_only_and_image_output() {
    for (blocks, expected_code) in [
        (
            vec![ContentBlock::Reasoning {
                text: "private".to_owned(),
            }],
            None,
        ),
        (
            vec![
                image('b'),
                ContentBlock::Text {
                    text: "partial".to_owned(),
                },
            ],
            Some("UNSUPPORTED_CONTENT"),
        ),
        (
            vec![ContentBlock::ToolResult {
                tool_call_id: CallId::new("summary-tool"),
                content: vec![image('c')],
                is_error: None,
            }],
            Some("UNSUPPORTED_CONTENT"),
        ),
    ] {
        let adapter = ScriptedAdapter::new(blocks);
        let harness = Harness::new(MODEL, &adapter);
        let error = harness
            .summarize(
                &prompt_input("history"),
                &session("unsafe"),
                default_summary_config(),
                Some(target(MODEL, MODEL)),
                None,
            )
            .await
            .unwrap_err();
        if let Some(code) = expected_code {
            assert_eq!(error.downcast_ref::<LlmError>().unwrap().code(), code);
        } else {
            assert!(error.to_string().contains("no text summary content"));
        }
    }
}
