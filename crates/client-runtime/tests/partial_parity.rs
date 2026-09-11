//! Assistant chunk folding, sparse compaction, and reference-discipline parity.

use std::rc::Rc;

use seekdeep_client_runtime::*;
use seekdeep_lossless_json::{JsonString, JsonValue as Value};

macro_rules! json {
    ($($tokens:tt)*) => { Value::from(serde_json::json!($($tokens)*)) };
}

#[test]
fn block_start_builds_empty_known_and_unknown_blocks() {
    let mut accumulator = PartialAccumulator::new(1, 0, Vec::new());
    for (index, block_type) in [
        (0, "text"),
        (1, "reasoning"),
        (2, "tool-call"),
        (3, "unknown"),
    ] {
        accumulator.push(&PartialChunk::BlockStart {
            index,
            block_type: block_type.into(),
        });
    }
    assert_eq!(
        accumulator.partial().blocks.as_ref(),
        &[
            Rc::new(AssistantBlock::Text {
                text: JsonString::default()
            }),
            Rc::new(AssistantBlock::Reasoning {
                text: JsonString::default()
            }),
            Rc::new(AssistantBlock::ToolCall {
                call_id: JsonString::default(),
                name: JsonString::default(),
                args_raw: JsonString::default()
            }),
            Rc::new(AssistantBlock::Other {
                block: Value::from(serde_json::Value::Null)
            }),
        ]
    );
}

#[test]
fn text_and_reasoning_accumulate_and_restart_when_lane_changes() {
    let mut accumulator = PartialAccumulator::new(1, 0, Vec::new());
    for text in ["无 start ", "也累积"] {
        accumulator.push(&PartialChunk::TextDelta {
            index: 0,
            text: text.into(),
        });
    }
    assert_eq!(
        accumulator.partial().blocks[0].as_ref(),
        &AssistantBlock::Text {
            text: "无 start 也累积".into()
        }
    );
    accumulator.push(&PartialChunk::ReasoningDelta {
        index: 0,
        text: "换型重起".into(),
    });
    assert_eq!(
        accumulator.partial().blocks[0].as_ref(),
        &AssistantBlock::Reasoning {
            text: "换型重起".into()
        }
    );
}

#[test]
fn history_prefix_tool_deltas_and_block_end_follow_source_rules() {
    let mut accumulator = PartialAccumulator::new(
        1,
        0,
        vec![Rc::new(AssistantBlock::Text {
            text: "已有".into(),
        })],
    );
    accumulator.push(&PartialChunk::TextDelta {
        index: 0,
        text: "增量".into(),
    });
    accumulator.push(&PartialChunk::ToolCallDelta {
        index: 1,
        id: "c1".into(),
        name: None,
        arguments_delta: "{\"a\"".into(),
    });
    accumulator.push(&PartialChunk::ToolCallDelta {
        index: 1,
        id: "late".into(),
        name: Some("echo".into()),
        arguments_delta: ":1}".into(),
    });
    assert_eq!(
        accumulator.partial().blocks[1].as_ref(),
        &AssistantBlock::ToolCall {
            call_id: "c1".into(),
            name: "echo".into(),
            args_raw: "{\"a\":1}".into(),
        }
    );
    accumulator.push(&PartialChunk::BlockEnd {
        index: 0,
        block: json!({"type":"text","text":"定稿全文"}),
    });
    assert_eq!(
        accumulator.partial().blocks[0].as_ref(),
        &AssistantBlock::Text {
            text: "定稿全文".into()
        }
    );
}

#[test]
fn invisible_variants_keep_snapshot_and_sparse_indexes_compact_in_order() {
    let mut accumulator = PartialAccumulator::new(3, 1, Vec::new());
    let first = accumulator.partial();
    assert!(Rc::ptr_eq(&first, &accumulator.partial()));
    assert!(!accumulator.push(&PartialChunk::Other {
        chunk_type: "usage".into()
    }));
    assert!(Rc::ptr_eq(&first, &accumulator.partial()));
    accumulator.push(&PartialChunk::BlockStart {
        index: 2,
        block_type: "text".into(),
    });
    accumulator.push(&PartialChunk::TextDelta {
        index: 2,
        text: "高位".into(),
    });
    accumulator.push(&PartialChunk::BlockStart {
        index: 0,
        block_type: "reasoning".into(),
    });
    let second = accumulator.partial();
    assert!(!Rc::ptr_eq(&first, &second));
    assert_eq!(second.blocks.len(), 2);
    assert!(matches!(
        second.blocks[0].as_ref(),
        AssistantBlock::Reasoning { .. }
    ));
    assert!(matches!(
        second.blocks[1].as_ref(),
        AssistantBlock::Text { .. }
    ));
}

#[test]
fn each_delta_replaces_only_its_block_reference() {
    let mut accumulator = PartialAccumulator::new(
        1,
        0,
        vec![
            Rc::new(AssistantBlock::Text { text: "a".into() }),
            Rc::new(AssistantBlock::Text {
                text: "stable".into(),
            }),
        ],
    );
    let before = accumulator.partial();
    accumulator.push(&PartialChunk::TextDelta {
        index: 0,
        text: "b".into(),
    });
    let after = accumulator.partial();
    assert!(!Rc::ptr_eq(&before.blocks[0], &after.blocks[0]));
    assert!(Rc::ptr_eq(&before.blocks[1], &after.blocks[1]));
}

#[test]
fn visible_chunk_discriminants_match_source() {
    for chunk_type in [
        "block-start",
        "text-delta",
        "reasoning-delta",
        "tool-call-delta",
        "block-end",
    ] {
        assert!(is_visible_assistant_chunk(chunk_type));
    }
    for chunk_type in ["usage", "finish", "future"] {
        assert!(!is_visible_assistant_chunk(chunk_type));
    }
}

#[test]
fn streamed_surrogates_join_without_replacing_unchanged_blocks_or_raw_payloads() {
    let high = JsonString::from_utf16(&[0xd800]);
    let low = JsonString::from_utf16(&[0xdc00]);
    let raw =
        Value::parse(r#"{"type":"future","payload":{"\udfff":"\ud800"}}"#.to_owned()).unwrap();
    let mut accumulator = PartialAccumulator::new(1, 0, vec![Rc::new(to_assistant_block(&raw))]);
    let before = accumulator.partial();
    accumulator.push(&PartialChunk::TextDelta {
        index: 1,
        text: high.clone(),
    });
    let mid = accumulator.partial();
    assert!(Rc::ptr_eq(&before.blocks[0], &mid.blocks[0]));
    assert_eq!(mid.blocks[1].as_ref(), &AssistantBlock::Text { text: high });
    accumulator.push(&PartialChunk::TextDelta {
        index: 1,
        text: low,
    });
    let after = accumulator.partial();
    assert!(Rc::ptr_eq(&mid.blocks[0], &after.blocks[0]));
    assert_eq!(
        after.blocks[1].as_ref(),
        &AssistantBlock::Text {
            text: JsonString::from_utf16(&[0xd800, 0xdc00])
        }
    );
    assert_eq!(
        after.blocks[0].as_ref(),
        &AssistantBlock::Other { block: raw }
    );
}
