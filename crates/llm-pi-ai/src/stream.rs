//! pi-ai assistant-event translation into the Harness streaming protocol.

use std::{collections::HashMap, sync::LazyLock};

use futures::{Stream, StreamExt};
use regex::Regex;
use seekdeep_llm::{
    BoxLlmChunkStream, CONTEXT_WINDOW_EXCEEDED_CODE, CallId, ContentBlock, EMPTY_RESPONSE_CODE,
    FinishReason, LlmError, LlmFailure, QUOTA_EXCEEDED_CODE, StreamChunk, TokenUsage,
    is_context_window_exceeded_error, is_quota_exceeded_error,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::json::stringify_object;
use crate::replay::{
    PiAssistantBlock, PiAssistantMessage, PiStopReason, PiUsage, to_pi_replay_state,
};

/// Completed native pi-ai tool call carried by a terminal block event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiToolCall {
    /// Provider call identity.
    pub id: CallId,
    /// Tool name.
    pub name: String,
    /// Parsed arguments.
    pub arguments: Map<String, Value>,
    /// Optional native reasoning signature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
}

/// Closed pi-ai assistant event union.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PiAssistantEvent {
    /// Stream opened.
    #[serde(rename = "start")]
    Start {
        /// Current native message.
        partial: PiAssistantMessage,
    },
    /// Text block opened.
    #[serde(rename = "text_start")]
    TextStart {
        /// Content index.
        #[serde(rename = "contentIndex")]
        content_index: u64,
        /// Current native message.
        partial: PiAssistantMessage,
    },
    /// Text suffix.
    #[serde(rename = "text_delta")]
    TextDelta {
        /// Content index.
        #[serde(rename = "contentIndex")]
        content_index: u64,
        /// Exact suffix.
        delta: String,
        /// Current native message.
        partial: PiAssistantMessage,
    },
    /// Text block completed.
    #[serde(rename = "text_end")]
    TextEnd {
        /// Content index.
        #[serde(rename = "contentIndex")]
        content_index: u64,
        /// Authoritative text.
        content: String,
        /// Current native message.
        partial: PiAssistantMessage,
    },
    /// Reasoning block opened.
    #[serde(rename = "thinking_start")]
    ThinkingStart {
        /// Content index.
        #[serde(rename = "contentIndex")]
        content_index: u64,
        /// Current native message.
        partial: PiAssistantMessage,
    },
    /// Reasoning suffix.
    #[serde(rename = "thinking_delta")]
    ThinkingDelta {
        /// Content index.
        #[serde(rename = "contentIndex")]
        content_index: u64,
        /// Exact suffix.
        delta: String,
        /// Current native message.
        partial: PiAssistantMessage,
    },
    /// Reasoning block completed.
    #[serde(rename = "thinking_end")]
    ThinkingEnd {
        /// Content index.
        #[serde(rename = "contentIndex")]
        content_index: u64,
        /// Authoritative reasoning.
        content: String,
        /// Current native message.
        partial: PiAssistantMessage,
    },
    /// Tool-call block opened.
    #[serde(rename = "toolcall_start")]
    ToolCallStart {
        /// Content index.
        #[serde(rename = "contentIndex")]
        content_index: u64,
        /// Current native message.
        partial: PiAssistantMessage,
    },
    /// Raw tool-argument suffix.
    #[serde(rename = "toolcall_delta")]
    ToolCallDelta {
        /// Content index.
        #[serde(rename = "contentIndex")]
        content_index: u64,
        /// Exact suffix.
        delta: String,
        /// Current native message.
        partial: PiAssistantMessage,
    },
    /// Tool call completed.
    #[serde(rename = "toolcall_end")]
    ToolCallEnd {
        /// Content index.
        #[serde(rename = "contentIndex")]
        content_index: u64,
        /// Parsed completed call.
        #[serde(rename = "toolCall")]
        tool_call: PiToolCall,
        /// Current native message.
        partial: PiAssistantMessage,
    },
    /// Successful terminal event.
    #[serde(rename = "done")]
    Done {
        /// SDK terminal reason.
        reason: PiStopReason,
        /// Completed message.
        message: PiAssistantMessage,
    },
    /// Failed or aborted terminal event.
    #[serde(rename = "error")]
    Error {
        /// SDK terminal reason.
        reason: PiStopReason,
        /// Failed message.
        error: PiAssistantMessage,
    },
}

/// Maps pi-ai usage, omitting zero cache counters.
#[must_use]
pub fn map_usage(usage: &PiUsage) -> TokenUsage {
    TokenUsage {
        input_tokens: usage.input,
        output_tokens: usage.output,
        cache_read_tokens: (usage.cache_read > 0).then_some(usage.cache_read),
        cache_write_tokens: (usage.cache_write > 0).then_some(usage.cache_write),
        reasoning_tokens: None,
    }
}

