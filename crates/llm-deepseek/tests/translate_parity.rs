//! Behavioral mirror of `packages/llm/llm-deepseek/tests/translate.spec.ts`.

use futures::{TryStreamExt as _, stream};
use seekdeep_llm::{
    ContentBlock, EMPTY_RESPONSE_CODE, FinishReason, LlmError, StreamChunk, TokenUsage,
};
use seekdeep_llm_deepseek::{
    sse::{DONE, PayloadStream},
    translate::{map_finish_reason, map_usage, translate},
    types::{WireCompletionTokenDetails, WirePromptTokenDetails, WireUsage},
};
use serde_json::{Value, json};

fn feed(payloads: Vec<Value>) -> PayloadStream {
    let payloads = payloads
        .into_iter()
        .map(|payload| {
            Ok(if payload == json!(DONE) {
                DONE.to_owned()
            } else if let Value::String(raw) = payload {
                raw
            } else {
                payload.to_string()
            })
        })
        .collect::<Vec<anyhow::Result<String>>>();
    Box::pin(stream::iter(payloads))
}

async fn collect(payloads: Vec<Value>) -> anyhow::Result<Vec<StreamChunk>> {
    translate(feed(payloads)).try_collect().await
}

fn first_chunk() -> Value {
    json!({"choices":[{"delta":{"role":"assistant","content":null,"reasoning_content":""}}]})
}

fn finish(reason: FinishReason) -> StreamChunk {
    StreamChunk::Finish {
        reason,
        replay_state: None,
    }
}

#[tokio::test]
async fn streams_text_and_defers_ends_usage_and_finish_until_done() {
    let chunks = collect(vec![
        first_chunk(),
        json!({"choices":[{"delta":{"content":"Hel"}}]}),
        json!({"choices":[{"delta":{"content":"lo"}}]}),
        json!({"choices":[{"delta":{"content":""},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":2}}),
        json!(DONE),
    ])
    .await
    .unwrap();
    assert_eq!(
        chunks,
        vec![
            StreamChunk::BlockStart {
                index: 0,
                block_type: "text".to_owned()
            },
            StreamChunk::TextDelta {
                index: 0,
                text: "Hel".to_owned()
            },
            StreamChunk::TextDelta {
                index: 0,
                text: "lo".to_owned()
            },
            StreamChunk::BlockEnd {
                index: 0,
                block: ContentBlock::Text {
                    text: "Hello".to_owned()
                }
            },
            StreamChunk::Usage {
                usage: TokenUsage {
                    input_tokens: 5,
                    output_tokens: 2,
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None
                }
            },
            finish(FinishReason::Stop)
        ]
    );
}

#[tokio::test]
async fn empty_initial_reasoning_is_ignored_but_real_reasoning_precedes_text() {
    let chunks = collect(vec![
        first_chunk(),
        json!({"choices":[{"delta":{"reasoning_content":"think","content":null}}]}),
        json!({"choices":[{"delta":{"reasoning_content":"ing","content":null}}]}),
        json!({"choices":[{"delta":{"reasoning_content":null,"content":"answer"}}]}),
        json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
        json!(DONE),
    ])
    .await
    .unwrap();
    assert_eq!(
        chunks
            .iter()
            .filter(|chunk| matches!(chunk, StreamChunk::BlockStart { .. }))
            .cloned()
            .collect::<Vec<_>>(),
        [
            StreamChunk::BlockStart {
                index: 0,
                block_type: "reasoning".to_owned()
            },
            StreamChunk::BlockStart {
                index: 1,
                block_type: "text".to_owned()
            }
        ]
    );
    assert_eq!(
        chunks
            .iter()
            .filter(|chunk| matches!(chunk, StreamChunk::BlockEnd { .. }))
            .cloned()
            .collect::<Vec<_>>(),
        [
            StreamChunk::BlockEnd {
                index: 0,
                block: ContentBlock::Reasoning {
                    text: "thinking".to_owned()
                }
            },
            StreamChunk::BlockEnd {
                index: 1,
                block: ContentBlock::Text {
                    text: "answer".to_owned()
                }
            }
        ]
    );
}

#[tokio::test]
async fn reassembles_fragmented_and_parallel_tool_calls_by_wire_index() {
    let chunks = collect(vec![
        first_chunk(),
        json!({"choices":[{"delta":{"tool_calls":[
            {"index":0,"id":"a","type":"function","function":{"name":"one","arguments":"{"}},
            {"index":1,"id":"b","type":"function","function":{"name":"two","arguments":""}}
        ]}}]}),
        json!({"choices":[{"delta":{"tool_calls":[
            {"index":0,"function":{"arguments":"}"}},
            {"index":1,"function":{"arguments":"{}"}}
        ]}}]}),
        json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
        json!(DONE),
    ])
    .await
    .unwrap();
    assert_eq!(
        chunks
            .iter()
            .filter(|chunk| matches!(chunk, StreamChunk::BlockStart { .. }))
            .count(),
        2
    );
    assert_eq!(
        chunks
            .iter()
            .filter(|chunk| matches!(chunk, StreamChunk::BlockEnd { .. }))
            .cloned()
            .collect::<Vec<_>>(),
        [
            StreamChunk::BlockEnd {
                index: 0,
                block: ContentBlock::ToolCall {
                    id: seekdeep_llm::CallId::new("a"),
                    name: "one".to_owned(),
                    arguments: "{}".to_owned()
                }
            },
            StreamChunk::BlockEnd {
                index: 1,
                block: ContentBlock::ToolCall {
                    id: seekdeep_llm::CallId::new("b"),
                    name: "two".to_owned(),
                    arguments: "{}".to_owned()
                }
            }
        ]
    );
    assert_eq!(chunks.last(), Some(&finish(FinishReason::ToolCalls)));
}

#[tokio::test]
async fn nameless_tool_deltas_open_once_and_use_empty_fallbacks() {
    let chunks = collect(vec![
        first_chunk(),
        json!({"choices":[{"delta":{"tool_calls":[{"index":0}]}}]}),
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{}}]}}]}),
        json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
        json!(DONE),
    ])
    .await
    .unwrap();
    assert_eq!(
        chunks
            .iter()
            .filter(|chunk| matches!(chunk, StreamChunk::BlockStart { .. }))
            .count(),
        1
    );
    assert_eq!(
        chunks[1],
        StreamChunk::ToolCallDelta {
            index: 0,
            id: seekdeep_llm::CallId::new(""),
            name: None,
            arguments_delta: String::new()
        }
    );
}

