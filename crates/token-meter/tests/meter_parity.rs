//! Replay measurement, anchoring, surface, configuration, and lifecycle parity.

use std::sync::Arc;

use seekdeep_cordis::Context;
use seekdeep_core::{
    request_header::{EpochHeader, canonical_header},
    session::{AppendOptions, Session, SessionId, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_llm::{
    ContentBlock, FinishReason, LlmCallConfig, Message, MessageRole, MessageSource, ModelId,
    ProviderId, StreamChunk, TokenUsage, ToolSchema, UserMessage,
};
use seekdeep_token_meter::{
    TOKEN_METER, TokenMeasurementBaseline, TokenMeterConfig, TokenMeterInstallation, install,
};
use serde_json::{Map, Value, json};

struct Meter {
    context: Context,
    installation: TokenMeterInstallation,
}

impl Meter {
    fn new() -> Self {
        let context = Context::new();
        let installation = install(&context, TokenMeterConfig::default()).unwrap();
        Self {
            context,
            installation,
        }
    }

    async fn close(self) {
        self.installation.dispose().await.unwrap();
        self.context.fiber().dispose().await.unwrap();
    }
}

fn header(model: &str, system: Option<&str>, tools: Option<Vec<ToolSchema>>) -> EpochHeader {
    canonical_header(EpochHeader {
        config: LlmCallConfig {
            provider: ProviderId::new("mock"),
            model: ModelId::new(model),
            reasoning_effort: None,
            temperature: None,
            max_tokens: None,
            stop: None,
        },
        adapter_defaults: None,
        system: system.map(str::to_owned),
        tools,
    })
}

fn user(text: &str) -> UserMessage {
    UserMessage::new(
        vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        MessageSource::user(),
    )
}

fn append_user(session: &Session, text: &str) -> u64 {
    session
        .append(
            "user/message",
            serde_json::to_value(user(text)).unwrap(),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .unwrap()
        .seq
}

fn append_header(session: &Session, header: &EpochHeader) {
    session
        .append(
            "request/header",
            json!({"header":header,"reason":"initial"}),
            AppendOptions::default(),
        )
        .unwrap();
}

#[derive(Clone, Copy)]
enum Provenance {
    Exact,
    Empty,
    Absent,
}

struct SuccessfulCall<'a> {
    turn: u64,
    step: u64,
    provider_text: &'a str,
    durable_text: &'a str,
    usage: Option<TokenUsage>,
    provenance: Provenance,
}

fn append_successful_call(
    session: &Session,
    header: &EpochHeader,
    options: SuccessfulCall<'_>,
) -> u64 {
    session
        .append(
            "step/start",
            json!({"turn":options.turn,"step":options.step}),
            AppendOptions::default(),
        )
        .unwrap();
    append_header(session, header);
    let mut sources = Vec::new();
    if matches!(options.provenance, Provenance::Exact) {
        let mut chunks = vec![
            StreamChunk::TextDelta {
                index: 0,
                text: options.provider_text.to_owned(),
            },
            StreamChunk::BlockEnd {
                index: 0,
                block: ContentBlock::Text {
                    text: options.provider_text.to_owned(),
                },
            },
        ];
        if let Some(usage) = options.usage.clone() {
            chunks.push(StreamChunk::Usage { usage });
        }
        chunks.push(StreamChunk::Finish {
            reason: FinishReason::Stop,
            replay_state: None,
        });
        for chunk in chunks {
            sources.push(
                session
                    .append(
                        "assistant/chunk",
                        json!({"turn":options.turn,"step":options.step,"chunk":chunk}),
                        AppendOptions::default(),
                    )
                    .unwrap()
                    .seq,
            );
        }
    }
    let content = if options.durable_text.is_empty() {
        Vec::new()
    } else {
        vec![ContentBlock::Text {
            text: options.durable_text.to_owned(),
        }]
    };
    let message = Message::new(
        MessageRole::Assistant,
        content,
        MessageSource::model(
            header.config.provider.as_str(),
            header.config.model.as_str(),
        ),
    );
    let mut data = json!({
        "turn":options.turn,
        "step":options.step,
        "message":message
    });
    if let Some(usage) = options.usage {
        data["usage"] = serde_json::to_value(usage).unwrap();
    }
    let source_event_seqs = match options.provenance {
        Provenance::Exact => Some(sources),
        Provenance::Empty => Some(Vec::new()),
        Provenance::Absent => None,
    };
    let assistant = session
        .append(
            "assistant/message",
            data,
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                source_event_seqs,
                ignorable: false,
            },
        )
        .unwrap();
    session
        .append(
            "step/end",
            json!({"turn":options.turn,"step":options.step}),
            AppendOptions::default(),
        )
        .unwrap();
    assistant.seq
}

fn usage() -> TokenUsage {
    TokenUsage {
        input_tokens: 20,
        output_tokens: 7,
        cache_read_tokens: Some(3),
        cache_write_tokens: Some(4),
        reasoning_tokens: Some(6),
    }
}

#[tokio::test]
async fn configuration_service_registration_and_fixed_pricing_match_the_source() {
    let context = Context::new();
    let definition = seekdeep_token_meter::plugin();
    assert_eq!(definition.name(), "token-meter");
    let plugin = context.plugin(definition, json!({})).unwrap();
    plugin.await_settled().await.unwrap();
    let service = context.get(TOKEN_METER).unwrap();
    assert_eq!(service.estimate_message(&user("abcd")), 9);
    let blocks = vec![
        ContentBlock::Text {
            text: "abcd".to_owned(),
        },
        ContentBlock::Reasoning {
            text: "ab".to_owned(),
        },
        ContentBlock::ToolCall {
            id: "c".into(),
            name: "read".to_owned(),
            arguments: "{\"x\":1}".to_owned(),
        },
        ContentBlock::ToolResult {
            tool_call_id: "c".into(),
            content: vec![ContentBlock::Text {
                text: "xy".to_owned(),
            }],
            is_error: Some(false),
        },
        ContentBlock::Unknown {
            block_type: "future-block".to_owned(),
            fields: Map::from_iter([("payload".to_owned(), Value::String("abcd".to_owned()))]),
        },
    ];
    let rich = Message::new(
        MessageRole::Assistant,
        blocks,
        MessageSource::plugin("test"),
    );
    assert!(service.estimate_message(&rich) > 30);

    plugin.dispose().await.unwrap();
    assert!(context.get(TOKEN_METER).is_none());
    context.fiber().dispose().await.unwrap();

    for key in ["models", "contextWindow", "contextWidow"] {
        let context = Context::new();
        let mounted = context
            .plugin(seekdeep_token_meter::plugin(), json!({(key):{}}))
            .unwrap();
        let error = mounted.await_settled().await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&format!("unknown key \"{key}\""))
        );
        context.fiber().dispose().await.unwrap();
    }
}

