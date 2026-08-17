//! Translate DeepSeek SSE payloads into the provider-neutral stream protocol.

use std::{
    collections::{HashMap, hash_map::Entry},
    pin::Pin,
};

use futures::{Stream, StreamExt};
use seekdeep_llm::{
    CallId, ContentBlock, EMPTY_RESPONSE_CODE, FinishReason, LlmError, LlmFailure, StreamChunk,
    TokenUsage,
};

use crate::{
    sse::{DONE, PayloadStream},
    types::{WireChunk, WireUsage},
};

/// Translated provider stream.
pub type TranslatedStream =
    Pin<Box<dyn Stream<Item = anyhow::Result<StreamChunk>> + Send + 'static>>;

#[derive(Clone, Debug)]
enum OpenBlock {
    Text {
        index: u64,
        text: String,
    },
    Reasoning {
        index: u64,
        text: String,
    },
    ToolCall {
        index: u64,
        text: String,
        call_id: Option<String>,
        name: Option<String>,
    },
}

impl OpenBlock {
    fn index(&self) -> u64 {
        match self {
            Self::Text { index, .. }
            | Self::Reasoning { index, .. }
            | Self::ToolCall { index, .. } => *index,
        }
    }

    fn close(&self) -> ContentBlock {
        match self {
            Self::Text { text, .. } => ContentBlock::Text { text: text.clone() },
            Self::Reasoning { text, .. } => ContentBlock::Reasoning { text: text.clone() },
            Self::ToolCall {
                text,
                call_id,
                name,
                ..
            } => ContentBlock::ToolCall {
                id: CallId::new(call_id.clone().unwrap_or_default()),
                name: name.clone().unwrap_or_default(),
                arguments: text.clone(),
            },
        }
    }
}

/// Maps one provider finish reason, preserving unknown values as error codes.
#[must_use]
pub fn map_finish_reason(reason: &str) -> FinishReason {
    match reason {
        "stop" => FinishReason::Stop,
        "tool_calls" => FinishReason::ToolCalls,
        "length" => FinishReason::MaxTokens,
        _ => FinishReason::Error {
            failure: failure(format!("model stopped: {reason}"), reason.to_uppercase()),
        },
    }
}

/// Converts inclusive wire prompt usage to disjoint harness counts.
///
/// # Errors
///
/// Rejects an impossible cache-hit count larger than total prompt tokens.
pub fn map_usage(usage: &WireUsage) -> anyhow::Result<TokenUsage> {
    let cache_read = usage
        .prompt_tokens_details
        .as_ref()
        .and_then(|details| details.cached_tokens)
        .or(usage.prompt_cache_hit_tokens);
    let input_tokens = usage
        .prompt_tokens
        .checked_sub(cache_read.unwrap_or(0))
        .ok_or_else(|| {
            LlmError::simple(
                "DeepSeek usage cache hits exceed prompt tokens",
                "MALFORMED_RESPONSE",
            )
        })?;
    Ok(TokenUsage {
        input_tokens,
        output_tokens: usage.completion_tokens,
        cache_read_tokens: cache_read,
        cache_write_tokens: None,
        reasoning_tokens: usage
            .completion_tokens_details
            .as_ref()
            .and_then(|details| details.reasoning_tokens),
    })
}

