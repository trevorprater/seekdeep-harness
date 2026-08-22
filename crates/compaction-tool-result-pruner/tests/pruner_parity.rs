//! Behavioral parity tests for the deterministic tool-result pruner.

use seekdeep_compaction_tool_result_pruner::{
    DEFAULTS, PRUNE_MARKER, ToolResultPruneConfig, ToolResultPruner, code_point_length,
    resolve_config,
};
use seekdeep_cordis::Context;
use seekdeep_core::session::{AppendOptions, Session, SessionId, SurfaceOp};
use seekdeep_llm::{CallId, ContentBlock, Message};
use seekdeep_token_meter::{TokenMeterConfig, install as install_token_meter};
use serde_json::{Value, json};

fn default_pruner() -> ToolResultPruner {
    let context = Context::new();
    ToolResultPruner::new(&context, &ToolResultPruneConfig::default())
        .expect("pruner")
        .as_ref()
        .clone()
}

#[test]
fn counts_code_points_without_splitting_surrogate_pairs() {
    assert_eq!(code_point_length("hello"), 5);
    assert_eq!(code_point_length(""), 0);
    assert_eq!(code_point_length("😀"), 1);
    assert_eq!(code_point_length("a😀b"), 3);
}

#[test]
fn resolves_defaults_and_rejects_oversized_emitted_budget() {
    let resolved = resolve_config(&ToolResultPruneConfig::default()).expect("defaults");
    assert_eq!(resolved.threshold_chars, DEFAULTS.threshold_chars);
    assert_eq!(resolved.head_chars, DEFAULTS.head_chars);
    assert_eq!(resolved.tail_chars, DEFAULTS.tail_chars);

    let oversized = ToolResultPruneConfig {
        threshold_chars: Some(100),
        head_chars: Some(60),
        tail_chars: Some(60),
    };
    let error = resolve_config(&oversized).expect_err("must reject");
    assert!(
        error.to_string().contains("headChars + marker + tailChars"),
        "{error}"
    );
}

#[test]
fn measures_only_text_blocks() {
    let pruner = default_pruner();
    let blocks = vec![
        ContentBlock::Text {
            text: "abcd".to_owned(),
        },
        ContentBlock::Reasoning {
            text: "ignored".to_owned(),
        },
        ContentBlock::Text {
            text: "ef".to_owned(),
        },
    ];
    assert_eq!(pruner.measure_content(&blocks), 6);
}

#[test]
fn prunes_an_over_budget_text_middle() {
    let pruner = default_pruner();
    let text = "a".repeat(9_000);
    let pruned_blocks = pruner
        .prune_content(&[ContentBlock::Text { text }])
        .expect("over-budget must prune");
    let ContentBlock::Text { text } = &pruned_blocks[0] else {
        panic!("expected a text block");
    };
    assert!(
        text.contains(PRUNE_MARKER),
        "{text:?} must carry the marker"
    );
    assert!(text.starts_with(&"a".repeat(DEFAULTS.head_chars)));
    assert!(text.ends_with(&"a".repeat(DEFAULTS.tail_chars)));
    assert!(pruner.measure_content(&pruned_blocks) < 9_000);
}

#[test]
fn leaves_within_budget_content_untouched() {
    let pruner = default_pruner();
    let blocks = [ContentBlock::Text {
        text: "short".to_owned(),
    }];
    assert!(pruner.prune_content(&blocks).is_none());
}

