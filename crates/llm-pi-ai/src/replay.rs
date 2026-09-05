//! Durable pi-ai replay metadata and assistant-history reconstruction.

use seekdeep_llm::{CallId, ContentBlock, LlmError, Message, ModelId, ProviderId};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

seekdeep_util::string_brand!(
    /// Extensible pi-ai API transport identity.
    pub struct PiApi;
);
seekdeep_util::string_brand!(
    /// Provider-issued response identity retained for native replay.
    pub struct PiResponseId;
);

/// Native pi-ai terminal reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PiStopReason {
    /// Natural stop.
    #[serde(rename = "stop")]
    Stop,
    /// Token limit.
    #[serde(rename = "length")]
    Length,
    /// Tool invocation.
    #[serde(rename = "toolUse")]
    ToolUse,
    /// Provider error.
    #[serde(rename = "error")]
    Error,
    /// Caller cancellation.
    #[serde(rename = "aborted")]
    Aborted,
}

/// Closed native role for a pi-ai assistant history item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PiAssistantRole {
    /// Model output.
    #[serde(rename = "assistant")]
    Assistant,
}

/// Closed discriminator for adapter-owned replay state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PiReplayKind {
    /// pi-ai adapter state.
    #[serde(rename = "pi-ai")]
    PiAi,
}

/// Zeroed historical cost accounting required by pi-ai messages.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiCost {
    /// Input cost.
    pub input: f64,
    /// Output cost.
    pub output: f64,
    /// Cache-read cost.
    pub cache_read: f64,
    /// Cache-write cost.
    pub cache_write: f64,
    /// Total cost.
    pub total: f64,
}

/// Native pi-ai token and cost accounting.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiUsage {
    /// Input tokens.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Cache-read tokens.
    pub cache_read: u64,
    /// Cache-write tokens.
    pub cache_write: u64,
    /// Total tokens.
    pub total_tokens: u64,
    /// Provider cost accounting.
    pub cost: PiCost,
}

/// One native pi-ai assistant block.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PiAssistantBlock {
    /// Visible text.
    #[serde(rename = "text")]
    Text {
        /// Exact text.
        text: String,
        /// Provider replay signature.
        #[serde(rename = "textSignature", skip_serializing_if = "Option::is_none")]
        text_signature: Option<String>,
    },
    /// Model reasoning.
    #[serde(rename = "thinking")]
    Thinking {
        /// Exact reasoning.
        thinking: String,
        /// Provider replay signature.
        #[serde(rename = "thinkingSignature", skip_serializing_if = "Option::is_none")]
        thinking_signature: Option<String>,
        /// Provider redaction marker.
        #[serde(skip_serializing_if = "Option::is_none")]
        redacted: Option<bool>,
    },
    /// Tool invocation.
    #[serde(rename = "toolCall")]
    ToolCall {
        /// Provider call identity.
        id: CallId,
        /// Tool name.
        name: String,
        /// Parsed object arguments.
        arguments: Map<String, Value>,
        /// Provider reasoning signature.
        #[serde(rename = "thoughtSignature", skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
}

/// Reconstructed native pi-ai assistant history item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiAssistantMessage {
    /// Native role discriminator.
    pub role: PiAssistantRole,
    /// Native blocks.
    pub content: Vec<PiAssistantBlock>,
    /// API transport identity.
    pub api: PiApi,
    /// Provider identity.
    pub provider: ProviderId,
    /// Model identity.
    pub model: ModelId,
    /// Actual response model when supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_model: Option<ModelId>,
    /// Provider response identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_id: Option<PiResponseId>,
    /// Historical usage, always zero for reconstructed messages.
    pub usage: PiUsage,
    /// Terminal reason.
    pub stop_reason: PiStopReason,
    /// Provider error text on failed or aborted messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Native timestamp, always zero for reconstructed history.
    pub timestamp: u64,
}

