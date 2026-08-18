//! Native assistant-event stream translation parity tests.

use std::{error::Error, fmt, sync::Arc};

use futures::{TryStreamExt, stream};
use seekdeep_llm::{CallId, FinishReason, LlmError, ModelId, ProviderId, StreamChunk};
use seekdeep_llm_pi_ai::{
    replay::{
        PiApi, PiAssistantBlock, PiAssistantMessage, PiAssistantRole, PiCost, PiStopReason, PiUsage,
    },
    stream::{PiAssistantEvent, PiToolCall, map_stop_reason, map_usage, to_stream_chunks},
};
use serde_json::{Map, json};

fn usage(input: u64, output: u64, cache_read: u64, cache_write: u64) -> PiUsage {
    PiUsage {
        input,
        output,
        cache_read,
        cache_write,
        total_tokens: input + output + cache_read + cache_write,
        cost: PiCost::default(),
    }
}

fn assistant(content: Vec<PiAssistantBlock>, stop_reason: PiStopReason) -> PiAssistantMessage {
    PiAssistantMessage {
        role: PiAssistantRole::Assistant,
        content,
        api: PiApi::new("openai-completions"),
        provider: ProviderId::new("deepseek"),
        model: ModelId::new("deepseek-v4-flash"),
        response_model: None,
        response_id: None,
        usage: usage(0, 0, 0, 0),
        stop_reason,
        error_message: None,
        timestamp: 0,
    }
}

fn text(text: &str) -> PiAssistantBlock {
    PiAssistantBlock::Text {
        text: text.to_owned(),
        text_signature: None,
    }
}

fn tool(id: &str, name: &str) -> PiAssistantBlock {
    PiAssistantBlock::ToolCall {
        id: CallId::new(id),
        name: name.to_owned(),
        arguments: Map::new(),
        thought_signature: None,
    }
}

#[test]
fn maps_usage_and_all_terminal_reasons() {
    assert_eq!(
        serde_json::to_value(map_usage(&usage(10, 5, 8, 2))).unwrap(),
        json!({"inputTokens":10,"outputTokens":5,"cacheReadTokens":8,"cacheWriteTokens":2})
    );
    assert_eq!(
        serde_json::to_value(map_usage(&usage(10, 5, 0, 0))).unwrap(),
        json!({"inputTokens":10,"outputTokens":5})
    );

    for (stop, expected) in [
        (PiStopReason::Stop, "stop"),
        (PiStopReason::Length, "max-tokens"),
        (PiStopReason::ToolUse, "tool-calls"),
        (PiStopReason::Aborted, "aborted"),
    ] {
        assert_eq!(
            map_stop_reason(&assistant(vec![text("ok")], stop), None).kind(),
            expected
        );
    }
    let empty = map_stop_reason(&assistant(vec![], PiStopReason::Stop), None);
    let FinishReason::Error { failure } = empty else {
        panic!("empty stop must be an error")
    };
    assert_eq!(failure.code, "EMPTY_RESPONSE");
    assert_eq!(
        failure.message,
        "model \"deepseek-v4-flash\" returned a completed response with no content"
    );
}

#[test]
fn classifies_provider_errors_and_context_overflow_exactly() {
    let cases = [
        ("HTTP 401: bad key", "AUTH"),
        ("HTTP 429: rate limit", "RATE_LIMIT"),
        ("HTTP 429: insufficient_quota", "QUOTA"),
        ("HTTP 400: invalid_request", "INVALID_REQUEST"),
        ("HTTP 500: backend down", "SERVER"),
        ("provider timed out", "TIMEOUT"),
        ("ECONNRESET socket closed", "TRANSPORT"),
        ("other side closed", "TRANSPORT"),
        ("HTTP2 request did not get a response", "TRANSPORT"),
        ("WebSocket closed unexpectedly", "TRANSPORT"),
        ("terminated", "TRANSPORT"),
        ("Premature close", "TRANSPORT"),
        ("Anthropic stream ended before message_stop", "TRANSPORT"),
        ("Stream ended without finish_reason", "TRANSPORT"),
        ("unclassified provider failure", "PI_AI_ERROR"),
    ];
    for (text, expected) in cases {
        let mut message = assistant(vec![], PiStopReason::Error);
        message.error_message = Some(text.to_owned());
        let FinishReason::Error { failure } = map_stop_reason(&message, None) else {
            panic!("provider error must map to an error finish")
        };
        assert_eq!(failure.code, expected, "{text}");
    }

    for text in [
        "prompt is too long: 213462 tokens > 200000 maximum",
        "HTTP 400: input exceeds the model context window limit",
        "HTTP 400: request too large for model context",
    ] {
        let mut message = assistant(vec![], PiStopReason::Error);
        message.error_message = Some(text.to_owned());
        let FinishReason::Error { failure } = map_stop_reason(&message, None) else {
            panic!("overflow must map to an error finish")
        };
        assert_eq!(failure.code, "CONTEXT_WINDOW_EXCEEDED", "{text}");
    }

    let mut throttled = assistant(vec![], PiStopReason::Error);
    throttled.error_message =
        Some("ThrottlingException: Too many tokens, rate limit reached".to_owned());
    let FinishReason::Error { failure } = map_stop_reason(&throttled, None) else {
        panic!("throttling must be an error finish")
    };
    assert_eq!(failure.code, "RATE_LIMIT");
}

