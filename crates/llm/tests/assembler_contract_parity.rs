//! Canonical chunk-to-message assembly and property contract parity.

use seekdeep_llm::{
    BlockAssembler, CallId, ContentBlock, FinishReason, MessageRole, MessageSource, StreamChunk,
    TokenUsage,
};
use serde_json::json;

use proptest::prelude::*;

#[test]
fn authoritative_close_usage_finish_replay_and_default_message_are_preserved() {
    let mut assembler = BlockAssembler::new();
    assembler.push(StreamChunk::BlockStart {
        index: 7,
        block_type: "text".into(),
    });
    assembler.push(StreamChunk::TextDelta {
        index: 7,
        text: "partial".into(),
    });
    assembler.push(StreamChunk::BlockEnd {
        index: 7,
        block: ContentBlock::Text {
            text: "authoritative".into(),
        },
    });
    let usage = TokenUsage {
        input_tokens: 3,
        output_tokens: 2,
        cache_read_tokens: Some(1),
        cache_write_tokens: None,
        reasoning_tokens: None,
    };
    assembler.push(StreamChunk::Usage {
        usage: usage.clone(),
    });
    assembler.push(StreamChunk::Finish {
        reason: FinishReason::ToolCalls,
        replay_state: Some(json!({"cursor": "next"})),
    });
    assert_eq!(
        assembler.blocks().unwrap(),
        [ContentBlock::Text {
            text: "authoritative".into()
        }]
    );
    assert_eq!(assembler.usage(), Some(&usage));
    assert_eq!(assembler.finish(), FinishReason::ToolCalls);
    assert_eq!(assembler.replay_state(), Some(&json!({"cursor": "next"})));
    let message = assembler.message(None).unwrap();
    assert_eq!(message.role(), MessageRole::Assistant);
    assert_eq!(message.source().kind, "plugin");
    assert_eq!(message.source().fields["plugin"], "seekdeep-llm/assembler");
}

#[test]
fn delta_only_blocks_fallback_tool_identity_and_first_seen_order_match_source() {
    let mut assembler = BlockAssembler::new();
    assembler.push(StreamChunk::TextDelta {
        index: 9,
        text: "visible".into(),
    });
    assembler.push(StreamChunk::ReasoningDelta {
        index: 1,
        text: "thought".into(),
    });
    assembler.push(StreamChunk::ToolCallDelta {
        index: 4,
        id: CallId::new("call-4"),
        name: None,
        arguments_delta: "{".into(),
    });
    assembler.push(StreamChunk::ToolCallDelta {
        index: 4,
        id: CallId::new("call-4"),
        name: Some("lookup".into()),
        arguments_delta: "}".into(),
    });
    assert_eq!(
        assembler.blocks().unwrap(),
        [
            ContentBlock::Text {
                text: "visible".into()
            },
            ContentBlock::Reasoning {
                text: "thought".into()
            },
            ContentBlock::ToolCall {
                id: CallId::new("call-4"),
                name: "lookup".into(),
                arguments: "{}".into()
            }
        ]
    );

    let mut fallback = BlockAssembler::new();
    fallback.push(StreamChunk::BlockStart {
        index: 3,
        block_type: "tool-call".into(),
    });
    assert_eq!(
        fallback.blocks().unwrap(),
        [ContentBlock::ToolCall {
            id: CallId::new("call-3"),
            name: String::new(),
            arguments: String::new()
        }]
    );
}

#[test]
fn duplicate_start_close_and_delta_stragglers_cannot_reopen_or_corrupt_a_block() {
    let mut assembler = BlockAssembler::new();
    assembler.push(StreamChunk::BlockStart {
        index: 0,
        block_type: "text".into(),
    });
    assembler.push(StreamChunk::BlockStart {
        index: 0,
        block_type: "reasoning".into(),
    });
    assembler.push(StreamChunk::TextDelta {
        index: 0,
        text: "first".into(),
    });
    assembler.push(StreamChunk::BlockEnd {
        index: 0,
        block: ContentBlock::Text {
            text: "closed".into(),
        },
    });
    assembler.push(StreamChunk::BlockEnd {
        index: 0,
        block: ContentBlock::Reasoning {
            text: "reclosed".into(),
        },
    });
    assembler.push(StreamChunk::TextDelta {
        index: 0,
        text: "straggler".into(),
    });
    assembler.push(StreamChunk::ToolCallDelta {
        index: 0,
        id: CallId::new("late"),
        name: Some("late".into()),
        arguments_delta: "late".into(),
    });
    assert_eq!(
        assembler.blocks().unwrap(),
        [ContentBlock::Text {
            text: "closed".into()
        }]
    );
}

