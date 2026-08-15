//! Provider-neutral content, stream, and request vocabulary.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use serde_json::{Map, Value};

use crate::{
    brand::{CallId, ReasoningEffortId},
    error::LlmFailure,
    message::Message,
};

/// One provider-neutral model content block.
#[derive(Clone, Debug, PartialEq)]
pub enum ContentBlock {
    /// Plain user-visible text.
    Text {
        /// Exact text.
        text: String,
    },
    /// Model reasoning text.
    Reasoning {
        /// Exact reasoning text.
        text: String,
    },
    /// Durable raster attachment reference.
    Image {
        /// Attachment-service owned reference fields.
        attachment: Value,
    },
    /// Model-requested tool invocation.
    ToolCall {
        /// Provider-issued call identity.
        id: CallId,
        /// Registered tool name.
        name: String,
        /// Raw provider-produced JSON argument string.
        arguments: String,
    },
    /// One tool result returned to the model.
    ToolResult {
        /// Correlated call identity.
        tool_call_id: CallId,
        /// Nested model-facing result blocks.
        content: Vec<ContentBlock>,
        /// Whether execution failed.
        is_error: Option<bool>,
    },
    /// Plugin-added block retained without interpretation.
    Unknown {
        /// Merge-extensible type tag.
        block_type: String,
        /// Every remaining field.
        fields: Map<String, Value>,
    },
}

impl ContentBlock {
    /// Returns the merge-extensible wire tag.
    #[must_use]
    pub fn block_type(&self) -> &str {
        match self {
            Self::Text { .. } => "text",
            Self::Reasoning { .. } => "reasoning",
            Self::Image { .. } => "image",
            Self::ToolCall { .. } => "tool-call",
            Self::ToolResult { .. } => "tool-result",
            Self::Unknown { block_type, .. } => block_type,
        }
    }
}

impl Serialize for ContentBlock {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut object = Map::new();
        object.insert(
            "type".to_owned(),
            Value::String(self.block_type().to_owned()),
        );
        match self {
            Self::Text { text } | Self::Reasoning { text } => {
                object.insert("text".to_owned(), Value::String(text.clone()));
            }
            Self::Image { attachment } => {
                object.insert("attachment".to_owned(), attachment.clone());
            }
            Self::ToolCall {
                id,
                name,
                arguments,
            } => {
                object.insert("id".to_owned(), Value::String(id.as_str().to_owned()));
                object.insert("name".to_owned(), Value::String(name.clone()));
                object.insert("arguments".to_owned(), Value::String(arguments.clone()));
            }
            Self::ToolResult {
                tool_call_id,
                content,
                is_error,
            } => {
                object.insert(
                    "toolCallId".to_owned(),
                    Value::String(tool_call_id.as_str().to_owned()),
                );
                object.insert(
                    "content".to_owned(),
                    serde_json::to_value(content).map_err(serde::ser::Error::custom)?,
                );
                if let Some(is_error) = is_error {
                    object.insert("isError".to_owned(), Value::Bool(*is_error));
                }
            }
            Self::Unknown { fields, .. } => object.extend(fields.clone()),
        }
        Value::Object(object).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ContentBlock {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let Value::Object(mut object) = Value::deserialize(deserializer)? else {
            return Err(D::Error::custom("content block must be an object"));
        };
        let block_type = take_string::<D::Error>(&mut object, "type")?;
        match block_type.as_str() {
            "text" => Ok(Self::Text {
                text: take_string::<D::Error>(&mut object, "text")?,
            }),
            "reasoning" => Ok(Self::Reasoning {
                text: take_string::<D::Error>(&mut object, "text")?,
            }),
            "image" => Ok(Self::Image {
                attachment: object
                    .remove("attachment")
                    .ok_or_else(|| D::Error::missing_field("attachment"))?,
            }),
            "tool-call" => Ok(Self::ToolCall {
                id: CallId::new(take_string::<D::Error>(&mut object, "id")?),
                name: take_string::<D::Error>(&mut object, "name")?,
                arguments: take_string::<D::Error>(&mut object, "arguments")?,
            }),
            "tool-result" => Ok(Self::ToolResult {
                tool_call_id: CallId::new(take_string::<D::Error>(&mut object, "toolCallId")?),
                content: serde_json::from_value(
                    object
                        .remove("content")
                        .ok_or_else(|| D::Error::missing_field("content"))?,
                )
                .map_err(D::Error::custom)?,
                is_error: object
                    .remove("isError")
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(D::Error::custom)?,
            }),
            _ => Ok(Self::Unknown {
                block_type,
                fields: object,
            }),
        }
    }
}

