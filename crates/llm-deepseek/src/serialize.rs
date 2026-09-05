//! Serialize provider-neutral messages into DeepSeek chat completions.

use seekdeep_llm::{
    ContentBlock, GenerateOptions, LlmError, LlmRequestPurpose, Message, MessageRole,
    content_has_image,
};
use serde::{Deserialize, Serialize};

use crate::types::{
    ThinkingMode, WireFunction, WireFunctionCall, WireFunctionKind, WireMessage,
    WireReasoningEffort, WireRequest, WireStreamOptions, WireThinking, WireTool, WireToolCall,
};

/// Adapter-level request defaults.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct RequestDefaults {
    /// Default thinking policy.
    pub thinking: Option<ThinkingMode>,
    /// Default reasoning effort, including the harness-only `off` value.
    pub reasoning_effort: Option<ReasoningEffort>,
}

/// Adapter-owned reasoning default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    /// Disable thinking.
    Off,
    /// High effort.
    High,
    /// Maximum effort.
    Max,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ResolvedThinking {
    thinking: Option<ThinkingMode>,
    reasoning_effort: Option<WireReasoningEffort>,
}

fn request_reasoning_effort(value: &str) -> Result<ReasoningEffort, LlmError> {
    match value {
        "off" => Ok(ReasoningEffort::Off),
        "high" => Ok(ReasoningEffort::High),
        "max" => Ok(ReasoningEffort::Max),
        _ => Err(LlmError::simple(
            format!("DeepSeek does not support reasoning effort \"{value}\""),
            "UNSUPPORTED_REASONING_EFFORT",
        )),
    }
}

fn resolve_thinking(
    options: &GenerateOptions,
    defaults: RequestDefaults,
) -> Result<ResolvedThinking, LlmError> {
    if options.purpose == Some(LlmRequestPurpose::SessionTitle) {
        return Ok(ResolvedThinking {
            thinking: Some(ThinkingMode::Disabled),
            reasoning_effort: None,
        });
    }
    let effort = options
        .reasoning_effort
        .as_ref()
        .map(|value| request_reasoning_effort(value.as_str()))
        .transpose()?
        .or(defaults.reasoning_effort);
    if defaults.thinking == Some(ThinkingMode::Disabled)
        && effort.is_some_and(|effort| effort != ReasoningEffort::Off)
    {
        let effort = match effort.expect("checked present") {
            ReasoningEffort::Off => "off",
            ReasoningEffort::High => "high",
            ReasoningEffort::Max => "max",
        };
        return Err(LlmError::simple(
            format!("DeepSeek deployment does not support reasoning effort \"{effort}\""),
            "UNSUPPORTED_REASONING_EFFORT",
        ));
    }
    Ok(match effort {
        Some(ReasoningEffort::Off) => ResolvedThinking {
            thinking: Some(ThinkingMode::Disabled),
            reasoning_effort: None,
        },
        Some(ReasoningEffort::High) => ResolvedThinking {
            thinking: Some(ThinkingMode::Enabled),
            reasoning_effort: Some(WireReasoningEffort::High),
        },
        Some(ReasoningEffort::Max) => ResolvedThinking {
            thinking: Some(ThinkingMode::Enabled),
            reasoning_effort: Some(WireReasoningEffort::Max),
        },
        None => ResolvedThinking {
            thinking: defaults.thinking,
            reasoning_effort: None,
        },
    })
}

fn flatten_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            ContentBlock::Reasoning { .. }
            | ContentBlock::Image { .. }
            | ContentBlock::ToolCall { .. }
            | ContentBlock::ToolResult { .. }
            | ContentBlock::Unknown { .. } => None,
        })
        .collect()
}

fn assert_text_only(blocks: &[ContentBlock]) -> Result<(), LlmError> {
    if content_has_image(blocks) {
        return Err(LlmError::simple(
            "The DeepSeek chat-completions adapter does not support image content.",
            "UNSUPPORTED_CONTENT",
        ));
    }
    Ok(())
}

fn serialize_assistant(message: &Message) -> WireMessage {
    let content = flatten_text(message.content());
    let reasoning = message
        .content()
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Reasoning { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    let tool_calls = message
        .content()
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => Some(WireToolCall {
                id: id.as_str().to_owned(),
                kind: WireFunctionKind::Function,
                function: WireFunctionCall {
                    name: name.clone(),
                    arguments: arguments.clone(),
                },
            }),
            _ => None,
        })
        .collect::<Vec<_>>();
    let has_tool_calls = !tool_calls.is_empty();
    WireMessage::Assistant {
        content,
        reasoning_content: (has_tool_calls && !reasoning.is_empty()).then_some(reasoning),
        tool_calls: has_tool_calls.then_some(tool_calls),
    }
}

/// Serializes the ordered conversation, expanding tool results into tool-role messages.
///
/// # Errors
///
/// Returns `UNSUPPORTED_CONTENT` when any nested core image block is present.
pub fn serialize_messages(messages: &[Message]) -> Result<Vec<WireMessage>, LlmError> {
    let mut wire = Vec::new();
    for message in messages {
        assert_text_only(message.content())?;
        match message.role() {
            MessageRole::System => wire.push(WireMessage::System {
                content: flatten_text(message.content()),
            }),
            MessageRole::Assistant => wire.push(serialize_assistant(message)),
            MessageRole::User => {
                let tool_results = message
                    .content()
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::ToolResult {
                            tool_call_id,
                            content,
                            ..
                        } => Some((tool_call_id, content)),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let content = flatten_text(message.content());
                if !content.is_empty() || tool_results.is_empty() {
                    wire.push(WireMessage::User { content });
                }
                for (tool_call_id, content) in tool_results {
                    let content = flatten_text(content);
                    wire.push(WireMessage::Tool {
                        tool_call_id: tool_call_id.as_str().to_owned(),
                        content: if content.is_empty() {
                            "(no output)".to_owned()
                        } else {
                            content
                        },
                    });
                }
            }
        }
    }
    Ok(wire)
}

/// Builds one complete streaming `DeepSeek` request.
///
/// # Errors
///
/// Returns a stable LLM error for image content or an unsupported reasoning effort.
pub fn serialize_request(
    options: &GenerateOptions,
    defaults: RequestDefaults,
) -> Result<WireRequest, LlmError> {
    let mut messages = Vec::new();
    if let Some(system) = &options.system {
        messages.push(WireMessage::System {
            content: system.clone(),
        });
    }
    messages.extend(serialize_messages(&options.messages)?);
    let tools = options.tools.as_ref().and_then(|tools| {
        (!tools.is_empty()).then(|| {
            tools
                .iter()
                .map(|tool| WireTool {
                    kind: WireFunctionKind::Function,
                    function: WireFunction {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        parameters: tool.parameters.clone(),
                    },
                })
                .collect()
        })
    });
    let thinking = resolve_thinking(options, defaults)?;
    Ok(WireRequest {
        model: options.model.to_string(),
        messages,
        stream: true,
        stream_options: WireStreamOptions {
            include_usage: true,
        },
        thinking: thinking.thinking.map(|kind| WireThinking { kind }),
        reasoning_effort: thinking.reasoning_effort,
        tools,
        temperature: options.temperature,
        max_tokens: options.max_tokens,
        stop: options.stop.clone(),
    })
}
