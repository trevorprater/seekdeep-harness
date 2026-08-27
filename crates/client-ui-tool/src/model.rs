//! Generic tool-row model derivation from frozen wire slices.

use serde_json::Value;

/// Generic atomic row variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolRowVariant {
    /// Search result.
    Search,
    /// File or URL read.
    Read,
    /// Shell command.
    Bash,
    /// File creation.
    Write,
    /// File edit.
    Edit,
    /// Code Mode program.
    Code,
    /// Unclassified tool.
    Others,
}

/// Tool row lifecycle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolRowState {
    /// Call has no result yet.
    Running,
    /// Settled successfully.
    Ok,
    /// Settled with an execution error.
    Error,
    /// Interrupted by lifecycle cancellation.
    Stopped,
}

/// Structured settled error identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolErrorInfo {
    /// Error class/name.
    pub name: String,
    /// Stable error code.
    pub code: String,
}

/// Frozen call-side fields retained by a settled result.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCallHead {
    /// Raw JSON arguments.
    pub args_raw: String,
}

/// Running or settled tool lifecycle.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolCallBlock {
    /// Call has not settled.
    Running {
        /// Stable call id.
        call_id: String,
        /// Raw JSON arguments.
        args_raw: String,
        /// Optional call-time render intent.
        call_view: Option<Value>,
    },
    /// Result has settled.
    Settled {
        /// Stable call id.
        call_id: String,
        /// Retained call head, absent after window truncation.
        call: Option<ToolCallHead>,
        /// Retained call-time render intent.
        call_view: Option<Value>,
        /// Result-time render intent.
        result_view: Option<Value>,
        /// Result content blocks.
        content: Vec<Value>,
        /// Execution error marker.
        is_error: bool,
        /// Structured error identity.
        error: Option<ToolErrorInfo>,
    },
}

impl ToolCallBlock {
    /// Stable call id.
    #[must_use]
    pub fn call_id(&self) -> &str {
        match self {
            Self::Running { call_id, .. } | Self::Settled { call_id, .. } => call_id,
        }
    }

    /// Optional call render intent.
    #[must_use]
    pub fn call_view(&self) -> Option<&Value> {
        match self {
            Self::Running { call_view, .. } | Self::Settled { call_view, .. } => call_view.as_ref(),
        }
    }

    /// Optional result render intent.
    #[must_use]
    pub fn result_view(&self) -> Option<&Value> {
        match self {
            Self::Running { .. } => None,
            Self::Settled { result_view, .. } => result_view.as_ref(),
        }
    }

    /// Whether this call is settled.
    #[must_use]
    pub const fn settled(&self) -> bool {
        matches!(self, Self::Settled { .. })
    }
}

/// Complete generic row model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolRowModel {
    /// Atomic row variant.
    pub variant: ToolRowVariant,
    /// Static or tool-refined title.
    pub title: String,
    /// One-line summary.
    pub summary: String,
    /// File path from arguments, when openable.
    pub file_path: Option<String>,
    /// Expanded input body.
    pub body: Option<String>,
    /// Flattened settled output.
    pub output: Option<String>,
    /// First output line on execution errors.
    pub error_summary: Option<String>,
    /// Lifecycle state.
    pub state: ToolRowState,
}

/// Classifies a tool name into its atomic row family.
#[must_use]
pub fn classify_tool(tool_name: &str) -> ToolRowVariant {
    match tool_name {
        "bash" | "pwsh" => ToolRowVariant::Bash,
        "read" | "web_fetch" | "cordis_package_inspect" | "cordis_runtime_inspect" => {
            ToolRowVariant::Read
        }
        "web_search" | "grep" | "glob" => ToolRowVariant::Search,
        "write" => ToolRowVariant::Write,
        "edit" => ToolRowVariant::Edit,
        "run_code" => ToolRowVariant::Code,
        _ => ToolRowVariant::Others,
    }
}

fn variant_title(variant: ToolRowVariant) -> &'static str {
    match variant {
        ToolRowVariant::Search => "Search",
        ToolRowVariant::Read => "Read",
        ToolRowVariant::Bash => "Bash",
        ToolRowVariant::Write => "Write",
        ToolRowVariant::Edit => "Edit",
        ToolRowVariant::Code => "Code",
        ToolRowVariant::Others => "Tool call",
    }
}

fn tool_title(tool_name: &str) -> Option<&'static str> {
    match tool_name {
        "cordis_package_inspect" | "cordis_runtime_inspect" => Some("Inspect"),
        "cordis_run" => Some("Run Cordis Plugin"),
        "cordis_stop" => Some("Stop Cordis Plugin"),
        "cordis_undefine" => Some("Remove Cordis Plugin"),
        "pwsh" => Some("Pwsh"),
        _ => None,
    }
}

