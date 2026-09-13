//! Model-request inspection contracts reconstructed from durable Session events.

use indexmap::IndexMap;
use seekdeep_lossless_json::{JsonString, JsonValue as Value};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Provider request configuration recorded on an Assistant lifecycle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssistantRequestConfig {
    /// Registered provider route.
    pub provider: JsonString,
    /// Provider-owned model identity.
    pub model: JsonString,
    /// Request purpose extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<JsonString>,
    /// Provider-specific thinking mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<JsonString>,
    /// Provider-specific reasoning effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<JsonString>,
    /// Sampling temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Output-token ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// Provider stop strings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<JsonString>>,
}

/// Stable provider/model identity reported for one completed request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantProvenanceView {
    /// Registered provider route.
    pub provider: JsonString,
    /// Provider-owned model identity.
    pub model: JsonString,
}

/// Complete model-visible request header for one ordinary generation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationPromptSnapshot {
    /// Effective provider, model, and sampling configuration.
    pub config: AssistantRequestConfig,
    /// Rendered system prompt, including the empty prompt.
    pub system: JsonString,
    /// Complete schema catalog sent to the model in stable order.
    pub tools: Vec<Value>,
}

/// Kind of model-visible prompt change introduced by a request header.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RequestPromptChangeKind {
    /// First recorded request header.
    Initial,
    /// Only the system prompt changed.
    System,
    /// Only the Tool catalog changed.
    Tools,
    /// System prompt and Tool catalog changed together.
    SystemAndTools,
}

/// System or Tool change introduced while preparing one ordinary request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestPromptChange {
    /// Request-header event sequence.
    pub seq: u64,
    /// Request-header Unix epoch milliseconds.
    pub time: i64,
    /// Difference from the previously recorded request header.
    pub kind: RequestPromptChangeKind,
    /// State immediately before the change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<Box<ConversationPromptSnapshot>>,
}

/// Durable provider-request lifecycle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestStatus {
    /// Request is still in flight.
    Running,
    /// Request completed successfully.
    Complete,
    /// Request completed with an error.
    Error,
}

/// Presence-preserving optional arbitrary JSON.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum OptionalJson {
    /// Property was absent.
    #[default]
    Absent,
    /// Property was present, including explicit JSON null.
    Present(Value),
}

impl OptionalJson {
    fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }
}

impl Serialize for OptionalJson {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Absent => serializer.serialize_unit(),
            Self::Present(value) => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for OptionalJson {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        <Value as Deserialize>::deserialize(deserializer).map(Self::Present)
    }
}

/// Lifecycle fields shared by ordinary and compaction requests.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestViewBase {
    /// Sequence that opened the operation.
    pub start_seq: u64,
    /// Start Unix epoch milliseconds.
    pub started_at: i64,
    /// Completion Unix epoch milliseconds, or null while running.
    pub completed_at: Option<i64>,
    /// Current lifecycle state.
    pub status: RequestStatus,
    /// Stable rendered failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonString>,
    /// Completed provider/model identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<AssistantProvenanceView>,
    /// Effective provider request configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_config: Option<AssistantRequestConfig>,
    /// Provider-owned usage payload.
    #[serde(default, skip_serializing_if = "OptionalJson::is_absent")]
    pub usage: OptionalJson,
    /// Durable result message or summary sequence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_seq: Option<u64>,
}

/// One provider request assembled from durable lifecycle events.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "purpose", rename_all = "lowercase")]
pub enum RequestView {
    /// Ordinary Assistant generation.
    Assistant {
        /// Shared lifecycle fields.
        #[serde(flatten)]
        base: Box<RequestViewBase>,
        /// Owning Turn.
        turn: u64,
        /// Agent-loop Step.
        step: u64,
        /// Effective ordinary request input.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<Box<ConversationPromptSnapshot>>,
        /// Prompt change logged for this request.
        #[serde(
            rename = "promptChange",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        prompt_change: Option<Box<RequestPromptChange>>,
        /// Retry ordinal after a failed request.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry: Option<u64>,
        /// Retry ceiling.
        #[serde(
            rename = "maxRetries",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        max_retries: Option<u64>,
        /// Delay before the retry.
        #[serde(
            rename = "retryDelayMs",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        retry_delay_ms: Option<u64>,
    },
    /// Provider request used for context compaction.
    Compaction {
        /// Shared lifecycle fields.
        #[serde(flatten)]
        base: Box<RequestViewBase>,
        /// Owning Turn, or null for between-Turn manual compaction.
        turn: Option<u64>,
        /// Direct compaction always carries zero.
        step: u64,
        /// Durable replacement message sequence.
        #[serde(
            rename = "replacementSeq",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        replacement_seq: Option<u64>,
        /// Safe compaction summary projection.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<Vec<Value>>,
        /// Complete provider output before safe projection.
        #[serde(rename = "rawOutput", default, skip_serializing_if = "Option::is_none")]
        raw_output: Option<Vec<Value>>,
    },
}

impl<'de> Deserialize<'de> for RequestView {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct AssistantFields {
            turn: u64,
            step: u64,
            prompt: Option<Box<ConversationPromptSnapshot>>,
            prompt_change: Option<Box<RequestPromptChange>>,
            retry: Option<u64>,
            max_retries: Option<u64>,
            retry_delay_ms: Option<u64>,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct CompactionFields {
            turn: Option<u64>,
            step: u64,
            replacement_seq: Option<u64>,
            summary: Option<Vec<Value>>,
            raw_output: Option<Vec<Value>>,
        }

        let value = <Value as Deserialize>::deserialize(deserializer)?;
        let purpose = value
            .get_value("purpose")
            .and_then(Value::as_str)
            .ok_or_else(|| serde::de::Error::missing_field("purpose"))?;
        match purpose {
            "assistant" => {
                let base = Box::new(value.deserialize().map_err(serde::de::Error::custom)?);
                let fields: AssistantFields =
                    value.deserialize().map_err(serde::de::Error::custom)?;
                Ok(Self::Assistant {
                    base,
                    turn: fields.turn,
                    step: fields.step,
                    prompt: fields.prompt,
                    prompt_change: fields.prompt_change,
                    retry: fields.retry,
                    max_retries: fields.max_retries,
                    retry_delay_ms: fields.retry_delay_ms,
                })
            }
            "compaction" => {
                let base = Box::new(value.deserialize().map_err(serde::de::Error::custom)?);
                let fields: CompactionFields =
                    value.deserialize().map_err(serde::de::Error::custom)?;
                Ok(Self::Compaction {
                    base,
                    turn: fields.turn,
                    step: fields.step,
                    replacement_seq: fields.replacement_seq,
                    summary: fields.summary,
                    raw_output: fields.raw_output,
                })
            }
            _ => Err(serde::de::Error::unknown_variant(
                purpose,
                &["assistant", "compaction"],
            )),
        }
    }
}

/// Request data consumed by stage-oriented Trajectory views.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestInspectionSnapshot {
    /// Provider requests in durable display order.
    pub requests: Vec<RequestView>,
    /// Tool schema by call identity in insertion order.
    pub call_schemas: IndexMap<String, Value>,
}
