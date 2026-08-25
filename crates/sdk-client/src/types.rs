//! Launch, notification, filtering, and owned-activity result types.

use std::{collections::BTreeMap, sync::Arc};

use seekdeep_core::session::{SessionEvent, SessionId};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One server-to-client notification as received from the wire.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessNotification {
    /// JSON-RPC notification method.
    pub method: String,
    /// Raw method-specific params.
    pub params: Map<String, Value>,
}

/// Predicate deciding whether a subscription receives one notification.
pub type NotificationFilter = Arc<dyn Fn(&HarnessNotification) -> bool + Send + Sync>;

/// Runtime process launch and timeout options.
#[derive(Clone, Debug)]
pub struct HarnessClientOptions {
    /// Runtime executable.
    pub command: String,
    /// Runtime arguments.
    pub args: Vec<String>,
    /// Runtime process working directory.
    pub cwd: Option<String>,
    /// Complete child environment when present.
    pub env: Option<BTreeMap<String, String>>,
    /// Default per-request timeout.
    pub request_timeout_ms: Option<f64>,
    /// Protocol shutdown timeout.
    pub shutdown_timeout_ms: f64,
    /// Cooperative EOF quiescence grace.
    pub dispose_eof_grace_ms: f64,
    /// Termination confirmation grace.
    pub dispose_grace_ms: f64,
}

impl HarnessClientOptions {
    /// Builds the mandatory launch portion with source defaults.
    #[must_use]
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            cwd: None,
            env: None,
            request_timeout_ms: None,
            shutdown_timeout_ms: 1_000.0,
            dispose_eof_grace_ms: 6_000.0,
            dispose_grace_ms: 3_000.0,
        }
    }
}

/// High-level runtime launch and route options.
#[derive(Clone, Debug)]
pub struct DeepSeekHarnessOptions {
    /// Runtime launch settings.
    pub launch: HarnessClientOptions,
    /// Workspace cwd; defaults to launch cwd then current directory.
    pub cwd: Option<String>,
    /// Provider route; defaults `deepseek-official`.
    pub provider: Option<String>,
    /// Model; defaults `deepseek-v4-flash`.
    pub model: Option<String>,
    /// Optional per-request output-token cap.
    pub max_tokens: Option<u64>,
}

/// One owned activity interval from inbox receipt through idle.
#[derive(Clone, Debug, PartialEq)]
pub struct RunResult {
    /// Root session.
    pub session_id: SessionId,
    /// Concatenated final assistant text.
    pub final_response: String,
    /// Root session events in wire order.
    pub events: Vec<SessionEvent>,
    /// Root and discovered-descendant notifications in wire order.
    pub notifications: Vec<HarnessNotification>,
}