#[tokio::test]
async fn trailing_latest_usage_wins_and_stop_without_blocks_is_empty_response() {
    let chunks = collect(vec![
        first_chunk(),
        json!({"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}),
        json!({"choices":[],"usage":{"prompt_tokens":2,"completion_tokens":0}}),
        json!(DONE),
    ])
    .await
    .unwrap();
    assert_eq!(
        chunks[0],
        StreamChunk::Usage {
            usage: TokenUsage {
                input_tokens: 2,
                output_tokens: 0,
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None
            }
        }
    );
    let StreamChunk::Finish {
        reason: FinishReason::Error { failure },
        ..
    } = &chunks[1]
    else {
        panic!("expected empty-response finish")
    };
    assert_eq!(failure.code, EMPTY_RESPONSE_CODE);
}

#[tokio::test]
async fn non_stop_empty_and_reasoning_only_completions_remain_successful() {
    let length = collect(vec![
        first_chunk(),
        json!({"choices":[{"delta":{},"finish_reason":"length"}]}),
        json!(DONE),
    ])
    .await
    .unwrap();
    assert_eq!(length.last(), Some(&finish(FinishReason::MaxTokens)));

    let reasoning = collect(vec![
        first_chunk(),
        json!({"choices":[{"delta":{"reasoning_content":"mull"}}]}),
        json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
        json!(DONE),
    ])
    .await
    .unwrap();
    assert_eq!(reasoning.last(), Some(&finish(FinishReason::Stop)));
}

#[tokio::test]
async fn malformed_json_and_payload_eof_have_stable_codes() {
    let malformed = collect(vec![json!("{bad json")]).await.unwrap_err();
    assert!(malformed.to_string().contains("malformed SSE payload"));
    assert_eq!(
        malformed.downcast_ref::<LlmError>().unwrap().code(),
        "MALFORMED_RESPONSE"
    );

    let closed = collect(vec![first_chunk()]).await.unwrap_err();
    assert!(closed.to_string().contains("without [DONE]"));
    assert_eq!(
        closed.downcast_ref::<LlmError>().unwrap().code(),
        "STREAM_CLOSED"
    );
}

#[test]
fn finish_reason_and_usage_mapping_are_exact() {
    assert_eq!(map_finish_reason("stop"), FinishReason::Stop);
    assert_eq!(map_finish_reason("tool_calls"), FinishReason::ToolCalls);
    assert_eq!(map_finish_reason("length"), FinishReason::MaxTokens);
    let FinishReason::Error { failure } = map_finish_reason("content_filter") else {
        panic!("expected error finish")
    };
    assert_eq!(failure.message, "model stopped: content_filter");
    assert_eq!(failure.code, "CONTENT_FILTER");

    assert_eq!(
        map_usage(&WireUsage {
            prompt_tokens: 283,
            completion_tokens: 69,
            prompt_cache_hit_tokens: Some(255),
            prompt_cache_miss_tokens: Some(28),
            prompt_tokens_details: Some(WirePromptTokenDetails {
                cached_tokens: Some(256)
            }),
            completion_tokens_details: Some(WireCompletionTokenDetails {
                reasoning_tokens: Some(24)
            })
        })
        .unwrap(),
        TokenUsage {
            input_tokens: 27,
            output_tokens: 69,
            cache_read_tokens: Some(256),
            cache_write_tokens: None,
            reasoning_tokens: Some(24)
        }
    );
}
