//! Provider-neutral content, stream, and request vocabulary.

use seekdeep_attachment::ImageAttachmentRef;
pub use seekdeep_util::abort::AbortSignal;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use serde_json::{Map, Value};

use crate::{
    brand::{CallId, ModelId, ProviderId, ReasoningEffortId, SessionId},
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
        /// Attachment-service owned durable reference.
        attachment: ImageAttachmentRef,
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
                object.insert(
                    "attachment".to_owned(),
                    serde_json::to_value(attachment).map_err(serde::ser::Error::custom)?,
                );
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
            Self::Unknown { fields, .. } => object.extend(
                fields
                    .iter()
                    .filter(|(field, _)| field.as_str() != "type")
                    .map(|(field, value)| (field.clone(), value.clone())),
            ),
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
        Ok(
            decode_known_content_block(&block_type, &object).unwrap_or(Self::Unknown {
                block_type,
                fields: object,
            }),
        )
    }
}

fn decode_known_content_block(
    block_type: &str,
    object: &Map<String, Value>,
) -> Option<ContentBlock> {
    match block_type {
        "text" => Some(ContentBlock::Text {
            text: object.get("text")?.as_str()?.to_owned(),
        }),
        "reasoning" => Some(ContentBlock::Reasoning {
            text: object.get("text")?.as_str()?.to_owned(),
        }),
        "image" => Some(ContentBlock::Image {
            attachment: serde_json::from_value(object.get("attachment")?.clone()).ok()?,
        }),
        "tool-call" => Some(ContentBlock::ToolCall {
            id: CallId::new(object.get("id")?.as_str()?),
            name: object.get("name")?.as_str()?.to_owned(),
            arguments: object.get("arguments")?.as_str()?.to_owned(),
        }),
        "tool-result" => Some(ContentBlock::ToolResult {
            tool_call_id: CallId::new(object.get("toolCallId")?.as_str()?),
            content: serde_json::from_value(object.get("content")?.clone()).ok()?,
            is_error: object
                .get("isError")
                .map(|value| serde_json::from_value(value.clone()))
                .transpose()
                .ok()?,
        }),
        _ => None,
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
#[derive(Clone, Debug, PartialEq)]
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
    /// Plugin- or future-core reason retained without interpretation.
    Unknown {
        /// Merge-extensible reason tag.
        kind: String,
        /// Every remaining wire field.
        fields: Map<String, Value>,
    },
}

impl FinishReason {
    /// Returns the merge-extensible wire tag.
    #[must_use]
    pub fn kind(&self) -> &str {
        match self {
            Self::Stop => "stop",
            Self::ToolCalls => "tool-calls",
            Self::MaxTokens => "max-tokens",
            Self::Aborted { .. } => "aborted",
            Self::Error { .. } => "error",
            Self::Unknown { kind, .. } => kind,
        }
    }
}

impl Serialize for FinishReason {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut object = Map::new();
        match self {
            Self::Aborted { failure } | Self::Error { failure } => {
                object.insert(
                    "failure".to_owned(),
                    serde_json::to_value(failure).map_err(serde::ser::Error::custom)?,
                );
            }
            Self::Unknown { fields, .. } => object.extend(fields.clone()),
            Self::Stop | Self::ToolCalls | Self::MaxTokens => {}
        }
        object.insert("kind".to_owned(), Value::String(self.kind().to_owned()));
        Value::Object(object).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for FinishReason {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let Value::Object(mut object) = Value::deserialize(deserializer)? else {
            return Err(D::Error::custom("finish reason must be an object"));
        };
        let kind = take_string::<D::Error>(&mut object, "kind")?;
        match kind.as_str() {
            "stop" => Ok(Self::Stop),
            "tool-calls" => Ok(Self::ToolCalls),
            "max-tokens" => Ok(Self::MaxTokens),
            "aborted" | "error" => {
                let failure = serde_json::from_value(
                    object
                        .remove("failure")
                        .ok_or_else(|| D::Error::missing_field("failure"))?,
                )
                .map_err(D::Error::custom)?;
                if kind == "aborted" {
                    Ok(Self::Aborted { failure })
                } else {
                    Ok(Self::Error { failure })
                }
            }
            _ => Ok(Self::Unknown {
                kind,
                fields: object,
            }),
        }
    }
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
    pub id: ProviderId,
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
    pub provider: ProviderId,
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
    pub id: ModelId,
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
    pub provider: Option<ProviderId>,
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
    pub provider: ProviderId,
    /// Exact model id.
    pub id: ModelId,
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
    pub provider: ProviderId,
    /// Exact model id.
    pub id: ModelId,
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
    pub provider: ProviderId,
    /// Provider-owned model id.
    pub model: ModelId,
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
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateOptions {
    /// Registered provider route.
    pub provider: ProviderId,
    /// Provider-owned model id.
    pub model: ModelId,
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
    pub session_id: Option<SessionId>,
    /// Auxiliary request purpose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<LlmRequestPurpose>,
    /// Process-local identity showing that the agent loop assembled this exact
    /// request lineage. This field is deliberately absent from every wire
    /// representation and cannot be supplied by callers.
    #[serde(skip)]
    agent_loop_request: Option<uuid::Uuid>,
}

impl GenerateOptions {
    /// Creates an unmarked request with every optional field omitted.
    #[must_use]
    pub fn new(provider: ProviderId, model: ModelId, messages: Vec<Message>) -> Self {
        Self {
            provider,
            model,
            reasoning_effort: None,
            messages,
            system: None,
            tools: None,
            temperature: None,
            max_tokens: None,
            stop: None,
            signal: None,
            session_id: None,
            purpose: None,
            agent_loop_request: None,
        }
    }

    /// Marks this request as assembled from an agent session's durable log.
    ///
    /// The random process-local identity survives the internal by-value moves
    /// and clones required by the Rust middleware chain, but never survives a
    /// serialization boundary.
    #[must_use]
    pub fn mark_agent_loop_request(mut self) -> Self {
        self.agent_loop_request = Some(uuid::Uuid::new_v4());
        self
    }

    /// Returns whether the request carries an agent-loop process-local mark.
    #[must_use]
    pub const fn is_agent_loop_request(&self) -> bool {
        self.agent_loop_request.is_some()
    }

    /// Clones an internally routed request without losing its exact-object mark.
    ///
    /// Ordinary [`Clone`] deliberately clears the process-local marker, just
    /// as spreading or cloning the source JavaScript request creates a distinct
    /// object absent from its `WeakSet`. Runtime routing uses this narrower
    /// operation only where Rust ownership requires a value copy while the
    /// source passes the same object onward.
    #[doc(hidden)]
    #[must_use]
    pub fn clone_preserving_agent_loop_request(&self) -> Self {
        let mut cloned = self.clone();
        cloned.agent_loop_request = self.agent_loop_request;
        cloned
    }

    pub(crate) fn clear_agent_loop_request(&mut self) {
        self.agent_loop_request = None;
    }
}

impl Clone for GenerateOptions {
    fn clone(&self) -> Self {
        Self {
            provider: self.provider.clone(),
            model: self.model.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
            messages: self.messages.clone(),
            system: self.system.clone(),
            tools: self.tools.clone(),
            temperature: self.temperature,
            max_tokens: self.max_tokens,
            stop: self.stop.clone(),
            signal: self.signal.clone(),
            session_id: self.session_id.clone(),
            purpose: self.purpose,
            agent_loop_request: None,
        }
    }
}

/// Marks one request lineage as assembled by the agent loop.
pub fn mark_agent_loop_request(request: GenerateOptions) -> GenerateOptions {
    request.mark_agent_loop_request()
}

/// Tests whether a request carries the process-local agent-loop identity.
#[must_use]
pub const fn is_agent_loop_request(request: &GenerateOptions) -> bool {
    request.is_agent_loop_request()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_finish_reasons_round_trip_losslessly() {
        let wire = serde_json::json!({
            "kind": "provider-policy",
            "category": "safety",
            "retryable": false
        });
        let reason: FinishReason = serde_json::from_value(wire.clone()).unwrap();
        let FinishReason::Unknown { kind, fields } = &reason else {
            panic!("unknown reason must remain explicit");
        };
        assert_eq!(kind, "provider-policy");
        assert_eq!(fields["category"], "safety");
        assert_eq!(serde_json::to_value(reason).unwrap(), wire);
    }

    #[test]
    fn agent_loop_mark_belongs_only_to_the_exact_request_value() {
        let request =
            GenerateOptions::new(ProviderId::new("mock"), ModelId::new("model"), Vec::new());
        let copy = request.clone();
        let marked = request.mark_agent_loop_request();
        assert!(marked.is_agent_loop_request());
        assert!(!copy.is_agent_loop_request());
        assert!(!marked.clone().is_agent_loop_request());
        assert!(
            marked
                .clone_preserving_agent_loop_request()
                .is_agent_loop_request()
        );
    }
}
