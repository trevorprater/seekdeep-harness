//! Replay-stable view models derived only from frozen tool-call data.

use seekdeep_cordis_dynamic_types::{
    CordisDynamicPackageId, CordisDynamicPluginId, CordisDynamicPluginRunId, DynamicCordisRunMode,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Lifecycle of the tool call itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CordisToolState {
    /// Arguments are still streaming or execution has not settled.
    Running,
    /// Execution settled successfully.
    Ok,
    /// Execution settled with a tool error.
    Error,
    /// Execution was interrupted.
    Stopped,
}

/// Running tool-call fields consumed by Cordis cards.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningToolCall {
    /// Streaming JSON argument text; it may be a truncated prefix.
    #[serde(default)]
    pub args_raw: String,
}

/// Settled call header retained in a tool result.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettledToolCall {
    /// Tool name.
    pub name: String,
    /// Complete or retained argument text.
    #[serde(default)]
    pub args_raw: String,
}

/// Structured execution error retained in a tool result.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ToolResultError {
    /// Error class or source name.
    pub name: String,
    /// Stable error code.
    pub code: String,
}

/// Settled tool-result fields consumed by Cordis cards.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettledToolResult {
    /// Literal result-node discriminant.
    pub kind: String,
    /// Append-only session-log sequence.
    pub seq: u64,
    /// Original call header when retained by the event window.
    #[serde(default)]
    pub call: Option<SettledToolCall>,
    /// Tool result content blocks.
    #[serde(default)]
    pub content: Vec<Value>,
    /// Tool-result error flag.
    #[serde(default)]
    pub is_error: bool,
    /// Structured error when supplied.
    #[serde(default)]
    pub error: Option<ToolResultError>,
    /// Successful presentation metadata.
    #[serde(default)]
    pub meta: Option<Value>,
}

/// Either a running call or settled tool result.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum ToolCallBlock {
    /// Settled result node; this variant is attempted first because running calls have no `kind`.
    Settled(Box<SettledToolResult>),
    /// Running call node.
    Running(RunningToolCall),
}

/// Frozen `cordis_define` presentation data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CordisDefineCard {
    /// Minted stable Plugin identity, available only from successful result metadata.
    pub plugin_id: Option<CordisDynamicPluginId>,
    /// Minted immutable Package identity, available only from successful result metadata.
    pub package_id: Option<CordisDynamicPackageId>,
    /// Package label or raw first argument line while JSON is incomplete.
    pub name: Option<String>,
    /// User-facing package purpose.
    pub purpose: Option<String>,
    /// Host function body.
    pub host_code: Option<String>,
    /// Client function body.
    pub client_code: Option<String>,
    /// Settled result text.
    pub output: Option<String>,
    /// First output line for a failed call.
    pub error_summary: Option<String>,
    /// Tool-call lifecycle.
    pub state: CordisToolState,
}

/// Frozen `cordis_run` presentation data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CordisRunCard {
    /// Stable Plugin identity.
    pub plugin_id: Option<CordisDynamicPluginId>,
    /// Target immutable Package identity.
    pub package_id: Option<CordisDynamicPackageId>,
    /// Exact successful activation identity.
    pub plugin_run_id: Option<CordisDynamicPluginRunId>,
    /// Run or update intent when recognized.
    pub mode: Option<DynamicCordisRunMode>,
    /// Settled session-log sequence.
    pub seq: Option<u64>,
    /// Settled result text.
    pub output: Option<String>,
    /// First output line for a failed call.
    pub error_summary: Option<String>,
    /// Tool-call lifecycle.
    pub state: CordisToolState,
}

/// Frozen `cordis_stop` or `cordis_undefine` presentation data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CordisActionCard {
    /// Stable Plugin identity parsed from either accepted argument spelling.
    pub plugin_id: Option<CordisDynamicPluginId>,
    /// Settled result text.
    pub output: Option<String>,
    /// First output line for a failed call.
    pub error_summary: Option<String>,
    /// Tool-call lifecycle.
    pub state: CordisToolState,
}

/// Derives one Define card from its running or settled tool-call block.
#[must_use]
pub fn cordis_define_card(block: &ToolCallBlock) -> CordisDefineCard {
    let args_raw = args_raw(block);
    let args = parse_args(args_raw);
    let code = args.as_ref().and_then(|value| object_at(value, "code"));
    let state = state_of(block);
    let output = result_text_of(block);
    let meta = meta_object(block);
    let raw_name = (!args_raw.is_empty()).then(|| first_line(args_raw).to_owned());
    CordisDefineCard {
        plugin_id: meta
            .and_then(|value| string_at(value, "pluginId"))
            .map(CordisDynamicPluginId::new),
        package_id: meta
            .and_then(|value| string_at(value, "packageId"))
            .map(CordisDynamicPackageId::new),
        name: args
            .as_ref()
            .and_then(|value| string_at(value, "name").map(str::to_owned))
            .or(raw_name),
        purpose: args
            .as_ref()
            .and_then(|value| string_at(value, "purpose"))
            .map(str::to_owned),
        host_code: code
            .and_then(|value| string_at(value, "host"))
            .map(str::to_owned),
        client_code: code
            .and_then(|value| string_at(value, "client"))
            .map(str::to_owned),
        error_summary: error_summary(state, output.as_deref()),
        output,
        state,
    }
}

