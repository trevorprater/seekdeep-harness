//! Python-facing configuration and open JSON protocol values.

use std::collections::BTreeMap;

use seekdeep_identity::SessionId;
use seekdeep_llm::{ModelId, ProviderId};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Opaque incoming JSON-RPC identity using Python's string-or-integer test.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(Value);

impl RequestId {
    /// Brands strings, integers, and Python's integer-subtype booleans.
    pub fn from_value(value: &Value) -> Option<Self> {
        (value.is_string() || value.is_boolean() || value.is_i64() || value.is_u64())
            .then(|| Self(value.clone()))
    }

    /// Returns the original wire identity without coercion.
    pub fn value(&self) -> &Value {
        &self.0
    }

    pub(crate) fn correlation_key(&self) -> String {
        crate::values::python_str(&self.0)
    }
}

/// Unsolicited runtime notification with an object payload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    /// Protocol method name, including unknown methods.
    pub method: String,
    /// Non-object wire params are represented as an empty object.
    pub payload: Map<String, Value>,
}

/// Incoming peer request awaiting an explicit response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IncomingRequest {
    /// Original peer correlation identity.
    pub id: RequestId,
    /// Protocol method name.
    pub method: String,
    /// Non-object wire params are represented as an empty object.
    pub payload: Map<String, Value>,
}

/// Mutable low-level Python client configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct HarnessConfig {
    /// Explicit runtime executable, preferred over the legacy bridge spelling.
    pub runtime_bin: Option<String>,
    /// Legacy explicit executable channel.
    pub bridge_bin: Option<String>,
    /// Complete argv; an empty tuple falls through to default argv selection.
    pub launch_args_override: Option<Vec<String>>,
    /// Child working directory, resolved at startup.
    pub cwd: Option<String>,
    /// Overrides merged onto the caller's current environment.
    pub env: Option<BTreeMap<String, String>>,
    /// Default request timeout in seconds; None waits indefinitely.
    pub request_timeout_seconds: Option<f64>,
    /// Shutdown request override and process wait; None defers the request default and waits for exit.
    pub shutdown_timeout_seconds: Option<f64>,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            runtime_bin: None,
            bridge_bin: None,
            launch_args_override: None,
            cwd: None,
            env: None,
            request_timeout_seconds: None,
            shutdown_timeout_seconds: Some(1.0),
        }
    }
}

/// High-level synchronous harness configuration, preserving Python field names.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct HarnessOptions {
    /// Provider selected during initialization.
    pub provider: ProviderId,
    /// Model selected during initialization.
    pub model: ModelId,
    /// Optional maxTokens wire value.
    pub max_tokens: Option<Value>,
    /// Workspace directory exposed to tools and initialization.
    pub cwd: Option<String>,
    /// Child working directory, independently selectable from the workspace.
    pub runtime_cwd: Option<String>,
    /// Explicit session-log root passed through the launch environment.
    pub session_root: Option<String>,
    /// Explicit Cordis configuration path.
    pub cordis: Option<String>,
    /// Caller-supplied environment overrides.
    pub env: BTreeMap<String, String>,
    /// Explicit runtime executable.
    pub runtime_bin: Option<String>,
    /// Complete runtime argv override.
    pub launch_args_override: Option<Vec<String>>,
    /// Default request timeout in seconds.
    pub request_timeout_seconds: Option<f64>,
    /// Shutdown request and process-termination timeout in seconds.
    pub shutdown_timeout_seconds: Option<f64>,
    /// Optional provider endpoint override.
    pub base_url: Option<String>,
    /// Optional provider API-key override; never included in diagnostics.
    pub api_key: Option<String>,
}

impl Default for HarnessOptions {
    fn default() -> Self {
        Self {
            provider: ProviderId::new("deepseek-official"),
            model: ModelId::new("deepseek-v4-flash"),
            max_tokens: None,
            cwd: None,
            runtime_cwd: None,
            session_root: None,
            cordis: None,
            env: BTreeMap::new(),
            runtime_bin: None,
            launch_args_override: None,
            request_timeout_seconds: None,
            shutdown_timeout_seconds: Some(1.0),
            base_url: None,
            api_key: None,
        }
    }
}

/// One root-session receipt-to-idle interval and its descendant notifications.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunResult {
    /// Root session that owns this run interval.
    pub session_id: SessionId,
    /// Text content of the last assistant/message with a content list.
    pub final_response: String,
    /// String kind of the last turn/end, or None if absent.
    pub finish_reason: Option<String>,
    /// Object events belonging only to the root session.
    pub events: Vec<Map<String, Value>>,
    /// Root and descendant notifications observed after the matching inbox receipt.
    pub notifications: Vec<Notification>,
    /// Caller-configured session-log root.
    pub session_root: Option<String>,
}