fn take_string<E: serde::de::Error>(
    object: &mut Map<String, Value>,
    field: &'static str,
) -> Result<String, E> {
    object
        .remove(field)
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| E::missing_field(field))
}

/// Why a provider response stopped.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum FinishReason {
    /// Natural provider stop.
    Stop,
    /// Provider requested tool execution.
    ToolCalls,
    /// Output-token ceiling reached.
    MaxTokens,
    /// Caller cancellation.
    Aborted {
        /// Provider-neutral cancellation facts.
        failure: LlmFailure,
    },
    /// Provider or transport failure.
    Error {
        /// Provider-neutral failure facts.
        failure: LlmFailure,
    },
}

/// Disjoint token accounting for one call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TokenUsage {
    /// Uncached input tokens.
    pub input_tokens: u64,
    /// Generated tokens.
    pub output_tokens: u64,
    /// Cache-hit input tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    /// Cache-populated input tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    /// Provider-reported reasoning tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
}

/// Raw adapter streaming protocol.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum StreamChunk {
    /// Declares an indexed block.
    BlockStart {
        /// Correlation index.
        index: u64,
        /// Merge-extensible block type.
        #[serde(rename = "blockType")]
        block_type: String,
    },
    /// Visible text delta.
    TextDelta {
        /// Correlation index.
        index: u64,
        /// Delta text.
        text: String,
    },
    /// Reasoning delta.
    ReasoningDelta {
        /// Correlation index.
        index: u64,
        /// Delta text.
        text: String,
    },
    /// Tool call delta.
    ToolCallDelta {
        /// Correlation index.
        index: u64,
        /// Provider call identity.
        id: CallId,
        /// Tool name when supplied on this frame.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// Raw arguments suffix.
        #[serde(rename = "argumentsDelta")]
        arguments_delta: String,
    },
    /// Authoritative completed block.
    BlockEnd {
        /// Correlation index.
        index: u64,
        /// Completed block.
        block: ContentBlock,
    },
    /// Token accounting.
    Usage {
        /// Complete accounting snapshot.
        usage: TokenUsage,
    },
    /// Terminal reason and optional replay state.
    Finish {
        /// Why generation stopped.
        reason: FinishReason,
        /// Adapter-private lossless JSON.
        #[serde(
            rename = "replayState",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        replay_state: Option<Value>,
    },
}

/// JSON schema sent to the model for one tool.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSchema {
    /// Tool name.
    pub name: String,
    /// Model-facing purpose.
    pub description: String,
    /// JSON Schema arguments object.
    pub parameters: Map<String, Value>,
}

/// Display metadata for one registered provider route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmProviderInfo {
    /// Provider route key.
    pub id: String,
    /// Human-readable provider name.
    pub name: String,
}

/// Request modality declared by an exact model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelModality(pub String);

/// Authentication setup a configurable provider presents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LlmProviderAuthentication {
    /// Durable API key.
    ApiKey,
    /// Provider-native environment or cloud chain.
    ProviderNative,
    /// Official Codex CLI `ChatGPT` OAuth state.
    CodexOauth,
}

/// A provider route an adapter can activate through configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LlmConfigurableProvider {
    /// Provider route.
    pub provider: String,
    /// Human-readable name.
    pub display_name: String,
    /// User-settings namespace.
    pub settings_ns: String,
    /// Path within that namespace.
    pub settings_path: Vec<String>,
    /// Supported authentication setup.
    pub authentication: LlmProviderAuthentication,
    /// Whether configuration alone declared the route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared: Option<bool>,
}

/// One endpoint-discovered model candidate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LlmDiscoveredModel {
    /// Provider model id.
    pub id: String,
    /// Optional display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Advertised combined context capacity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// Advertised output cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
}

/// Draft endpoint interrogation request for model discovery.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LlmModelDiscoveryRequest {
    /// Existing provider route being edited.
    pub provider: Option<String>,
    /// Draft endpoint URL.
    pub base_url: Option<String>,
    /// Draft wire protocol.
    pub api: Option<String>,
    /// One-shot credential; never persisted by the runtime.
    pub api_key: Option<String>,
    /// Caller cancellation channel.
    pub signal: Option<AbortSignal>,
}