#[test]
fn finish_defaults_to_stop_last_finish_wins_and_max_tokens_drops_tool_calls_only() {
    let empty = BlockAssembler::new();
    assert_eq!(empty.finish(), FinishReason::Stop);
    assert!(empty.usage().is_none());
    assert!(empty.replay_state().is_none());

    let mut assembler = BlockAssembler::new();
    assembler.push(StreamChunk::TextDelta {
        index: 0,
        text: "keep".into(),
    });
    assembler.push(StreamChunk::ToolCallDelta {
        index: 1,
        id: CallId::new("unsafe-truncated-call"),
        name: Some("tool".into()),
        arguments_delta: "{".into(),
    });
    assembler.push(StreamChunk::Finish {
        reason: FinishReason::Stop,
        replay_state: Some(json!({"old": true})),
    });
    assembler.push(StreamChunk::Finish {
        reason: FinishReason::MaxTokens,
        replay_state: Some(json!({"new": true})),
    });
    assert_eq!(assembler.finish(), FinishReason::MaxTokens);
    assert_eq!(assembler.replay_state(), Some(&json!({"new": true})));
    assert_eq!(
        assembler.blocks().unwrap(),
        [ContentBlock::Text {
            text: "keep".into()
        }]
    );
}

#[test]
fn incomplete_unknown_block_fails_but_an_authoritative_plugin_block_is_retained() {
    let mut incomplete = BlockAssembler::new();
    incomplete.push(StreamChunk::BlockStart {
        index: 0,
        block_type: "plugin-block".into(),
    });
    assert!(
        incomplete
            .blocks()
            .unwrap_err()
            .to_string()
            .contains("cannot assemble incomplete block of type \"plugin-block\"")
    );

    let plugin = ContentBlock::Unknown {
        block_type: "plugin-block".into(),
        fields: serde_json::Map::from_iter([("answer".into(), json!(42))]),
    };
    incomplete.push(StreamChunk::BlockEnd {
        index: 0,
        block: plugin.clone(),
    });
    assert_eq!(incomplete.blocks().unwrap(), [plugin]);
}

#[test]
fn repeated_assembly_is_stable_across_many_interleaved_sequences() {
    for seed in 0_u64..128 {
        let mut assembler = BlockAssembler::new();
        let mut state = seed.wrapping_add(1);
        for step in 0..64_u64 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let index = state % 12;
            match state % 3 {
                0 => assembler.push(StreamChunk::TextDelta {
                    index,
                    text: format!("t{step}"),
                }),
                1 => assembler.push(StreamChunk::ReasoningDelta {
                    index,
                    text: format!("r{step}"),
                }),
                _ => assembler.push(StreamChunk::ToolCallDelta {
                    index,
                    id: CallId::new(format!("call-{index}")),
                    name: Some(format!("tool-{index}")),
                    arguments_delta: format!("{step}"),
                }),
            }
        }
        let first = assembler.blocks();
        let second = assembler.blocks();
        assert_eq!(first.as_ref().ok(), second.as_ref().ok());
        if let Ok(blocks) = first {
            assert!(blocks.len() <= 12);
            assert!(blocks.iter().all(|block| matches!(
                block,
                ContentBlock::Text { .. }
                    | ContentBlock::Reasoning { .. }
                    | ContentBlock::ToolCall { .. }
            )));
        }
    }
}

#[test]
fn caller_supplied_message_source_is_used_verbatim() {
    let mut assembler = BlockAssembler::new();
    assembler.push(StreamChunk::TextDelta {
        index: 0,
        text: "hello".into(),
    });
    let source = MessageSource::plugin("custom-plugin");
    assert_eq!(
        assembler.message(Some(source.clone())).unwrap().source(),
        &source
    );
}

fn arbitrary_text() -> impl Strategy<Value = String> {
    proptest::collection::vec(any::<char>(), 0..16)
        .prop_map(|characters| characters.into_iter().collect())
}