#[tokio::test]
async fn empty_and_later_measurements_are_owned_detached_snapshots() {
    let meter = Meter::new();
    let session = Session::create(&SessionId::new("detached"), None, None).unwrap();
    assert_eq!(
        meter.installation.measure(&session, None).unwrap(),
        seekdeep_token_meter::TokenMeasurement {
            log_revision: 0,
            baseline: TokenMeasurementBaseline::None { tokens: 0 },
            surface_delta_tokens: 0,
            total_tokens: 0,
            surface_tokens: 0,
            nodes: Vec::new(),
        }
    );
    append_user(&session, "first");
    let snapshot = meter.installation.measure(&session, None).unwrap();
    append_user(&session, "second");
    let advanced = meter.installation.measure(&session, None).unwrap();
    assert_eq!(snapshot.log_revision, 1);
    assert_eq!(snapshot.nodes.len(), 1);
    assert_eq!(advanced.log_revision, 2);
    assert_eq!(advanced.nodes.len(), 2);
    assert_eq!(
        snapshot.nodes.iter().map(|node| node.tokens).sum::<u64>(),
        snapshot.surface_tokens
    );
    meter.close().await;
}

#[tokio::test]
async fn heuristic_envelope_override_changes_pressure_but_never_the_surface_snapshot() {
    let meter = Meter::new();
    let session = Session::create(&SessionId::new("heuristic"), None, None).unwrap();
    append_user(&session, "question");
    let logged_header = header(
        "model",
        Some("system"),
        Some(vec![ToolSchema {
            name: "read".to_owned(),
            description: "read".to_owned(),
            parameters: Map::from_iter([("type".to_owned(), json!("object"))]),
        }]),
    );
    append_header(&session, &logged_header);
    let logged = meter.installation.measure(&session, None).unwrap();
    assert!(matches!(
        logged.baseline,
        TokenMeasurementBaseline::Estimated { .. }
    ));
    assert!(logged.total_tokens > logged.surface_tokens);

    let override_header = header("other", Some(&"large override ".repeat(100)), None);
    let overridden = meter
        .installation
        .measure(&session, Some(override_header))
        .unwrap();
    assert!(overridden.total_tokens > logged.total_tokens);
    assert_eq!(overridden.surface_tokens, logged.surface_tokens);
    assert_eq!(overridden.nodes, logged.nodes);
    meter.close().await;
}