#[test]
fn detects_silent_and_length_stop_overflow_from_resolved_capacity() {
    let mut silent = assistant(vec![text("x")], PiStopReason::Stop);
    silent.usage = usage(101, 0, 0, 0);
    assert!(matches!(map_stop_reason(&silent, None), FinishReason::Stop));
    let FinishReason::Error { failure } = map_stop_reason(&silent, Some(100)) else {
        panic!("silent overflow must be an error")
    };
    assert_eq!(failure.code, "CONTEXT_WINDOW_EXCEEDED");
    assert_eq!(
        failure.message,
        "pi-ai detected context overflow for model \"deepseek-v4-flash\""
    );

    let mut length = assistant(vec![], PiStopReason::Length);
    length.usage = usage(80, 0, 19, 0);
    assert!(matches!(
        map_stop_reason(&length, None),
        FinishReason::MaxTokens
    ));
    assert!(matches!(
        map_stop_reason(&length, Some(100)),
        FinishReason::Error { .. }
    ));
}

#[tokio::test]
async fn translates_every_content_event_then_usage_and_replay_finish() {
    let partial_tool = assistant(vec![tool("call-1", "f")], PiStopReason::Stop);
    let mut done = assistant(
        vec![
            text("hi"),
            PiAssistantBlock::Thinking {
                thinking: "mull".to_owned(),
                thinking_signature: None,
                redacted: None,
            },
            tool("call-1", "f"),
        ],
        PiStopReason::ToolUse,
    );
    done.usage = usage(3, 2, 0, 0);
    let empty = assistant(vec![], PiStopReason::Stop);
    let events = vec![
        PiAssistantEvent::Start {
            partial: empty.clone(),
        },
        PiAssistantEvent::TextStart {
            content_index: 0,
            partial: empty.clone(),
        },
        PiAssistantEvent::TextDelta {
            content_index: 0,
            delta: "hi".to_owned(),
            partial: empty.clone(),
        },
        PiAssistantEvent::TextEnd {
            content_index: 0,
            content: "hi".to_owned(),
            partial: empty.clone(),
        },
        PiAssistantEvent::ThinkingStart {
            content_index: 1,
            partial: empty.clone(),
        },
        PiAssistantEvent::ThinkingDelta {
            content_index: 1,
            delta: "mull".to_owned(),
            partial: empty.clone(),
        },
        PiAssistantEvent::ThinkingEnd {
            content_index: 1,
            content: "mull".to_owned(),
            partial: empty.clone(),
        },
        PiAssistantEvent::ToolCallStart {
            content_index: 0,
            partial: partial_tool.clone(),
        },
        PiAssistantEvent::ToolCallDelta {
            content_index: 0,
            delta: "{\"a\"".to_owned(),
            partial: partial_tool.clone(),
        },
        PiAssistantEvent::ToolCallDelta {
            content_index: 0,
            delta: ":1}".to_owned(),
            partial: partial_tool.clone(),
        },
        PiAssistantEvent::ToolCallEnd {
            content_index: 2,
            tool_call: PiToolCall {
                id: CallId::new("call-1"),
                name: "f".to_owned(),
                arguments: Map::from_iter([
                    ("a".to_owned(), json!(1)),
                    ("large".to_owned(), json!(1e20)),
                ]),
                thought_signature: None,
            },
            partial: partial_tool,
        },
        PiAssistantEvent::Done {
            reason: PiStopReason::ToolUse,
            message: done,
        },
    ];
    let chunks: Vec<StreamChunk> = to_stream_chunks(
        stream::iter(events.into_iter().map(Ok::<_, anyhow::Error>)),
        None,
    )
    .try_collect()
    .await
    .unwrap();
    assert_eq!(
        serde_json::to_value(chunks).unwrap(),
        expected_content_chunks()
    );
}

