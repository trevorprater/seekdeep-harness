//! Client plugin descriptor and Host invocation teaching errors.

use seekdeep_cordis_dynamic_types::{
    CordisDynamicPluginId, DynamicCordisInvokeErrorCode, DynamicCordisInvokeResult,
};
use serde_json::Value;

/// Stable Client plugin name.
pub const CLIENT_RUNNER_NAME: &str = "cordis-client-runner";

/// Exact required Client services and Remote namespace.
pub const CLIENT_RUNNER_INJECT: &[&str] = &[
    "loader",
    "modules",
    "slots",
    "remote",
    "remote.dynamicCordisRunner",
];

/// Client-side failure preserving the optional Host stack.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
#[error("{message}")]
pub struct ClientHostCallError {
    /// Teaching error at the Client call site.
    pub message: String,
    /// Original Host stack when supplied.
    pub host_stack: Option<String>,
}

/// Unwraps one Runner invocation or returns its code-specific teaching error.
///
/// # Errors
///
/// Returns stopped, stale, missing-method, or handler failure diagnostics.
pub fn unwrap_host_invoke(
    plugin_id: &CordisDynamicPluginId,
    method: &str,
    result: DynamicCordisInvokeResult,
) -> Result<Value, ClientHostCallError> {
    match result {
        DynamicCordisInvokeResult::Success { value } => Ok(value),
        DynamicCordisInvokeResult::Failure { code, error } => {
            let call = format!("host.call({method:?}) on {plugin_id}");
            let message = match code {
                DynamicCordisInvokeErrorCode::PluginNotRunning => format!(
                    "{call} found no active Host half — the Plugin is stopped or was removed."
                ),
                DynamicCordisInvokeErrorCode::StaleRun => {
                    format!("{call} belongs to an activation that has already been replaced.")
                }
                DynamicCordisInvokeErrorCode::MethodNotFound => format!(
                    "{call} is not registered: the host half must declare it with harness.handle({method:?}, fn)."
                ),
                DynamicCordisInvokeErrorCode::HandlerError => {
                    format!("{call} failed inside the host handler: {}", error.message)
                }
            };
            Err(ClientHostCallError {
                message,
                host_stack: error.stack,
            })
        }
    }
}

/// Adds package/method and JSON-contract teaching to a wire or codec failure.
#[must_use]
pub fn host_wire_failure(plugin_id: &CordisDynamicPluginId, method: &str, failure: &str) -> String {
    format!(
        "host.call({method:?}) on {plugin_id} did not complete: {failure}\nBoth directions carry JSON only: pass plain JSON data as the argument — or omit it, and the handler receives null — and answer from harness.handle({method:?}, fn) with JSON (`return null` when there is nothing to report)."
    )
}