#[tokio::test]
async fn provider_anchor_uses_disjoint_usage_and_signed_durable_rewrites() {
    let meter = Meter::new();
    let session = Session::create(&SessionId::new("usage"), None, None).unwrap();
    append_user(&session, "before");
    append_successful_call(
        &session,
        &header("model", None, None),
        SuccessfulCall {
            turn: 1,
            step: 1,
            provider_text: "short",
            durable_text: "a much longer rewritten durable assistant answer",
            usage: Some(usage()),
            provenance: Provenance::Exact,
        },
    );
    let result = meter.installation.measure(&session, None).unwrap();
    assert_eq!(
        result.baseline,
        TokenMeasurementBaseline::Usage {
            tokens: 34,
            usage: usage(),
        }
    );
    assert!(result.surface_delta_tokens > 0);
    assert_eq!(
        result.total_tokens,
        u64::try_from(34_i64 + result.surface_delta_tokens).unwrap()
    );
    meter.close().await;
}

#[tokio::test]
async fn low_or_absent_usage_anchors_estimate_and_carry_signed_surface_movement() {
    let meter = Meter::new();
    let low = Session::create(&SessionId::new("low"), None, None).unwrap();
    let low_header = header("model", Some("system context"), None);
    let assistant = append_successful_call(
        &low,
        &low_header,
        SuccessfulCall {
            turn: 1,
            step: 1,
            provider_text: &"abcd".repeat(512),
            durable_text: &"abcd".repeat(512),
            usage: Some(TokenUsage {
                input_tokens: 20,
                output_tokens: 7,
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
            }),
            provenance: Provenance::Exact,
        },
    );
    let anchored = meter.installation.measure(&low, None).unwrap();
    assert!(matches!(
        anchored.baseline,
        TokenMeasurementBaseline::Estimated { .. }
    ));
    low.append(
        "user/message",
        serde_json::to_value(user("short")).unwrap(),
        AppendOptions {
            surface_op: Some(SurfaceOp::replace(assistant, assistant)),
            source_event_seqs: Some(vec![assistant]),
            ignorable: false,
        },
    )
    .unwrap();
    let shrunken = meter.installation.measure(&low, None).unwrap();
    assert!(shrunken.surface_delta_tokens < 0);
    assert!(shrunken.total_tokens > 0);

    let absent = Session::create(&SessionId::new("absent"), None, None).unwrap();
    append_successful_call(
        &absent,
        &header("model", Some("s"), None),
        SuccessfulCall {
            turn: 1,
            step: 1,
            provider_text: "provider",
            durable_text: "rewritten",
            usage: None,
            provenance: Provenance::Exact,
        },
    );
    let before = meter.installation.measure(&absent, None).unwrap();
    assert_eq!(before.surface_delta_tokens, 0);
    append_user(&absent, "later");
    assert!(
        meter
            .installation
            .measure(&absent, None)
            .unwrap()
            .surface_delta_tokens
            > 0
    );
    meter.close().await;
}