/// One model advertised by a registered provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LlmModelInfo {
    /// Owning provider route.
    pub provider: String,
    /// Exact model id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Optional distinction from similar models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Accepted request modalities; absence means unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<ModelModality>>,
}

/// Provider-owned combined context capacity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LlmModelContext {
    /// Maximum combined input and response tokens.
    pub context_window: u64,
}

/// Display metadata for one adapter-owned reasoning effort.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmReasoningEffortInfo {
    /// Opaque stable effort id.
    pub id: ReasoningEffortId,
    /// Display name.
    pub name: String,
    /// Optional distinction from similar efforts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Reasoning levels supported by one exact model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LlmModelReasoningInfo {
    /// Efforts in adapter-preferred order.
    pub efforts: Vec<LlmReasoningEffortInfo>,
    /// Adapter-selected default effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<ReasoningEffortId>,
}

/// Exact-route model metadata resolved by its owning adapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LlmResolvedModelInfo {
    /// Owning provider route.
    pub provider: String,
    /// Exact model id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Accepted request modalities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<ModelModality>>,
    /// Context capacity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<LlmModelContext>,
    /// Adapter-owned request default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_max_tokens: Option<u64>,
    /// Adapter-owned reasoning metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<LlmModelReasoningInfo>,
}

/// Cloneable caller-cancellation signal.
#[derive(Debug, Default)]
struct AbortState {
    aborted: AtomicBool,
    sources: Vec<AbortSignal>,
}

/// Cloneable caller-cancellation signal.
#[derive(Clone, Debug, Default)]
pub struct AbortSignal(Arc<AbortState>);

impl AbortSignal {
    /// Requests cancellation.
    pub fn abort(&self) {
        self.0.aborted.store(true, Ordering::Release);
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_aborted(&self) -> bool {
        self.0.aborted.load(Ordering::Acquire) || self.0.sources.iter().any(Self::is_aborted)
    }

    /// Creates one live signal that observes cancellation from either source.
    ///
    /// Equal inputs preserve their exact identity; distinct inputs are kept as
    /// flat sources so wrapper replacement cannot detach caller cancellation.
    #[must_use]
    pub fn fuse(first: &Self, second: &Self) -> Self {
        if first == second {
            return first.clone();
        }
        Self(Arc::new(AbortState {
            aborted: AtomicBool::new(false),
            sources: vec![first.clone(), second.clone()],
        }))
    }
}

impl PartialEq for AbortSignal {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for AbortSignal {}

/// Provider-neutral auxiliary-call classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LlmRequestPurpose {
    /// Conversation compaction.
    Compaction,
    /// Session-title generation.
    SessionTitle,
}

/// Conversation request configuration fields that affect the request epoch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LlmCallConfig {
    /// Registered provider route.
    pub provider: String,
    /// Provider-owned model id.
    pub model: String,
    /// Adapter-owned reasoning level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffortId>,
    /// Sampling temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Output cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// Stop sequences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
}

/// Config fields materialized by exact adapter resolution.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LlmCallConfigAdapterDefaults {
    /// Reasoning effort came from the adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<bool>,
    /// Output cap came from the adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<bool>,
}

/// Fully assembled provider-neutral request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateOptions {
    /// Registered provider route.
    pub provider: String,
    /// Provider-owned model id.
    pub model: String,
    /// Adapter-owned reasoning level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffortId>,
    /// Ordered conversation messages.
    pub messages: Vec<Message>,
    /// System prompt slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Model-visible tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolSchema>>,
    /// Sampling temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Output cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// Stop sequences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    /// Caller cancellation channel.
    #[serde(skip)]
    pub signal: Option<AbortSignal>,
    /// Session id for routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Auxiliary request purpose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<LlmRequestPurpose>,
}

/// Whether a chunk carries non-empty model output.
#[must_use]
pub fn is_token_delta(chunk: &StreamChunk) -> bool {
    match chunk {
        StreamChunk::TextDelta { text, .. } | StreamChunk::ReasoningDelta { text, .. } => {
            !text.is_empty()
        }
        StreamChunk::ToolCallDelta {
            name,
            arguments_delta,
            ..
        } => name.is_some() || !arguments_delta.is_empty(),
        StreamChunk::BlockStart { .. }
        | StreamChunk::BlockEnd { .. }
        | StreamChunk::Usage { .. }
        | StreamChunk::Finish { .. } => false,
    }
}