/// Derives one Run card from its running or settled tool-call block.
#[must_use]
pub fn cordis_run_card(block: &ToolCallBlock) -> CordisRunCard {
    let args = parse_args(args_raw(block));
    let meta = meta_object(block);
    let state = state_of(block);
    let output = result_text_of(block);
    let args_plugin_id = args.as_ref().and_then(|value| string_at(value, "pluginId"));
    let args_package_id = args
        .as_ref()
        .and_then(|value| string_at(value, "packageId"));
    let mode = match args.as_ref().and_then(|value| string_at(value, "mode")) {
        Some("run") => Some(DynamicCordisRunMode::Run),
        Some("update") => Some(DynamicCordisRunMode::Update),
        Some(_) | None => None,
    };
    CordisRunCard {
        plugin_id: meta
            .and_then(|value| string_at(value, "pluginId"))
            .or(args_plugin_id)
            .map(CordisDynamicPluginId::new),
        package_id: meta
            .and_then(|value| string_at(value, "packageId"))
            .or(args_package_id)
            .map(CordisDynamicPackageId::new),
        plugin_run_id: meta
            .and_then(|value| string_at(value, "pluginRunId"))
            .map(CordisDynamicPluginRunId::new),
        mode,
        seq: match block {
            ToolCallBlock::Settled(result) => Some(result.seq),
            ToolCallBlock::Running(_) => None,
        },
        error_summary: error_summary(state, output.as_deref()),
        output,
        state,
    }
}

/// Derives one Stop or Remove card from its running or settled tool-call block.
#[must_use]
pub fn cordis_action_card(block: &ToolCallBlock) -> CordisActionCard {
    let args = parse_args(args_raw(block));
    let state = state_of(block);
    let output = result_text_of(block);
    CordisActionCard {
        plugin_id: args
            .as_ref()
            .and_then(|value| string_at(value, "pluginId").or_else(|| string_at(value, "id")))
            .map(CordisDynamicPluginId::new),
        error_summary: error_summary(state, output.as_deref()),
        output,
        state,
    }
}

fn args_raw(block: &ToolCallBlock) -> &str {
    match block {
        ToolCallBlock::Settled(result) => result
            .call
            .as_ref()
            .map_or("", |call| call.args_raw.as_str()),
        ToolCallBlock::Running(call) => &call.args_raw,
    }
}

fn parse_args(args_raw: &str) -> Option<Value> {
    let parsed: Value = serde_json::from_str(args_raw).ok()?;
    (parsed.is_object() || parsed.is_array()).then_some(parsed)
}

fn first_line(text: &str) -> &str {
    text.split_once('\n').map_or(text, |(first, _)| first)
}

fn string_at<'a>(source: &'a Value, key: &str) -> Option<&'a str> {
    source.get(key)?.as_str().filter(|value| !value.is_empty())
}

fn object_at<'a>(source: &'a Value, key: &str) -> Option<&'a Value> {
    source
        .get(key)
        .filter(|value| value.is_object() || value.is_array())
}

fn state_of(block: &ToolCallBlock) -> CordisToolState {
    let ToolCallBlock::Settled(result) = block else {
        return CordisToolState::Running;
    };
    if result
        .error
        .as_ref()
        .is_some_and(|error| error.code == "interrupted")
    {
        CordisToolState::Stopped
    } else if result.is_error {
        CordisToolState::Error
    } else {
        CordisToolState::Ok
    }
}

fn meta_object(block: &ToolCallBlock) -> Option<&Value> {
    let ToolCallBlock::Settled(result) = block else {
        return None;
    };
    (!result.is_error)
        .then_some(result.meta.as_ref())
        .flatten()
        .filter(|value| value.is_object() || value.is_array())
}

fn result_text_of(block: &ToolCallBlock) -> Option<String> {
    let ToolCallBlock::Settled(result) = block else {
        return None;
    };
    let text = result
        .content
        .iter()
        .map(|item| {
            if item.get("type").and_then(Value::as_str) == Some("text") {
                item.get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            } else {
                serde_json::to_string_pretty(item).expect("tool content is JSON")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    if !text.is_empty() {
        return Some(text);
    }
    result
        .error
        .as_ref()
        .map(|error| format!("{}: {}", error.name, error.code))
}

fn error_summary(state: CordisToolState, output: Option<&str>) -> Option<String> {
    (state == CordisToolState::Error)
        .then_some(output)
        .flatten()
        .map(first_line)
        .map(str::to_owned)
}