/// Flattens settled content blocks to display text.
#[must_use]
pub fn result_text(block: &ToolCallBlock) -> String {
    let ToolCallBlock::Settled { content, error, .. } = block else {
        return String::new();
    };
    let mut parts = content
        .iter()
        .map(|block| {
            if block.get("type").and_then(Value::as_str) == Some("text") {
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            } else {
                serde_json::to_string_pretty(block).unwrap_or_else(|_| "null".to_owned())
            }
        })
        .collect::<Vec<_>>();
    if parts.is_empty()
        && let Some(error) = error
    {
        parts.push(format!("{}: {}", error.name, error.code));
    }
    parts.join("\n")
}

fn first_line(text: &str) -> &str {
    text.split_once('\n').map_or(text, |(first, _)| first)
}

fn argument_string<'a>(
    arguments: &'a serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<&'a str> {
    keys.iter().find_map(|key| {
        arguments
            .get(*key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    })
}

/// Removes a workspace root prefix for display only.
#[must_use]
pub fn relativize_to_cwd(text: &str, cwd: Option<&str>) -> String {
    let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty()) else {
        return text.to_owned();
    };
    let root = cwd.trim_end_matches(['/', '\\']);
    text.strip_prefix(root)
        .and_then(|suffix| suffix.strip_prefix(['/', '\\']))
        .unwrap_or(text)
        .to_owned()
}

fn summary_keys(variant: ToolRowVariant) -> &'static [&'static str] {
    match variant {
        ToolRowVariant::Bash => &["description", "command"],
        ToolRowVariant::Read => &["path", "file_path", "url"],
        ToolRowVariant::Search => &["query", "pattern", "url"],
        ToolRowVariant::Write | ToolRowVariant::Edit => &["path", "file_path"],
        ToolRowVariant::Code => &["description"],
        ToolRowVariant::Others => &[],
    }
}

fn raw_arguments(block: &ToolCallBlock) -> &str {
    match block {
        ToolCallBlock::Running { args_raw, .. } => args_raw,
        ToolCallBlock::Settled { call, .. } => call.as_ref().map_or("", |call| &call.args_raw),
    }
}

fn summary(variant: ToolRowVariant, raw: &str) -> String {
    let Ok(parsed) = serde_json::from_str::<Value>(raw) else {
        return first_line(raw).to_owned();
    };
    let preferred = parsed
        .as_object()
        .and_then(|arguments| argument_string(arguments, summary_keys(variant)));
    let fallback = match &parsed {
        Value::Object(arguments) => arguments.values().find_map(Value::as_str),
        Value::Array(arguments) => arguments.iter().find_map(Value::as_str),
        _ => None,
    }
    .filter(|value| !value.is_empty());
    preferred.or(fallback).map_or_else(
        || first_line(raw).to_owned(),
        |value| first_line(value).to_owned(),
    )
}

fn file_path(variant: ToolRowVariant, raw: &str) -> Option<String> {
    if !matches!(
        variant,
        ToolRowVariant::Read | ToolRowVariant::Write | ToolRowVariant::Edit
    ) {
        return None;
    }
    let Value::Object(arguments) = serde_json::from_str(raw).ok()? else {
        return None;
    };
    argument_string(&arguments, &["path", "file_path"]).map(|value| first_line(value).to_owned())
}

fn body(variant: ToolRowVariant, raw: &str) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    let Ok(parsed) = serde_json::from_str::<Value>(raw) else {
        return Some(raw.to_owned());
    };
    if variant == ToolRowVariant::Code
        && let Some(code) = parsed
            .get("code")
            .and_then(Value::as_str)
            .filter(|code| !code.is_empty())
    {
        return Some(code.to_owned());
    }
    serde_json::to_string_pretty(&parsed).ok()
}

/// Derives one complete generic row from a frozen call slice.
#[must_use]
pub fn tool_row_model(tool_name: &str, block: &ToolCallBlock, cwd: Option<&str>) -> ToolRowModel {
    let variant = classify_tool(tool_name);
    let raw = raw_arguments(block);
    let state = match block {
        ToolCallBlock::Running { .. } => ToolRowState::Running,
        ToolCallBlock::Settled {
            error: Some(error), ..
        } if error.code == "interrupted" => ToolRowState::Stopped,
        ToolCallBlock::Settled { is_error: true, .. } => ToolRowState::Error,
        ToolCallBlock::Settled { .. } => ToolRowState::Ok,
    };
    let base = if raw.is_empty() {
        block.call_id().to_owned()
    } else {
        relativize_to_cwd(&summary(variant, raw), cwd)
    };
    let owned_title = tool_title(tool_name);
    let summary =
        if variant == ToolRowVariant::Others && !tool_name.is_empty() && owned_title.is_none() {
            format!("{tool_name} · {base}")
        } else {
            base
        };
    let text = block
        .settled()
        .then(|| result_text(block))
        .filter(|text| !text.is_empty());
    let error_summary = (state == ToolRowState::Error)
        .then(|| text.as_deref().map(first_line).map(ToOwned::to_owned))
        .flatten();
    ToolRowModel {
        variant,
        title: owned_title
            .unwrap_or_else(|| variant_title(variant))
            .to_owned(),
        summary,
        file_path: file_path(variant, raw),
        body: body(variant, raw),
        output: text,
        error_summary,
        state,
    }
}
