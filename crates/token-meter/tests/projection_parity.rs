//! Pure folds, bounded checkpoints, optional registration, and restore parity.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use seekdeep_cordis::Context;
use seekdeep_core::{
    request_header::{EpochHeader, canonical_header},
    session::{AppendOptions, SessionEvent, SessionId, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::{
    ContentBlock, LlmCallConfig, MessageSource, ModelId, ProviderId, TokenUsage, ToolSchema,
    UserMessage,
};
use seekdeep_session_projection::{
    ProjectionDefinition, ProjectionTransition, SESSION_PROJECTIONS, SessionProjectionRegistry,
};
use seekdeep_token_meter::{
    breakdown_projection::{CONTEXT_BREAKDOWN_KEY, context_breakdown_definition},
    estimate::{estimate_content, estimate_system_tokens, estimate_tools_tokens},
    surface_projection::{ShadowPriceClaim, fold_surface_projection},
    usage_projection::{
        CONTEXT_PRESSURE_KEY, TOKEN_USAGE_KEY, context_pressure_definition, token_usage_definition,
    },
};
use serde_json::{Map, Value, json};

fn event(seq: u64, event_type: &str, data: Value, surface_op: Option<SurfaceOp>) -> SessionEvent {
    SessionEvent {
        event_type: event_type.to_owned(),
        seq,
        time: i64::try_from(seq).unwrap(),
        data,
        source_event_seqs: None,
        surface_op,
        ignorable: None,
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

fn header(system: Option<&str>, tools: Option<Vec<ToolSchema>>) -> EpochHeader {
    canonical_header(EpochHeader {
        config: LlmCallConfig {
            provider: ProviderId::new("mock"),
            model: ModelId::new("model"),
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

fn usage(
    input: u64,
    output: u64,
    read: Option<u64>,
    write: Option<u64>,
    reasoning: Option<u64>,
) -> TokenUsage {
    TokenUsage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: read,
        cache_write_tokens: write,
        reasoning_tokens: reasoning,
    }
}

fn usage_chunk(seq: u64, turn: u64, step: u64, usage: &TokenUsage) -> SessionEvent {
    event(
        seq,
        "assistant/chunk",
        json!({"turn":turn,"step":step,"chunk":{"type":"usage","usage":usage}}),
        None,
    )
}

fn assistant_usage(seq: u64, turn: u64, step: u64, usage: &TokenUsage) -> SessionEvent {
    event(
        seq,
        "assistant/message",
        json!({"turn":turn,"step":step,"usage":usage}),
        Some(SurfaceOp::append()),
    )
}

fn fold(
    definition: &ProjectionDefinition,
    events: &[SessionEvent],
) -> anyhow::Result<(Value, Value, usize)> {
    let mut state = definition.initial_state()?;
    let mut changes = 0;
    for event in events {
        if let ProjectionTransition::Changed(next) = definition.apply_event(&state, event)? {
            state = next;
            changes += 1;
        }
    }
    let view = definition.project(&state)?;
    Ok((state, view, changes))
}

#[test]
fn token_usage_replaces_same_step_samples_and_accumulates_disjoint_buckets() {
    let definition = token_usage_definition();
    let first = usage(10, 5, Some(3), Some(2), Some(4));
    let corrected = usage(12, 7, Some(4), Some(1), Some(6));
    let later = usage(20, 11, None, Some(2), Some(10));
    let events = vec![
        usage_chunk(0, 1, 1, &first),
        assistant_usage(1, 1, 1, &first),
        assistant_usage(2, 1, 1, &corrected),
        usage_chunk(3, 1, 2, &later),
        event(
            4,
            "user/message",
            serde_json::to_value(user("replacement")).unwrap(),
            Some(SurfaceOp::replace(1, 1)),
        ),
    ];
    let (_, view, changes) = fold(&definition, &events).unwrap();
    assert_eq!(changes, 3);
    assert_eq!(
        view,
        json!({
            "uncachedInputTokens":32,
            "outputTokens":18,
            "cacheReadTokens":4,
            "cacheWriteTokens":3
        })
    );
    assert_eq!(definition.state_version, 1);
}

#[test]
fn failed_request_usage_chunk_survives_without_a_final_message() {
    let definition = token_usage_definition();
    let (_, view, _) = fold(
        &definition,
        &[usage_chunk(0, 1, 1, &usage(9, 2, Some(1), None, None))],
    )
    .unwrap();
    assert_eq!(
        view,
        json!({
            "uncachedInputTokens":9,
            "outputTokens":2,
            "cacheReadTokens":1,
            "cacheWriteTokens":0
        })
    );
}

#[test]
fn context_pressure_is_last_wins_prompt_side_and_tracks_surface_movement() {
    let definition = context_pressure_definition();
    let appended = event(
        2,
        "user/message",
        serde_json::to_value(user("abcd")).unwrap(),
        Some(SurfaceOp::append()),
    );
    let price = seekdeep_token_meter::estimate_message(&user("abcd"));
    let events = vec![
        event(0, "request/context", json!({"contextWindow":1000}), None),
        usage_chunk(1, 1, 1, &usage(10, 500, Some(3), Some(2), Some(400))),
        appended,
    ];
    let (_, view, _) = fold(&definition, &events).unwrap();
    assert_eq!(
        view,
        json!({
            "pressureTokens":15,
            "projectedTokens":15 + price,
            "contextWindow":1000
        })
    );

    let newer = vec![
        usage_chunk(0, 1, 1, &usage(10, 500, Some(3), Some(2), None)),
        usage_chunk(1, 1, 2, &usage(20, 999, None, Some(5), None)),
        event(2, "request/context", json!({}), None),
    ];
    let (_, view, _) = fold(&definition, &newer).unwrap();
    assert_eq!(view, json!({"pressureTokens":25,"projectedTokens":25}));
}

#[test]
fn context_pressure_compaction_consumes_shadow_price_and_clamps_below_zero() {
    let definition = context_pressure_definition();
    let head = event(
        0,
        "user/message",
        serde_json::to_value(user(&"long ".repeat(20))).unwrap(),
        Some(SurfaceOp::append()),
    );
    let head_tokens = seekdeep_token_meter::estimate_message(&user(&"long ".repeat(20)));
    let replacement = event(
        3,
        "user/message",
        serde_json::to_value(user("x")).unwrap(),
        Some(SurfaceOp::replace(0, 0)),
    );
    let events = vec![
        head,
        usage_chunk(1, 1, 1, &usage(1, 0, None, None, None)),
        event(
            2,
            "compaction/summary",
            json!({"shadowedRange":{"start":0,"end":0},"shadowedTokenCount":head_tokens}),
            None,
        ),
        replacement,
    ];
    let (_, view, _) = fold(&definition, &events).unwrap();
    assert_eq!(view["pressureTokens"], 1);
    assert_eq!(view["projectedTokens"], 0);

    let no_claim = event(
        1,
        "user/message",
        serde_json::to_value(user("replacement")).unwrap(),
        Some(SurfaceOp::replace(0, 0)),
    );
    let (_, view, _) = fold(
        &definition,
        &[
            usage_chunk(0, 1, 1, &usage(5, 0, None, None, None)),
            no_claim,
        ],
    )
    .unwrap();
    assert_eq!(view["projectedTokens"], 5);
}

#[test]
fn breakdown_prices_latest_envelope_surface_and_metered_replacement() {
    let definition = context_breakdown_definition();
    let tools = vec![ToolSchema {
        name: "read".to_owned(),
        description: "read".to_owned(),
        parameters: Map::from_iter([("type".to_owned(), json!("object"))]),
    }];
    let first_header = header(Some("system"), Some(tools));
    let user_event = event(
        1,
        "user/message",
        serde_json::to_value(user("long message")).unwrap(),
        Some(SurfaceOp::append()),
    );
    let user_tokens = seekdeep_token_meter::estimate_message(&user("long message"));
    let replacement = event(
        3,
        "user/message",
        serde_json::to_value(user("x")).unwrap(),
        Some(SurfaceOp::replace(1, 1)),
    );
    let replacement_tokens = seekdeep_token_meter::estimate_message(&user("x"));
    let events = vec![
        event(
            0,
            "request/header",
            json!({"header":first_header,"reason":"initial"}),
            None,
        ),
        user_event,
        event(
            2,
            "compaction/prune",
            json!({"shadowedRange":{"start":1,"end":1},"shadowedTokenCount":user_tokens}),
            None,
        ),
        replacement,
    ];
    let (_, view, _) = fold(&definition, &events).unwrap();
    assert_eq!(view["messageTokens"], replacement_tokens);
    assert!(view["systemTokens"].as_u64().unwrap() > 0);
    assert!(view["toolsTokens"].as_u64().unwrap() > 0);
    assert_eq!(definition.state_version, 2);
}

#[test]
fn breakdown_empty_assistant_and_restatement_emit_no_spurious_change() {
    let definition = context_breakdown_definition();
    let envelope = header(Some("same"), None);
    let header_event = |seq| {
        event(
            seq,
            "request/header",
            json!({"header":envelope,"reason":"initial"}),
            None,
        )
    };
    let empty = event(
        2,
        "assistant/message",
        json!({
            "turn":1,"step":1,
            "message":{
                "id":"empty","role":"assistant","content":[],
                "source":{"kind":"model","provider":"mock","model":"model"}
            }
        }),
        Some(SurfaceOp::append()),
    );
    let (_, view, changes) = fold(&definition, &[header_event(0), header_event(1), empty]).unwrap();
    assert_eq!(changes, 1);
    assert_eq!(view["messageTokens"], 0);
}

#[test]
fn bounded_surface_fold_is_neutral_without_claim_and_rejects_mismatch() {
    let replacement = event(
        10,
        "user/message",
        serde_json::to_value(user("x")).unwrap(),
        Some(SurfaceOp::replace(1, 2)),
    );
    assert_eq!(
        fold_surface_projection(None, &replacement)
            .unwrap()
            .delta_tokens,
        0
    );
    let error = fold_surface_projection(
        Some(&ShadowPriceClaim {
            start: 3,
            end: 4,
            tokens: 20,
        }),
        &replacement,
    )
    .unwrap_err();
    assert!(error.to_string().contains("armed claim covers 3-4"));
}

#[test]
fn shared_estimator_prices_envelope_parts_independently() {
    assert_eq!(estimate_system_tokens(None), 0);
    assert_eq!(estimate_tools_tokens(None), 0);
    let tools = vec![ToolSchema {
        name: "read".to_owned(),
        description: "read".to_owned(),
        parameters: Map::from_iter([("type".to_owned(), json!("object"))]),
    }];
    let envelope = header(Some("abcd"), Some(tools));
    assert_eq!(estimate_system_tokens(Some(&envelope)), 5);
    assert!(estimate_tools_tokens(Some(&envelope)) > 4);
    assert!(
        estimate_content(&[ContentBlock::Unknown {
            block_type: "future".to_owned(),
            fields: Map::from_iter([("payload".to_owned(), json!("abcd"))]),
        }]) > 4
    );
}

#[tokio::test]
async fn optional_registry_appearance_registers_three_units_and_disposal_removes_them() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    let meter = context
        .plugin(seekdeep_token_meter::plugin(), json!({}))
        .unwrap();
    meter.await_settled().await.unwrap();
    assert!(context.get(SESSION_PROJECTIONS).is_none());
    let projections = SessionProjectionRegistry::install(&context).unwrap();
    let session = sessions
        .create(
            &context,
            Some(SessionId::new("projection-registration")),
            CreateSessionOptions::default(),
        )
        .unwrap();
    let snapshot = projections.snapshot(&session).unwrap();
    assert_eq!(snapshot.values.len(), 3);
    assert_eq!(snapshot.values[TOKEN_USAGE_KEY]["outputTokens"], 0);
    assert_eq!(snapshot.values[CONTEXT_PRESSURE_KEY], json!({}));
    assert_eq!(
        snapshot.values[CONTEXT_BREAKDOWN_KEY],
        json!({"systemTokens":0,"toolsTokens":0,"messageTokens":0})
    );
    meter.dispose().await.unwrap();
    assert!(projections.snapshot(&session).unwrap().values.is_empty());
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn checkpoint_state_stays_bounded_restores_from_json_and_matches_measurement() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    let projections = SessionProjectionRegistry::install(&context).unwrap();
    let meter = context
        .plugin(seekdeep_token_meter::plugin(), json!({}))
        .unwrap();
    meter.await_settled().await.unwrap();
    let service = context.get(seekdeep_token_meter::TOKEN_METER).unwrap();
    let session = sessions
        .create(
            &context,
            Some(SessionId::new("projection-checkpoint")),
            CreateSessionOptions::default(),
        )
        .unwrap();
    for index in 0..40 {
        session
            .append(
                "user/message",
                serde_json::to_value(user(&format!("message-{index}"))).unwrap(),
                AppendOptions {
                    surface_op: Some(SurfaceOp::append()),
                    ..AppendOptions::default()
                },
            )
            .unwrap();
    }
    let before_compaction = service.measure(&session, None).unwrap();
    let first = before_compaction.nodes.first().unwrap();
    session
        .append(
            "compaction/summary",
            json!({
                "shadowedRange":{"start":first.seq,"end":first.seq},
                "shadowedTokenCount":first.tokens
            }),
            AppendOptions::default(),
        )
        .unwrap();
    session
        .append(
            "user/message",
            serde_json::to_value(user("summary")).unwrap(),
            AppendOptions {
                surface_op: Some(SurfaceOp::replace(first.seq, first.seq)),
                source_event_seqs: Some(vec![first.seq]),
                ignorable: false,
            },
        )
        .unwrap();
    let checkpoint = projections.checkpoint(&session).unwrap();
    let wire = serde_json::to_string(&checkpoint).unwrap();
    assert!(!wire.contains("nodes"));
    assert!(wire.len() < 1_500);
    let decoded = serde_json::from_str(&wire).unwrap();
    let restored = projections.restore(&decoded, &[], session.seq()).unwrap();
    assert_eq!(restored.snapshot, projections.snapshot(&session).unwrap());
    assert_eq!(
        restored.snapshot.values[CONTEXT_BREAKDOWN_KEY]["messageTokens"],
        service.measure(&session, None).unwrap().surface_tokens
    );

    let changed = Arc::new(AtomicUsize::new(0));
    let changed_listener = changed.clone();
    projections
        .on_changed(
            &context,
            Arc::new(move |_, _, _, _| {
                changed_listener.fetch_add(1, Ordering::AcqRel);
                Ok(())
            }),
        )
        .unwrap();
    session
        .append("diagnostic/noop", json!({}), AppendOptions::default())
        .unwrap();
    assert_eq!(changed.load(Ordering::Acquire), 0);

    meter.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}