/// Minimal provider-native metadata stored beside durable Harness content.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiAiReplayState {
    /// Adapter state discriminator.
    pub kind: PiReplayKind,
    /// Schema version.
    pub version: u8,
    /// API transport identity.
    pub api: PiApi,
    /// Provider identity.
    pub provider: ProviderId,
    /// Model identity.
    pub model: ModelId,
    /// Actual response model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_model: Option<ModelId>,
    /// Provider response identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_id: Option<PiResponseId>,
    /// Terminal reason.
    pub stop_reason: PiStopReason,
    /// Per-content-block native metadata.
    pub blocks: Vec<PiAiReplayBlock>,
}

/// Replay metadata corresponding to one durable content block.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PiAiReplayBlock {
    /// Visible text metadata.
    #[serde(rename = "text")]
    Text {
        /// Provider signature.
        #[serde(rename = "textSignature", skip_serializing_if = "Option::is_none")]
        text_signature: Option<String>,
    },
    /// Reasoning metadata.
    #[serde(rename = "reasoning")]
    Reasoning {
        /// Provider signature.
        #[serde(rename = "thinkingSignature", skip_serializing_if = "Option::is_none")]
        thinking_signature: Option<String>,
        /// Provider redaction marker.
        #[serde(skip_serializing_if = "Option::is_none")]
        redacted: Option<bool>,
    },
    /// Tool-call metadata.
    #[serde(rename = "tool-call")]
    ToolCall {
        /// Provider reasoning signature.
        #[serde(rename = "thoughtSignature", skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
}

/// Projects a completed native assistant response into durable replay state.
#[must_use]
pub fn to_pi_replay_state(message: &PiAssistantMessage) -> PiAiReplayState {
    let blocks = message
        .content
        .iter()
        .map(|block| match block {
            PiAssistantBlock::Text { text_signature, .. } => PiAiReplayBlock::Text {
                text_signature: text_signature.clone(),
            },
            PiAssistantBlock::Thinking {
                thinking_signature,
                redacted,
                ..
            } => PiAiReplayBlock::Reasoning {
                thinking_signature: thinking_signature.clone(),
                redacted: *redacted,
            },
            PiAssistantBlock::ToolCall {
                thought_signature, ..
            } => PiAiReplayBlock::ToolCall {
                thought_signature: thought_signature.clone(),
            },
        })
        .collect();
    PiAiReplayState {
        kind: PiReplayKind::PiAi,
        version: 1,
        api: message.api.clone(),
        provider: message.provider.clone(),
        model: message.model.clone(),
        response_model: message.response_model.clone(),
        response_id: message.response_id.clone(),
        stop_reason: message.stop_reason,
        blocks,
    }
}

/// Reconstructs one durable Harness assistant message as native pi-ai history.
///
/// # Errors
///
/// Returns a stable [`LlmError`] for invalid replay metadata or unsupported
/// structured assistant images.
pub fn to_pi_assistant(message: &Message) -> Result<PiAssistantMessage, LlmError> {
    let source = message.source();
    if source.kind != "model" {
        return foreign_assistant(message);
    }
    let Some(replay) = source.fields.get("replayState") else {
        return foreign_assistant(message);
    };
    replayed_assistant(message, replay)
}

