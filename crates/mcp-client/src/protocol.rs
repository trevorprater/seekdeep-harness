//! MCP protocol vocabulary and injectable client-generation boundary.

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use seekdeep_llm::AbortSignal;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::Notify;

use crate::Config;

/// One tool advertised by an MCP server.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpTool {
    /// Server-owned wire name.
    pub name: String,
    /// Model-facing description.
    #[serde(default)]
    pub description: Option<String>,
    /// Raw MCP input schema.
    pub input_schema: Value,
    /// Optional structured-output schema.
    #[serde(default)]
    pub output_schema: Option<Value>,
    /// Optional task-execution declaration.
    #[serde(default)]
    pub execution: Option<McpToolExecution>,
}

/// Tool execution capabilities advertised by a server.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolExecution {
    /// Task support vocabulary; `required` is unsupported by this bridge.
    #[serde(default)]
    pub task_support: Option<String>,
}

/// One page from `tools/list`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolPage {
    /// Tools in server order.
    #[serde(default)]
    pub tools: Vec<McpTool>,
    /// Cursor for the next page.
    #[serde(default)]
    pub next_cursor: Option<String>,
}

/// Exact MCP client generation used by discovery, calls, and supervision.
#[async_trait]
pub trait McpClient: std::fmt::Debug + Send + Sync {
    /// Establishes the transport and completes the initialize handshake.
    async fn connect(&self) -> anyhow::Result<()>;
    /// Fetches one uncached tools page.
    async fn list_tools(&self, cursor: Option<&str>) -> anyhow::Result<McpToolPage>;
    /// Calls one raw tool name with cooperative cancellation.
    async fn call_tool(
        &self,
        raw_name: &str,
        arguments: Map<String, Value>,
        signal: AbortSignal,
    ) -> anyhow::Result<Value>;
    /// Closes and fully reaps this generation.
    async fn close(&self) -> anyhow::Result<()>;
    /// Signal that settles only after this generation is no longer usable.
    fn closed_signal(&self) -> AbortSignal;
    /// Current tool-list notification generation.
    fn list_change_generation(&self) -> u64;
    /// Waits for a notification newer than `after`.
    async fn wait_list_change(&self, after: u64);
}

/// Factory for fresh protocol generations.
#[async_trait]
pub trait McpClientFactory: std::fmt::Debug + Send + Sync {
    /// Builds one unconnected generation from the immutable plugin config.
    async fn create(&self, config: &Config) -> anyhow::Result<Arc<dyn McpClient>>;
}

/// Shared close and list-change signals for concrete clients and test fixtures.
#[derive(Debug, Default)]
pub struct McpClientSignals {
    closed: AbortSignal,
    list_generation: AtomicU64,
    list_changed: Notify,
}

impl McpClientSignals {
    /// Marks the exact generation closed once.
    pub fn close(&self) {
        self.closed
            .abort_with_reason(Value::String("MCP generation closed".to_owned()));
        self.list_changed.notify_waiters();
    }

    /// Announces one `notifications/tools/list_changed` observation.
    pub fn tools_changed(&self) {
        self.list_generation.fetch_add(1, Ordering::AcqRel);
        self.list_changed.notify_waiters();
    }

    /// Cloneable close signal.
    #[must_use]
    pub fn closed_signal(&self) -> AbortSignal {
        self.closed.clone()
    }

    /// Current notification generation.
    #[must_use]
    pub fn list_change_generation(&self) -> u64 {
        self.list_generation.load(Ordering::Acquire)
    }

    /// Waits until a newer notification arrives or the generation closes.
    pub async fn wait_list_change(&self, after: u64) {
        loop {
            let notified = self.list_changed.notified();
            if self.list_change_generation() > after || self.closed.is_aborted() {
                return;
            }
            notified.await;
        }
    }
}

/// Completes stateless tool schemas that omit their required object root type.
pub fn normalize_tool_schemas(reply: &mut Value) {
    let Some(tools) = reply
        .pointer_mut("/result/tools")
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for tool in tools {
        let Some(schema) = tool.get_mut("inputSchema").and_then(Value::as_object_mut) else {
            continue;
        };
        schema
            .entry("type".to_owned())
            .or_insert_with(|| Value::String("object".to_owned()));
    }
}

/// Converts case-preserving configured headers into a request-header map.
pub(crate) fn configured_headers(
    headers: &BTreeMap<String, String>,
) -> anyhow::Result<reqwest::header::HeaderMap> {
    let mut output = reqwest::header::HeaderMap::new();
    for (name, value) in headers {
        output.insert(
            reqwest::header::HeaderName::from_bytes(name.as_bytes())?,
            reqwest::header::HeaderValue::from_str(value)?,
        );
    }
    Ok(output)
}