/// Translates payloads, deferring block ends, usage, and finish until `[DONE]`.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn translate(mut payloads: PayloadStream) -> TranslatedStream {
    Box::pin(async_stream::try_stream! {
        let mut blocks = Vec::<OpenBlock>::new();
        let mut text_block = None::<usize>;
        let mut reasoning_block = None::<usize>;
        let mut tool_blocks = HashMap::<i64, usize>::new();
        let mut pending_finish = None::<FinishReason>;
        let mut pending_usage = None::<TokenUsage>;

        while let Some(payload) = payloads.next().await {
            let payload = payload?;
            if payload == DONE {
                for block in &blocks {
                    yield StreamChunk::BlockEnd {
                        index: block.index(),
                        block: block.close(),
                    };
                }
                if let Some(usage) = pending_usage {
                    yield StreamChunk::Usage { usage };
                }
                let mut reason = pending_finish.unwrap_or(FinishReason::Stop);
                if reason == FinishReason::Stop && blocks.is_empty() {
                    reason = FinishReason::Error {
                        failure: failure(
                            "model returned a completed response with no content",
                            EMPTY_RESPONSE_CODE,
                        ),
                    };
                }
                yield StreamChunk::Finish {
                    reason,
                    replay_state: None,
                };
                return;
            }

            let chunk = serde_json::from_str::<WireChunk>(&payload).map_err(|_| {
                LlmError::simple(
                    format!("malformed SSE payload: {}", payload.chars().take(120).collect::<String>()),
                    "MALFORMED_RESPONSE",
                )
            })?;
            for choice in chunk.choices {
                if let Some(delta) = choice.delta {
                    if let Some(reasoning) = delta.reasoning_content.filter(|value| !value.is_empty()) {
                        let position = *reasoning_block.get_or_insert_with(|| {
                            let position = blocks.len();
                            blocks.push(OpenBlock::Reasoning {
                                index: block_index(position),
                                text: String::new(),
                            });
                            position
                        });
                        let index = blocks[position].index();
                        if let OpenBlock::Reasoning { text, .. } = &mut blocks[position] {
                            if text.is_empty() {
                                yield StreamChunk::BlockStart {
                                    index,
                                    block_type: "reasoning".to_owned(),
                                };
                            }
                            text.push_str(&reasoning);
                        }
                        yield StreamChunk::ReasoningDelta { index, text: reasoning };
                    }
                    if let Some(content) = delta.content.filter(|value| !value.is_empty()) {
                        let position = *text_block.get_or_insert_with(|| {
                            let position = blocks.len();
                            blocks.push(OpenBlock::Text {
                                index: block_index(position),
                                text: String::new(),
                            });
                            position
                        });
                        let index = blocks[position].index();
                        if let OpenBlock::Text { text, .. } = &mut blocks[position] {
                            if text.is_empty() {
                                yield StreamChunk::BlockStart {
                                    index,
                                    block_type: "text".to_owned(),
                                };
                            }
                            text.push_str(&content);
                        }
                        yield StreamChunk::TextDelta { index, text: content };
                    }
                    for call in delta.tool_calls {
                        let (position, newly_opened) = match tool_blocks.entry(call.index) {
                            Entry::Occupied(entry) => (*entry.get(), false),
                            Entry::Vacant(entry) => {
                                let position = blocks.len();
                                blocks.push(OpenBlock::ToolCall {
                                    index: block_index(position),
                                    text: String::new(),
                                    call_id: None,
                                    name: None,
                                });
                                entry.insert(position);
                                (position, true)
                            }
                        };
                        let index = blocks[position].index();
                        if newly_opened {
                            yield StreamChunk::BlockStart {
                                index,
                                block_type: "tool-call".to_owned(),
                            };
                        }
                        let fragment = call
                            .function
                            .as_ref()
                            .and_then(|function| function.arguments.clone())
                            .unwrap_or_default();
                        let (id, name) = if let OpenBlock::ToolCall { text, call_id, name, .. } = &mut blocks[position] {
                            if let Some(value) = call.id {
                                *call_id = Some(value);
                            }
                            if let Some(value) = call.function.and_then(|function| function.name) {
                                *name = Some(value);
                            }
                            text.push_str(&fragment);
                            (CallId::new(call_id.clone().unwrap_or_default()), name.clone())
                        } else {
                            unreachable!("tool map points only at tool blocks")
                        };
                        yield StreamChunk::ToolCallDelta {
                            index,
                            id,
                            name,
                            arguments_delta: fragment,
                        };
                    }
                }
                if let Some(reason) = choice.finish_reason {
                    pending_finish = Some(map_finish_reason(&reason));
                }
            }
            if let Some(usage) = chunk.usage {
                pending_usage = Some(map_usage(&usage)?);
            }
        }
        Err::<(), anyhow::Error>(LlmError::simple(
            "SSE payload stream ended without [DONE]",
            "STREAM_CLOSED",
        ).into())?;
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

fn block_index(position: usize) -> u64 {
    u64::try_from(position).unwrap_or(u64::MAX)
}
