//! Assistant chunk folding, sparse compaction, and reference-discipline parity.

use std::rc::Rc;

use seekdeep_client_runtime::*;
use serde_json::json;

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
            block_type: block_type.to_owned(),
        });
    }
    assert_eq!(
        accumulator.partial().blocks.as_ref(),
        &[
            Rc::new(AssistantBlock::Text {
                text: String::new()
            }),
            Rc::new(AssistantBlock::Reasoning {
                text: String::new()
            }),
            Rc::new(AssistantBlock::ToolCall {
                call_id: String::new(),
                name: String::new(),
                args_raw: String::new()
            }),
            Rc::new(AssistantBlock::Other {
                block: serde_json::Value::Null
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
            text: text.to_owned(),
        });
    }
    assert_eq!(
        accumulator.partial().blocks[0].as_ref(),
        &AssistantBlock::Text {
            text: "无 start 也累积".to_owned()
        }
    );
    accumulator.push(&PartialChunk::ReasoningDelta {
        index: 0,
        text: "换型重起".to_owned(),
    });
    assert_eq!(
        accumulator.partial().blocks[0].as_ref(),
        &AssistantBlock::Reasoning {
            text: "换型重起".to_owned()
        }
    );
}

#[test]
fn history_prefix_tool_deltas_and_block_end_follow_source_rules() {
    let mut accumulator = PartialAccumulator::new(
        1,
        0,
        vec![Rc::new(AssistantBlock::Text {
            text: "已有".to_owned(),
        })],
    );
    accumulator.push(&PartialChunk::TextDelta {
        index: 0,
        text: "增量".to_owned(),
    });
    accumulator.push(&PartialChunk::ToolCallDelta {
        index: 1,
        id: "c1".to_owned(),
        name: None,
        arguments_delta: "{\"a\"".to_owned(),
    });
    accumulator.push(&PartialChunk::ToolCallDelta {
        index: 1,
        id: "late".to_owned(),
        name: Some("echo".to_owned()),
        arguments_delta: ":1}".to_owned(),
    });
    assert_eq!(
        accumulator.partial().blocks[1].as_ref(),
        &AssistantBlock::ToolCall {
            call_id: "c1".to_owned(),
            name: "echo".to_owned(),
            args_raw: "{\"a\":1}".to_owned(),
        }
    );
    accumulator.push(&PartialChunk::BlockEnd {
        index: 0,
        block: json!({"type":"text","text":"定稿全文"}),
    });
    assert_eq!(
        accumulator.partial().blocks[0].as_ref(),
        &AssistantBlock::Text {
            text: "定稿全文".to_owned()
        }
    );
}

#[test]
fn invisible_variants_keep_snapshot_and_sparse_indexes_compact_in_order() {
    let mut accumulator = PartialAccumulator::new(3, 1, Vec::new());
    let first = accumulator.partial();
    assert!(Rc::ptr_eq(&first, &accumulator.partial()));
    assert!(!accumulator.push(&PartialChunk::Other {
        chunk_type: "usage".to_owned()
    }));
    assert!(Rc::ptr_eq(&first, &accumulator.partial()));
    accumulator.push(&PartialChunk::BlockStart {
        index: 2,
        block_type: "text".to_owned(),
    });
    accumulator.push(&PartialChunk::TextDelta {
        index: 2,
        text: "高位".to_owned(),
    });
    accumulator.push(&PartialChunk::BlockStart {
        index: 0,
        block_type: "reasoning".to_owned(),
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
            Rc::new(AssistantBlock::Text {
                text: "a".to_owned(),
            }),
            Rc::new(AssistantBlock::Text {
                text: "stable".to_owned(),
            }),
        ],
    );
    let before = accumulator.partial();
    accumulator.push(&PartialChunk::TextDelta {
        index: 0,
        text: "b".to_owned(),
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