/// Maps a terminal pi-ai message to the Harness finish vocabulary.
#[must_use]
pub fn map_stop_reason(message: &PiAssistantMessage, context_window: Option<u64>) -> FinishReason {
    let pi_overflow = is_pi_context_overflow(message, context_window);
    let harness_overflow = message.stop_reason == PiStopReason::Error
        && message
            .error_message
            .as_deref()
            .is_some_and(is_context_window_exceeded_error);
    if pi_overflow || harness_overflow {
        return FinishReason::Error {
            failure: failure(
                message.error_message.clone().unwrap_or_else(|| {
                    format!(
                        "pi-ai detected context overflow for model \"{}\"",
                        message.model.as_str()
                    )
                }),
                CONTEXT_WINDOW_EXCEEDED_CODE,
            ),
        };
    }

    match message.stop_reason {
        PiStopReason::Stop if message.content.is_empty() => FinishReason::Error {
            failure: failure(
                format!(
                    "model \"{}\" returned a completed response with no content",
                    message.model.as_str()
                ),
                EMPTY_RESPONSE_CODE,
            ),
        },
        PiStopReason::Stop => FinishReason::Stop,
        PiStopReason::Length => FinishReason::MaxTokens,
        PiStopReason::ToolUse => FinishReason::ToolCalls,
        PiStopReason::Aborted => FinishReason::Aborted {
            failure: failure(
                message
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "pi-ai stream aborted".to_owned()),
                "ABORTED",
            ),
        },
        PiStopReason::Error => {
            let text = message
                .error_message
                .clone()
                .unwrap_or_else(|| "pi-ai stream error".to_owned());
            let code = classify_pi_ai_error(&text);
            FinishReason::Error {
                failure: failure(text, code),
            }
        }
    }
}

/// Translates a fallible pi-ai event stream into Harness chunks.
///
/// Source iterator failures remain the same concrete error inside
/// `anyhow::Error`; a clean EOF before `done` or `error` becomes
/// `LlmError(STREAM_CLOSED)`.
pub fn to_stream_chunks<S>(events: S, context_window: Option<u64>) -> BoxLlmChunkStream
where
    S: Stream<Item = anyhow::Result<PiAssistantEvent>> + Send + 'static,
{
    Box::pin(async_stream::try_stream! {
        futures::pin_mut!(events);
        let mut tool_ids = HashMap::<u64, (String, String)>::new();
        while let Some(event) = events.next().await {
            match event? {
                PiAssistantEvent::Start { .. } => {}
                PiAssistantEvent::TextStart { content_index, .. } => yield StreamChunk::BlockStart {
                    index: content_index,
                    block_type: "text".to_owned(),
                },
                PiAssistantEvent::TextDelta { content_index, delta, .. } => yield StreamChunk::TextDelta {
                    index: content_index,
                    text: delta,
                },
                PiAssistantEvent::TextEnd { content_index, content, .. } => yield StreamChunk::BlockEnd {
                    index: content_index,
                    block: ContentBlock::Text { text: content },
                },
                PiAssistantEvent::ThinkingStart { content_index, .. } => yield StreamChunk::BlockStart {
                    index: content_index,
                    block_type: "reasoning".to_owned(),
                },
                PiAssistantEvent::ThinkingDelta { content_index, delta, .. } => yield StreamChunk::ReasoningDelta {
                    index: content_index,
                    text: delta,
                },
                PiAssistantEvent::ThinkingEnd { content_index, content, .. } => yield StreamChunk::BlockEnd {
                    index: content_index,
                    block: ContentBlock::Reasoning { text: content },
                },
                PiAssistantEvent::ToolCallStart { content_index, partial } => {
                    let native = usize::try_from(content_index).ok()
                        .and_then(|index| partial.content.get(index));
                    let (id, name) = match native {
                        Some(PiAssistantBlock::ToolCall { id, name, .. }) => {
                            (id.as_str().to_owned(), name.clone())
                        }
                        _ => (String::new(), String::new()),
                    };
                    tool_ids.insert(content_index, (id, name));
                    yield StreamChunk::BlockStart {
                        index: content_index,
                        block_type: "tool-call".to_owned(),
                    };
                }
                PiAssistantEvent::ToolCallDelta { content_index, delta, .. } => {
                    let known = tool_ids.get(&content_index);
                    yield StreamChunk::ToolCallDelta {
                        index: content_index,
                        id: CallId::new(known.map_or("", |(id, _)| id.as_str())),
                        name: known.and_then(|(_, name)| (!name.is_empty()).then(|| name.clone())),
                        arguments_delta: delta,
                    };
                }
                PiAssistantEvent::ToolCallEnd { content_index, tool_call, .. } => yield StreamChunk::BlockEnd {
                    index: content_index,
                    block: ContentBlock::ToolCall {
                        id: tool_call.id,
                        name: tool_call.name,
                        arguments: stringify_object(&tool_call.arguments)?,
                    },
                },
                PiAssistantEvent::Done { message, .. } => {
                    yield StreamChunk::Usage { usage: map_usage(&message.usage) };
                    yield StreamChunk::Finish {
                        reason: map_stop_reason(&message, context_window),
                        replay_state: Some(serde_json::to_value(to_pi_replay_state(&message))?),
                    };
                    return;
                }
                PiAssistantEvent::Error { error, .. } => {
                    yield StreamChunk::Usage { usage: map_usage(&error.usage) };
                    yield StreamChunk::Finish {
                        reason: map_stop_reason(&error, context_window),
                        replay_state: None,
                    };
                    return;
                }
            }
        }
        Err::<(), _>(LlmError::simple(
            "pi-ai event stream ended without done/error",
            "STREAM_CLOSED",
        ))?;
    })
}