fn arbitrary_chunk() -> impl Strategy<Value = StreamChunk> {
    (0_u64..5, 0_u8..12, arbitrary_text(), arbitrary_text()).prop_map(
        |(index, variant, first, second)| match variant {
            0 => StreamChunk::BlockStart {
                index,
                block_type: "text".to_owned(),
            },
            1 => StreamChunk::BlockStart {
                index,
                block_type: "reasoning".to_owned(),
            },
            2 => StreamChunk::BlockStart {
                index,
                block_type: "tool-call".to_owned(),
            },
            3 => StreamChunk::TextDelta { index, text: first },
            4 => StreamChunk::ReasoningDelta { index, text: first },
            5 => StreamChunk::ToolCallDelta {
                index,
                id: CallId::new(format!("call-{index}-{first}")),
                name: Some(second),
                arguments_delta: first,
            },
            6 => StreamChunk::BlockEnd {
                index,
                block: ContentBlock::Text { text: first },
            },
            7 => StreamChunk::BlockEnd {
                index,
                block: ContentBlock::Reasoning { text: first },
            },
            8 => StreamChunk::BlockEnd {
                index,
                block: ContentBlock::ToolCall {
                    id: CallId::new(format!("closed-{index}")),
                    name: first,
                    arguments: second,
                },
            },
            9 => StreamChunk::Usage {
                usage: TokenUsage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                },
            },
            10 => StreamChunk::Finish {
                reason: FinishReason::ToolCalls,
                replay_state: None,
            },
            _ => StreamChunk::Finish {
                reason: FinishReason::Error {
                    failure: seekdeep_llm::LlmFailure {
                        message: format!("failure-{first}"),
                        code: "UNKNOWN".to_owned(),
                        status: None,
                        provider_retry_after_ms: None,
                        request_id: None,
                    },
                },
                replay_state: None,
            },
        },
    )
}

fn feed(chunks: &[StreamChunk]) -> BlockAssembler {
    let mut assembler = BlockAssembler::new();
    for chunk in chunks {
        assembler.push(chunk.clone());
    }
    assembler
}

fn chunk_index(chunk: &StreamChunk) -> Option<u64> {
    match chunk {
        StreamChunk::BlockStart { index, .. }
        | StreamChunk::TextDelta { index, .. }
        | StreamChunk::ReasoningDelta { index, .. }
        | StreamChunk::ToolCallDelta { index, .. }
        | StreamChunk::BlockEnd { index, .. } => Some(*index),
        StreamChunk::Usage { .. } | StreamChunk::Finish { .. } => None,
    }
}

proptest! {
    #[test]
    fn partial_count_never_exceeds_distinct_indices(
        chunks in proptest::collection::vec(arbitrary_chunk(), 0..31),
    ) {
        let distinct = chunks.iter().filter_map(chunk_index).collect::<std::collections::HashSet<_>>();
        let blocks = feed(&chunks).blocks();
        prop_assert!(blocks.is_ok(), "known chunks must always assemble");
        prop_assert!(blocks.unwrap_or_default().len() <= distinct.len());
    }

    #[test]
    fn repeated_arbitrary_assembly_is_idempotent(
        chunks in proptest::collection::vec(arbitrary_chunk(), 0..31),
    ) {
        let assembler = feed(&chunks);
        let first = assembler.blocks();
        prop_assert!(first.is_ok(), "known chunks must always assemble");
        let first = first.unwrap_or_default();
        prop_assert_eq!(&first, &assembler.blocks().unwrap_or_default());
        let message = assembler.message(None);
        prop_assert!(message.is_ok(), "known chunks must always form a message");
        let message = message.expect("checked above");
        prop_assert_eq!(&first, message.content());
    }

    #[test]
    fn arbitrary_known_chunks_only_assemble_known_blocks(
        chunks in proptest::collection::vec(arbitrary_chunk(), 0..31),
    ) {
        let blocks = feed(&chunks).blocks();
        prop_assert!(blocks.is_ok(), "known chunks must always assemble");
        for block in blocks.unwrap_or_default() {
            let is_known = matches!(
                block,
                ContentBlock::Text { .. }
                    | ContentBlock::Reasoning { .. }
                    | ContentBlock::ToolCall { .. }
            );
            prop_assert!(is_known, "only known blocks may be assembled");
        }
    }

    #[test]
    fn arbitrary_finish_is_last_write_wins_or_stop(
        chunks in proptest::collection::vec(arbitrary_chunk(), 0..31),
    ) {
        let expected = chunks.iter().rev().find_map(|chunk| match chunk {
            StreamChunk::Finish { reason, .. } => Some(reason.clone()),
            _ => None,
        }).unwrap_or(FinishReason::Stop);
        prop_assert_eq!(feed(&chunks).finish(), expected);
    }
}