#[test]
fn preserves_rich_block_order_and_supports_zero_head_and_tail() {
    let context = Context::new();
    let config = ToolResultPruneConfig {
        threshold_chars: Some(50),
        head_chars: Some(4),
        tail_chars: Some(3),
    };
    let pruner = ToolResultPruner::new(&context, &config).expect("pruner");
    let reasoning = ContentBlock::Reasoning {
        text: "private-rich-block".to_owned(),
    };
    let call = ContentBlock::ToolCall {
        id: CallId::new("nested"),
        name: "nested".to_owned(),
        arguments: "{}".to_owned(),
    };
    let result = pruner
        .prune_content(&[
            ContentBlock::Text {
                text: "A".repeat(40),
            },
            reasoning.clone(),
            ContentBlock::Text {
                text: "B".repeat(30),
            },
            call.clone(),
            ContentBlock::Text {
                text: "C".repeat(30),
            },
        ])
        .expect("pruned");
    assert_eq!(
        result,
        [
            ContentBlock::Text {
                text: format!("AAAA{PRUNE_MARKER}"),
            },
            reasoning,
            call,
            ContentBlock::Text {
                text: "CCC".to_owned(),
            },
        ]
    );
    assert!(pruner.measure_content(&result) <= 50);

    let marker_only = ToolResultPruner::new(
        &Context::new(),
        &ToolResultPruneConfig {
            threshold_chars: Some(u64::try_from(code_point_length(PRUNE_MARKER)).expect("marker")),
            head_chars: Some(0),
            tail_chars: Some(0),
        },
    )
    .expect("marker-only pruner");
    assert_eq!(
        marker_only.prune_content(&[ContentBlock::Text {
            text: "x".repeat(100),
        }]),
        Some(vec![ContentBlock::Text {
            text: PRUNE_MARKER.to_owned(),
        }])
    );
}