fn foreign_assistant(message: &Message) -> Result<PiAssistantMessage, LlmError> {
    let mut content = Vec::new();
    for block in message.content() {
        match block {
            ContentBlock::Text { text } => content.push(PiAssistantBlock::Text {
                text: text.clone(),
                text_signature: None,
            }),
            ContentBlock::Reasoning { text } => content.push(PiAssistantBlock::Thinking {
                thinking: text.clone(),
                thinking_signature: None,
                redacted: None,
            }),
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => content.push(PiAssistantBlock::ToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments: parse_arguments(arguments),
                thought_signature: None,
            }),
            ContentBlock::Image { .. } => {
                return Err(LlmError::simple(
                    "pi-ai chat history cannot represent structured assistant image output",
                    "UNSUPPORTED_CONTENT",
                ));
            }
            ContentBlock::ToolResult { .. } | ContentBlock::Unknown { .. } => {}
        }
    }
    let provider =
        model_source_string(message, "provider").unwrap_or_else(|| "seekdeep-foreign".to_owned());
    let model =
        model_source_string(message, "model").unwrap_or_else(|| "seekdeep-foreign".to_owned());
    let stop_reason = if content
        .iter()
        .any(|block| matches!(block, PiAssistantBlock::ToolCall { .. }))
    {
        PiStopReason::ToolUse
    } else {
        PiStopReason::Stop
    };
    Ok(PiAssistantMessage {
        role: PiAssistantRole::Assistant,
        content,
        api: PiApi::new("seekdeep-foreign"),
        provider: ProviderId::new(provider),
        model: ModelId::new(model),
        response_model: None,
        response_id: None,
        usage: PiUsage::default(),
        stop_reason,
        error_message: None,
        timestamp: 0,
    })
}

fn replayed_assistant(
    message: &Message,
    raw_state: &Value,
) -> Result<PiAssistantMessage, LlmError> {
    let state = read_replay_state(raw_state)?;
    let source_provider = model_source_string(message, "provider").unwrap_or_default();
    let source_model = model_source_string(message, "model").unwrap_or_default();
    if state.provider.as_str() != source_provider {
        return Err(invalid_replay("provider does not match assistant source"));
    }
    if state.model.as_str() != source_model {
        return Err(invalid_replay("model does not match assistant source"));
    }
    if state.blocks.len() != message.content().len() {
        return Err(invalid_replay(
            "block count does not match assistant content",
        ));
    }
    let mut content = Vec::with_capacity(message.content().len());
    for (index, (block, replay)) in message.content().iter().zip(&state.blocks).enumerate() {
        let native = match (block, replay) {
            (ContentBlock::Text { text }, PiAiReplayBlock::Text { text_signature }) => {
                PiAssistantBlock::Text {
                    text: text.clone(),
                    text_signature: text_signature.clone(),
                }
            }
            (
                ContentBlock::Reasoning { text },
                PiAiReplayBlock::Reasoning {
                    thinking_signature,
                    redacted,
                },
            ) => PiAssistantBlock::Thinking {
                thinking: text.clone(),
                thinking_signature: thinking_signature.clone(),
                redacted: *redacted,
            },
            (
                ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                },
                PiAiReplayBlock::ToolCall { thought_signature },
            ) => PiAssistantBlock::ToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments: parse_arguments(arguments),
                thought_signature: thought_signature.clone(),
            },
            _ => {
                return Err(invalid_replay(format!(
                    "block {index} does not match assistant content"
                )));
            }
        };
        content.push(native);
    }
    Ok(PiAssistantMessage {
        role: PiAssistantRole::Assistant,
        content,
        api: state.api,
        provider: state.provider,
        model: state.model,
        response_model: state.response_model,
        response_id: state.response_id,
        usage: PiUsage::default(),
        stop_reason: state.stop_reason,
        error_message: None,
        timestamp: 0,
    })
}