#[tokio::test]
async fn explicit_empty_and_absent_legacy_provenance_choose_different_anchors() {
    let meter = Meter::new();
    let explicit = Session::create(&SessionId::new("explicit"), None, None).unwrap();
    let legacy = Session::create(&SessionId::new("legacy"), None, None).unwrap();
    for (session, provenance) in [
        (&explicit, Provenance::Empty),
        (&legacy, Provenance::Absent),
    ] {
        append_successful_call(
            session,
            &header("model", None, None),
            SuccessfulCall {
                turn: 1,
                step: 1,
                provider_text: "",
                durable_text: "listener injected text",
                usage: Some(usage()),
                provenance,
            },
        );
    }
    assert!(
        meter
            .installation
            .measure(&explicit, None)
            .unwrap()
            .surface_delta_tokens
            > 0
    );
    assert_eq!(
        meter
            .installation
            .measure(&legacy, None)
            .unwrap()
            .surface_delta_tokens,
        0
    );
    meter.close().await;
}

#[tokio::test]
async fn latest_success_and_full_canonical_envelope_control_anchor_reuse() {
    let meter = Meter::new();
    let session = Session::create(&SessionId::new("switch"), None, None).unwrap();
    let alpha = header("alpha", Some("same envelope"), None);
    append_successful_call(
        &session,
        &alpha,
        SuccessfulCall {
            turn: 1,
            step: 1,
            provider_text: "alpha",
            durable_text: "alpha",
            usage: Some(usage()),
            provenance: Provenance::Exact,
        },
    );
    assert!(matches!(
        meter.installation.measure(&session, None).unwrap().baseline,
        TokenMeasurementBaseline::Usage { tokens: 34, .. }
    ));
    append_successful_call(
        &session,
        &header("beta", None, None),
        SuccessfulCall {
            turn: 1,
            step: 2,
            provider_text: "beta response",
            durable_text: "beta response",
            usage: Some(TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
            }),
            provenance: Provenance::Exact,
        },
    );
    assert!(matches!(
        meter.installation.measure(&session, None).unwrap().baseline,
        TokenMeasurementBaseline::Usage { tokens: 150, .. }
    ));
    append_header(&session, &alpha);
    assert!(matches!(
        meter.installation.measure(&session, None).unwrap().baseline,
        TokenMeasurementBaseline::Estimated { .. }
    ));

    let canonical_empty_tools = EpochHeader {
        tools: Some(Vec::new()),
        ..alpha.clone()
    };
    let independent = Session::create(&SessionId::new("canonical"), None, None).unwrap();
    append_successful_call(
        &independent,
        &alpha,
        SuccessfulCall {
            turn: 1,
            step: 1,
            provider_text: "answer",
            durable_text: "answer",
            usage: Some(usage()),
            provenance: Provenance::Exact,
        },
    );
    assert!(matches!(
        meter
            .installation
            .measure(&independent, Some(canonical_empty_tools))
            .unwrap()
            .baseline,
        TokenMeasurementBaseline::Usage { .. }
    ));
    let changed = header("alpha", Some("changed"), None);
    assert!(matches!(
        meter
            .installation
            .measure(&independent, Some(changed))
            .unwrap()
            .baseline,
        TokenMeasurementBaseline::Estimated { .. }
    ));
    meter.close().await;
}

#[tokio::test]
async fn surface_replace_and_empty_assistant_nodes_replay_positionally() {
    let meter = Meter::new();
    let session = Session::create(&SessionId::new("surface"), None, None).unwrap();
    let assistant = append_successful_call(
        &session,
        &header("model", None, None),
        SuccessfulCall {
            turn: 1,
            step: 1,
            provider_text: &"long provider answer ".repeat(100),
            durable_text: &"long provider answer ".repeat(100),
            usage: Some(usage()),
            provenance: Provenance::Exact,
        },
    );
    append_user(&session, "new tail");
    let before = meter.installation.measure(&session, None).unwrap();
    session
        .append(
            "user/message",
            serde_json::to_value(user("replacement")).unwrap(),
            AppendOptions {
                surface_op: Some(SurfaceOp::replace(assistant, assistant)),
                source_event_seqs: Some(vec![assistant]),
                ignorable: false,
            },
        )
        .unwrap();
    let after = meter.installation.measure(&session, None).unwrap();
    assert_eq!(before.nodes.len(), 2);
    assert_eq!(after.nodes.len(), 2);
    assert!(after.surface_delta_tokens < before.surface_delta_tokens);
    assert_eq!(
        after.nodes.iter().map(|node| node.tokens).sum::<u64>(),
        after.surface_tokens
    );

    let empty = Session::create(&SessionId::new("empty-assistant"), None, None).unwrap();
    let empty_seq = append_successful_call(
        &empty,
        &header("model", None, None),
        SuccessfulCall {
            turn: 1,
            step: 1,
            provider_text: "",
            durable_text: "",
            usage: None,
            provenance: Provenance::Empty,
        },
    );
    let empty_measurement = meter.installation.measure(&empty, None).unwrap();
    assert_eq!(
        empty_measurement.nodes,
        vec![seekdeep_token_meter::TokenSurfaceNode {
            seq: empty_seq,
            tokens: 0,
        }]
    );
    meter.close().await;
}