fn append_tool_step(
    session: &Session,
    turn: u64,
    call: &str,
    content: Vec<ContentBlock>,
    extra: &Value,
) -> (u64, Message) {
    let call_id = CallId::new(call);
    session
        .append(
            "turn/start",
            json!({"turn": turn}),
            AppendOptions::default(),
        )
        .expect("turn start");
    session
        .append(
            "step/start",
            json!({"turn": turn, "step": 1}),
            AppendOptions::default(),
        )
        .expect("step start");
    let assistant = Message::assistant(
        vec![ContentBlock::ToolCall {
            id: call_id.clone(),
            name: "bash".to_owned(),
            arguments: "{}".to_owned(),
        }],
        "test-model",
        "test-model",
    );
    session
        .append(
            "assistant/message",
            json!({"turn": turn, "step": 1, "message": assistant}),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .expect("assistant");
    session
        .append(
            "tool/call",
            json!({
                "turn": turn,
                "step": 1,
                "callId": call_id,
                "name": "bash",
                "arguments": "{}",
            }),
            AppendOptions::default(),
        )
        .expect("tool call");
    let message = Message::tool_result(&CallId::new(call), content, false);
    let mut data = json!({"turn": turn, "step": 1, "message": message});
    if let (Some(data), Some(extra)) = (data.as_object_mut(), extra.as_object()) {
        data.extend(extra.clone());
    }
    let result = session
        .append(
            "tool/result",
            data,
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .expect("tool result");
    session
        .append(
            "step/end",
            json!({"turn": turn, "step": 1}),
            AppendOptions::default(),
        )
        .expect("step end");
    session
        .append(
            "turn/end",
            json!({"turn": turn, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("turn end");
    (result.seq, message)
}

fn small_pruner(context: &Context) -> std::sync::Arc<ToolResultPruner> {
    ToolResultPruner::new(
        context,
        &ToolResultPruneConfig {
            threshold_chars: Some(50),
            head_chars: Some(4),
            tail_chars: Some(3),
        },
    )
    .expect("pruner")
}

#[test]
fn session_prune_preserves_data_prices_and_cites_the_replaced_result() {
    let context = Context::new();
    let meter = install_token_meter(&context, TokenMeterConfig::default()).expect("meter");
    let pruner = small_pruner(&context);
    let session = Session::create(&SessionId::new("preserve"), None, None).expect("session");
    let (original_seq, original_message) = append_tool_step(
        &session,
        1,
        "one",
        vec![ContentBlock::Text {
            text: "x".repeat(100),
        }],
        &json!({
            "isError": true,
            "error": {"name": "ExitError", "code": "EXIT_1"},
            "meta": {"diff": ["a", "b"]},
            "futureField": {"nested": true},
        }),
    );
    session
        .append("turn/start", json!({"turn": 2}), AppendOptions::default())
        .expect("open turn");

    let result = pruner.prune_session(&session).expect("prune");
    assert_eq!(result.pruned.len(), 1);
    let entry = &result.pruned[0];
    assert_eq!(entry.original_seq, original_seq);
    assert_eq!(entry.call_id.as_str(), "one");
    assert_eq!(entry.chars_before, 100);
    assert!(entry.chars_after <= 50);
    assert_eq!(result.chars_removed, entry.chars_before - entry.chars_after);

    let events = session.events();
    let replacement = &events[usize::try_from(entry.replacement_seq).expect("replacement index")];
    assert_eq!(replacement.event_type, "tool/result");
    assert_eq!(replacement.data["isError"], true);
    assert_eq!(
        replacement.data["error"],
        json!({"name": "ExitError", "code": "EXIT_1"})
    );
    assert_eq!(replacement.data["meta"], json!({"diff": ["a", "b"]}));
    assert_eq!(replacement.data["futureField"], json!({"nested": true}));
    assert_eq!(
        replacement.surface_op,
        Some(SurfaceOp::replace(original_seq, original_seq))
    );
    assert_eq!(replacement.source_event_seqs, Some(vec![original_seq]));
    assert!(!session.surface_nodes().contains(&original_seq));

    let price = &events[usize::try_from(entry.replacement_seq - 1).expect("price index")];
    assert_eq!(price.event_type, "compaction/prune");
    assert_eq!(
        price.data["shadowedRange"],
        json!({"start": original_seq, "end": original_seq})
    );
    assert_eq!(price.data["shadowedSeqs"], json!([original_seq]));
    assert_eq!(
        price.data["shadowedTokenCount"],
        meter.estimate_message(&original_message)
    );

    let replay = Session::create(
        session.id(),
        Some(session.events()),
        Some(session.header().clone()),
    )
    .expect("replay");
    assert_eq!(replay.derive_messages(), session.derive_messages());
    assert_eq!(replay.replace_generation(), session.replace_generation());
    assert_eq!(
        pruner.prune_session(&session).expect("converged"),
        seekdeep_compaction_tool_result_pruner::PruneResult {
            pruned: Vec::new(),
            chars_removed: 0,
        }
    );
}

#[test]
fn session_prune_uses_stable_surface_order_and_skips_short_results() {
    let context = Context::new();
    install_token_meter(&context, TokenMeterConfig::default()).expect("meter");
    let pruner = small_pruner(&context);
    let session = Session::create(&SessionId::new("multiple"), None, None).expect("session");
    append_tool_step(
        &session,
        1,
        "a",
        vec![ContentBlock::Text {
            text: "A".repeat(100),
        }],
        &json!({}),
    );
    append_tool_step(
        &session,
        2,
        "b",
        vec![ContentBlock::Text {
            text: "short".to_owned(),
        }],
        &json!({}),
    );
    append_tool_step(
        &session,
        3,
        "c",
        vec![ContentBlock::Text {
            text: "C".repeat(80),
        }],
        &json!({}),
    );
    session
        .append("turn/start", json!({"turn": 4}), AppendOptions::default())
        .expect("open turn");
    let result = pruner.prune_session(&session).expect("prune");
    assert_eq!(
        result
            .pruned
            .iter()
            .map(|entry| entry.call_id.as_str())
            .collect::<Vec<_>>(),
        ["a", "c"]
    );
    assert_eq!(
        result.chars_removed,
        result
            .pruned
            .iter()
            .map(|entry| entry.chars_before - entry.chars_after)
            .sum::<usize>()
    );
}