fn read_replay_state(value: &Value) -> Result<PiAiReplayState, LlmError> {
    let Some(state) = value.as_object() else {
        return Err(invalid_replay("expected an object"));
    };
    if state.get("kind").and_then(Value::as_str) != Some("pi-ai") {
        return Err(invalid_replay("unknown state kind"));
    }
    if state.get("version").and_then(Value::as_u64) != Some(1) {
        return Err(invalid_replay(format!(
            "unsupported version {}",
            js_string(state.get("version"))
        )));
    }
    let api = required_string(state, "api")?;
    let provider = required_string(state, "provider")?;
    let model = required_string(state, "model")?;
    let stop_reason = match state.get("stopReason").and_then(Value::as_str) {
        Some("stop") => PiStopReason::Stop,
        Some("length") => PiStopReason::Length,
        Some("toolUse") => PiStopReason::ToolUse,
        Some("error") => PiStopReason::Error,
        Some("aborted") => PiStopReason::Aborted,
        _ => return Err(invalid_replay("unknown stopReason")),
    };
    let response_model = optional_string(state, "responseModel")?.map(ModelId::new);
    let response_id = optional_string(state, "responseId")?.map(PiResponseId::new);
    let Some(raw_blocks) = state.get("blocks").and_then(Value::as_array) else {
        return Err(invalid_replay("blocks must be an array"));
    };
    let mut blocks = Vec::with_capacity(raw_blocks.len());
    for (index, value) in raw_blocks.iter().enumerate() {
        let Some(block) = value.as_object() else {
            return Err(invalid_replay(format!("block {index} must be an object")));
        };
        let block_type = block.get("type").and_then(Value::as_str);
        if !matches!(block_type, Some("text" | "reasoning" | "tool-call")) {
            return Err(invalid_replay(format!("block {index} has an unknown type")));
        }
        for signature in ["textSignature", "thinkingSignature", "thoughtSignature"] {
            if block.get(signature).is_some_and(|value| !value.is_string()) {
                return Err(invalid_replay(format!(
                    "block {index} {signature} must be a string"
                )));
            }
        }
        if block
            .get("redacted")
            .is_some_and(|value| !value.is_boolean())
        {
            return Err(invalid_replay(format!(
                "block {index} redacted must be boolean"
            )));
        }
        let replay = match block_type {
            Some("text") => PiAiReplayBlock::Text {
                text_signature: block
                    .get("textSignature")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            },
            Some("reasoning") => PiAiReplayBlock::Reasoning {
                thinking_signature: block
                    .get("thinkingSignature")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                redacted: block.get("redacted").and_then(Value::as_bool),
            },
            Some("tool-call") => PiAiReplayBlock::ToolCall {
                thought_signature: block
                    .get("thoughtSignature")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            },
            _ => unreachable!("block type validated above"),
        };
        blocks.push(replay);
    }
    Ok(PiAiReplayState {
        kind: PiReplayKind::PiAi,
        version: 1,
        api: PiApi::new(api),
        provider: ProviderId::new(provider),
        model: ModelId::new(model),
        response_model,
        response_id,
        stop_reason,
        blocks,
    })
}

fn required_string(state: &Map<String, Value>, key: &str) -> Result<String, LlmError> {
    match state.get(key).and_then(Value::as_str) {
        Some(value) if !value.is_empty() => Ok(value.to_owned()),
        _ => Err(invalid_replay(format!("{key} must be a non-empty string"))),
    }
}

fn optional_string(state: &Map<String, Value>, key: &str) -> Result<Option<String>, LlmError> {
    match state.get(key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(invalid_replay(format!("{key} must be a string"))),
    }
}

fn model_source_string(message: &Message, key: &str) -> Option<String> {
    (message.source().kind == "model").then(|| {
        message
            .source()
            .fields
            .get(key)?
            .as_str()
            .map(str::to_owned)
    })?
}

fn parse_arguments(raw: &str) -> Map<String, Value> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

fn invalid_replay(message: impl AsRef<str>) -> LlmError {
    LlmError::simple(
        format!("invalid pi-ai replay state: {}", message.as_ref()),
        "INVALID_REPLAY_STATE",
    )
}

fn js_string(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_owned(),
        Some(Value::Null) => "null".to_owned(),
        Some(Value::Bool(value)) => value.to_string(),
        Some(Value::Number(value)) => value.to_string(),
        Some(Value::String(value)) => value.clone(),
        Some(Value::Array(_)) => String::new(),
        Some(Value::Object(_)) => "[object Object]".to_owned(),
    }
}