#[tokio::test]
async fn malformed_step_history_fails_transactionally_on_every_measurement() {
    let meter = Meter::new();
    let no_step = Session::create(&SessionId::new("no-step"), None, None).unwrap();
    append_header(&no_step, &header("model", None, None));
    no_step
        .append(
            "assistant/message",
            json!({
                "turn":1,"step":1,
                "message":Message::new(
                    MessageRole::Assistant,
                    vec![ContentBlock::Text { text:"bad".to_owned() }],
                    MessageSource::model("mock","model")
                )
            }),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                source_event_seqs: Some(Vec::new()),
                ignorable: false,
            },
        )
        .unwrap();
    for _ in 0..2 {
        assert!(
            meter
                .installation
                .measure(&no_step, None)
                .unwrap_err()
                .to_string()
                .contains("no matching step/start")
        );
    }

    let overlap = Session::create(&SessionId::new("overlap"), None, None).unwrap();
    overlap
        .append(
            "step/start",
            json!({"turn":1,"step":1}),
            AppendOptions::default(),
        )
        .unwrap();
    overlap
        .append(
            "step/start",
            json!({"turn":1,"step":2}),
            AppendOptions::default(),
        )
        .unwrap();
    for _ in 0..2 {
        assert!(
            meter
                .installation
                .measure(&overlap, None)
                .unwrap_err()
                .to_string()
                .contains("arrived before turn 1/step 1 ended")
        );
    }
    meter.close().await;
}

#[tokio::test]
async fn direct_installation_is_visible_and_reversible() {
    let context = Context::new();
    let installation = install(&context, TokenMeterConfig::default()).unwrap();
    assert!(Arc::ptr_eq(
        &context.get(TOKEN_METER).unwrap(),
        &installation
    ));
    installation.dispose().await.unwrap();
    assert!(context.get(TOKEN_METER).is_none());
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn reader_catch_up_eager_observation_service_reload_and_invariant_reservation_are_reversible()
{
    let context = Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    let first = context
        .plugin(seekdeep_token_meter::plugin(), json!({}))
        .unwrap();
    first.await_settled().await.unwrap();
    let service = context.get(TOKEN_METER).unwrap();
    let session = sessions
        .create(
            &context,
            Some(SessionId::new("listener-order")),
            CreateSessionOptions::default(),
        )
        .unwrap();
    assert_eq!(service.measure(&session, None).unwrap().log_revision, 0);
    append_user(&session, "one");
    assert_eq!(service.measure(&session, None).unwrap().log_revision, 1);
    first.dispose().await.unwrap();

    let second = context
        .plugin(seekdeep_token_meter::plugin(), json!({}))
        .unwrap();
    second.await_settled().await.unwrap();
    let reloaded = context.get(TOKEN_METER).unwrap();
    assert!(!Arc::ptr_eq(&service, &reloaded));
    assert_eq!(reloaded.measure(&session, None).unwrap().log_revision, 1);

    let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
    let reservation = seekdeep_token_meter::invariant::register_invariant(&registry).unwrap();
    reservation.await_ready().await.unwrap();
    assert!(registry.is_registered("@deepseek-ai/seekdeep-token-meter"));
    reservation.dispose().await.unwrap();
    assert!(!registry.is_registered("@deepseek-ai/seekdeep-token-meter"));

    second.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}
