//! DeepSeek OpenAI-compatible chat-completions wire vocabulary.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Request body for `POST {base_url}/chat/completions`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WireRequest {
    /// Provider model id.
    pub model: String,
    /// Ordered conversation.
    pub messages: Vec<WireMessage>,
    /// This adapter always requests a stream.
    pub stream: bool,
    /// Usage is requested on the stream.
    pub stream_options: WireStreamOptions,
    /// Thinking-mode toggle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<WireThinking>,
    /// Thinking effort accepted by `DeepSeek`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<WireReasoningEffort>,
    /// Callable functions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<WireTool>>,
    /// Sampling temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Output-token ceiling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// Provider stop sequences.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
}

/// Streaming options sent on every request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct WireStreamOptions {
    /// Ask for a usage frame.
    pub include_usage: bool,
}

/// Thinking-mode object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct WireThinking {
    /// Enabled or disabled.
    #[serde(rename = "type")]
    pub kind: ThinkingMode,
}

/// `DeepSeek` thinking mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingMode {
    /// Enable thinking.
    Enabled,
    /// Disable thinking.
    Disabled,
}

/// `DeepSeek` reasoning-effort vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WireReasoningEffort {
    /// High effort.
    High,
    /// Maximum effort.
    Max,
}

/// One chat-completions history entry.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum WireMessage {
    /// System instruction.
    System {
        /// Flattened text.
        content: String,
    },
    /// User input.
    User {
        /// Flattened text.
        content: String,
    },
    /// Assistant history.
    Assistant {
        /// Visible text; always a string for replay compatibility.
        content: String,
        /// Thinking passback on tool-call turns only.
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<String>,
        /// Completed tool calls.
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<WireToolCall>>,
    },
    /// Result for one tool call.
    Tool {
        /// Provider call identity.
        tool_call_id: String,
        /// Flattened result text.
        content: String,
    },
}

/// Completed assistant tool call.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WireToolCall {
    /// Provider call identity.
    pub id: String,
    /// Fixed OpenAI-compatible tag.
    #[serde(rename = "type")]
    pub kind: WireFunctionKind,
    /// Function payload.
    pub function: WireFunctionCall,
}

/// Fixed function tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WireFunctionKind {
    /// Function tool.
    Function,
}

/// Completed function invocation.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WireFunctionCall {
    /// Tool name.
    pub name: String,
    /// Raw JSON arguments.
    pub arguments: String,
}

/// One function declaration.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WireTool {
    /// Fixed OpenAI-compatible tag.
    #[serde(rename = "type")]
    pub kind: WireFunctionKind,
    /// Function schema.
    pub function: WireFunction,
}

/// Function schema fields.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WireFunction {
    /// Tool name.
    pub name: String,
    /// Model-facing description.
    pub description: String,
    /// JSON Schema parameters.
    pub parameters: Map<String, Value>,
}

/// One parsed chat-completion chunk.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct WireChunk {
    /// Streamed choices; requests ask for one but parsing remains permissive.
    #[serde(default)]
    pub choices: Vec<WireChoice>,
    /// Latest usage, when non-null.
    #[serde(default)]
    pub usage: Option<WireUsage>,
}

/// One streamed choice.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct WireChoice {
    /// Incremental fields.
    #[serde(default)]
    pub delta: Option<WireDelta>,
    /// Terminal provider reason.
    #[serde(default)]
    pub finish_reason: Option<String>,
}

/// Incremental choice content.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct WireDelta {
    /// Provider role announcement.
    #[serde(default)]
    pub role: Option<String>,
    /// Visible text; null and absence are both ignored.
    #[serde(default)]
    pub content: Option<String>,
    /// Thinking text; empty, null, and absence do not open a block.
    #[serde(default)]
    pub reasoning_content: Option<String>,
    /// Parallel tool-call fragments.
    #[serde(default)]
    pub tool_calls: Vec<WireToolCallDelta>,
}

/// One streamed tool-call fragment.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct WireToolCallDelta {
    /// Stable wire index.
    pub index: i64,
    /// Provider call identity, normally first-frame only.
    #[serde(default)]
    pub id: Option<String>,
    /// Fixed wire tag, ignored after parsing.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// Function fragment.
    #[serde(default)]
    pub function: Option<WireFunctionDelta>,
}

/// Incremental function fields.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct WireFunctionDelta {
    /// Tool name, normally first-frame only.
    #[serde(default)]
    pub name: Option<String>,
    /// Raw JSON suffix.
    #[serde(default)]
    pub arguments: Option<String>,
}

/// Provider token accounting.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct WireUsage {
    /// Input tokens including cache hits.
    pub prompt_tokens: u64,
    /// Generated tokens.
    pub completion_tokens: u64,
    /// DeepSeek-native cache-hit spelling.
    #[serde(default)]
    pub prompt_cache_hit_tokens: Option<u64>,
    /// DeepSeek-native cache-miss spelling.
    #[serde(default)]
    pub prompt_cache_miss_tokens: Option<u64>,
    /// OpenAI-compatible prompt detail.
    #[serde(default)]
    pub prompt_tokens_details: Option<WirePromptTokenDetails>,
    /// OpenAI-compatible completion detail.
    #[serde(default)]
    pub completion_tokens_details: Option<WireCompletionTokenDetails>,
}

/// Prompt-token details.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct WirePromptTokenDetails {
    /// Cached input tokens.
    #[serde(default)]
    pub cached_tokens: Option<u64>,
}

/// Completion-token details.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct WireCompletionTokenDetails {
    /// Reasoning tokens.
    #[serde(default)]
    pub reasoning_tokens: Option<u64>,
}

/// Non-success response body.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct WireError {
    /// Provider error details.
    #[serde(default)]
    pub error: Option<WireErrorDetail>,
}

/// Provider error detail fields.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct WireErrorDetail {
    /// Human-readable provider message.
    #[serde(default)]
    pub message: Option<String>,
    /// Provider class.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// Provider code.
    #[serde(default)]
    pub code: Option<String>,
}