fn expected_content_chunks() -> serde_json::Value {
    json!([
        {"type":"block-start","index":0,"blockType":"text"},
        {"type":"text-delta","index":0,"text":"hi"},
        {"type":"block-end","index":0,"block":{"type":"text","text":"hi"}},
        {"type":"block-start","index":1,"blockType":"reasoning"},
        {"type":"reasoning-delta","index":1,"text":"mull"},
        {"type":"block-end","index":1,"block":{"type":"reasoning","text":"mull"}},
        {"type":"block-start","index":0,"blockType":"tool-call"},
        {"type":"tool-call-delta","index":0,"id":"call-1","name":"f","argumentsDelta":"{\"a\""},
        {"type":"tool-call-delta","index":0,"id":"call-1","name":"f","argumentsDelta":":1}"},
        {"type":"block-end","index":2,"block":{"type":"tool-call","id":"call-1","name":"f","arguments":"{\"a\":1,\"large\":100000000000000000000}"}},
        {"type":"usage","usage":{"inputTokens":3,"outputTokens":2}},
        {"type":"finish","reason":{"kind":"tool-calls"},"replayState":{"kind":"pi-ai","version":1,"api":"openai-completions","provider":"deepseek","model":"deepseek-v4-flash","stopReason":"toolUse","blocks":[{"type":"text"},{"type":"reasoning"},{"type":"tool-call"}]}}
    ])
}

#[tokio::test]
async fn terminal_error_omits_replay_and_missing_tool_state_is_tolerated() {
    let empty = assistant(vec![], PiStopReason::Stop);
    let mut failed = assistant(vec![], PiStopReason::Error);
    failed.error_message = Some("boom".to_owned());
    failed.usage = usage(1, 0, 0, 0);
    let chunks: Vec<StreamChunk> = to_stream_chunks(
        stream::iter(vec![
            Ok(PiAssistantEvent::ToolCallDelta {
                content_index: 0,
                delta: "{}".to_owned(),
                partial: empty,
            }),
            Ok(PiAssistantEvent::Error {
                reason: PiStopReason::Error,
                error: failed,
            }),
        ]),
        None,
    )
    .try_collect()
    .await
    .unwrap();
    assert_eq!(
        serde_json::to_value(chunks).unwrap(),
        json!([
            {"type":"tool-call-delta","index":0,"id":"","argumentsDelta":"{}"},
            {"type":"usage","usage":{"inputTokens":1,"outputTokens":0}},
            {"type":"finish","reason":{"kind":"error","failure":{"message":"boom","code":"PI_AI_ERROR"}}}
        ])
    );
}

#[derive(Debug)]
struct Marker(Arc<()>);

impl fmt::Display for Marker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SDK transport exploded")
    }
}

impl Error for Marker {}

#[tokio::test]
async fn eof_is_stream_closed_and_source_error_retains_concrete_identity() {
    let eof = to_stream_chunks(
        stream::iter(vec![Ok(PiAssistantEvent::Start {
            partial: assistant(vec![], PiStopReason::Stop),
        })]),
        None,
    )
    .try_collect::<Vec<_>>()
    .await
    .unwrap_err();
    let llm = eof.downcast_ref::<LlmError>().unwrap();
    assert_eq!(llm.code(), "STREAM_CLOSED");

    let identity = Arc::new(());
    let source = anyhow::Error::new(Marker(identity.clone()));
    let error = to_stream_chunks(stream::iter(vec![Err::<PiAssistantEvent, _>(source)]), None)
        .try_collect::<Vec<_>>()
        .await
        .unwrap_err();
    let marker = error.downcast_ref::<Marker>().unwrap();
    assert!(Arc::ptr_eq(&identity, &marker.0));
}