fn failure(message: impl Into<String>, code: impl Into<String>) -> LlmFailure {
    LlmFailure {
        message: message.into(),
        code: code.into(),
        status: None,
        provider_retry_after_ms: None,
        request_id: None,
    }
}

fn classify_pi_ai_error(message: &str) -> &'static str {
    if AUTH.is_match(message) {
        return "AUTH";
    }
    if is_quota_exceeded_error(message) {
        return QUOTA_EXCEEDED_CODE;
    }
    if RATE_LIMIT.is_match(message) {
        return "RATE_LIMIT";
    }
    if INVALID_REQUEST.is_match(message) {
        return "INVALID_REQUEST";
    }
    if SERVER.is_match(message) {
        return "SERVER";
    }
    if TIMEOUT.is_match(message) {
        return "TIMEOUT";
    }
    if STREAM_TRUNCATED.is_match(message) || TRANSPORT.is_match(message) {
        return "TRANSPORT";
    }
    "PI_AI_ERROR"
}

fn is_pi_context_overflow(message: &PiAssistantMessage, context_window: Option<u64>) -> bool {
    if message.stop_reason == PiStopReason::Error
        && let Some(text) = message.error_message.as_deref()
        && !NON_OVERFLOW.iter().any(|pattern| pattern.is_match(text))
        && OVERFLOW.iter().any(|pattern| pattern.is_match(text))
    {
        return true;
    }
    let Some(window) = context_window.filter(|window| *window > 0) else {
        return false;
    };
    let input = message.usage.input.saturating_add(message.usage.cache_read);
    if message.stop_reason == PiStopReason::Stop && input > window {
        return true;
    }
    message.stop_reason == PiStopReason::Length
        && message.usage.output == 0
        && u128::from(input) * 100 >= u128::from(window) * 99
}

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("compile-time pi-ai classifier regex is valid")
}

static AUTH: LazyLock<Regex> = LazyLock::new(|| regex(r"\b(?:401|403)\b"));
static RATE_LIMIT: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)\b429\b|rate.?limit"));
static INVALID_REQUEST: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)\b400\b|invalid.?request"));
static SERVER: LazyLock<Regex> = LazyLock::new(|| regex(r"\b5[0-9][0-9]\b"));
static TIMEOUT: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)\btime(?:d)?\s*out\b|timeout"));
static STREAM_TRUNCATED: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)stream ended (?:before|without)\b"));
static TRANSPORT: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)\b(?:network|connection|socket|fetch)\b|\bECONN[A-Z]+\b|\b(?:other side closed|HTTP2 request did not get a response|WebSocket closed unexpectedly)\b|\bterminated\b|premature close",
    )
});

static NON_OVERFLOW: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)^(Throttling error|Service unavailable):",
        r"(?i)rate limit",
        r"(?i)too many requests",
    ]
    .into_iter()
    .map(regex)
    .collect()
});

static OVERFLOW: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)prompt is too long",
        r"(?i)request_too_large",
        r"(?i)input is too long for requested model",
        r"(?i)exceeds the context window",
        r"(?i)exceeds (?:the )?(?:model'?s )?maximum context length(?: of [0-9,]+ tokens?|\s*\([0-9,]+\))",
        r"(?i)input token count.*exceeds the maximum",
        r"(?i)maximum prompt length is [0-9]+",
        r"(?i)reduce the length of the messages",
        r"(?i)maximum context length is [0-9]+ tokens",
        r"(?i)exceeds (?:the )?maximum allowed input length of [0-9,]+ tokens?",
        r"(?i)input \([0-9]+ tokens\) is longer than the model'?s context length \([0-9]+ tokens\)",
        r"(?i)exceeds the limit of [0-9]+",
        r"(?i)exceeds the available context size",
        r"(?i)greater than the context length",
        r"(?i)context window exceeds limit",
        r"(?i)exceeded model token limit",
        r"(?i)too large for model with [0-9]+ maximum context length",
        r"(?i)prompt has [0-9,]+ tokens?, but the configured context size is [0-9,]+ tokens?",
        r"(?i)model_context_window_exceeded",
        r"(?i)prompt too long; exceeded (?:max )?context length",
        r"(?i)range of input length should be",
        r"(?i)context[_ ]length[_ ]exceeded",
        r"(?i)too many tokens",
        r"(?i)token limit exceeded",
        r"(?i)^4(?:00|13)\s*(?:status code)?\s*\(no body\)",
    ]
    .into_iter()
    .map(regex)
    .collect()
});
