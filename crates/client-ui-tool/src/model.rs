//! Generic tool-row model derivation from frozen wire slices.

use seekdeep_lossless_json::{JsonString, JsonValue};

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
    pub args_raw: JsonString,
}

/// Running or settled tool lifecycle.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolCallBlock {
    /// Call has not settled.
    Running {
        /// Stable call id.
        call_id: String,
        /// Raw JSON arguments.
        args_raw: JsonString,
        /// Optional call-time render intent.
        call_view: Option<JsonValue>,
    },
    /// Result has settled.
    Settled {
        /// Stable call id.
        call_id: String,
        /// Retained call head, absent after window truncation.
        call: Option<ToolCallHead>,
        /// Retained call-time render intent.
        call_view: Option<JsonValue>,
        /// Result-time render intent.
        result_view: Option<JsonValue>,
        /// Result content blocks.
        content: Vec<JsonValue>,
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
    pub fn call_view(&self) -> Option<&JsonValue> {
        match self {
            Self::Running { call_view, .. } | Self::Settled { call_view, .. } => call_view.as_ref(),
        }
    }

    /// Optional result render intent.
    #[must_use]
    pub fn result_view(&self) -> Option<&JsonValue> {
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
    pub summary: JsonString,
    /// File path from arguments, when openable.
    pub file_path: Option<JsonString>,
    /// Expanded input body.
    pub body: Option<JsonString>,
    /// Flattened settled output.
    pub output: Option<JsonString>,
    /// First output line on execution errors.
    pub error_summary: Option<JsonString>,
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
pub fn result_text(block: &ToolCallBlock) -> JsonString {
    let ToolCallBlock::Settled { content, error, .. } = block else {
        return JsonString::default();
    };
    let mut parts = content
        .iter()
        .map(|block| {
            if block.get_value("type").and_then(JsonValue::as_str) == Some("text") {
                block
                    .get_value("text")
                    .and_then(json_string)
                    .unwrap_or_default()
            } else {
                block.stringify_pretty().into()
            }
        })
        .collect::<Vec<_>>();
    if parts.is_empty()
        && let Some(error) = error
    {
        parts.push(format!("{}: {}", error.name, error.code).into());
    }
    JsonString::join(&parts, "\n")
}

fn first_line(text: &JsonString) -> JsonString {
    let units = text.utf16_units();
    let end = units
        .iter()
        .position(|unit| *unit == u16::from(b'\n'))
        .unwrap_or(units.len());
    JsonString::from_utf16(&units[..end])
}

pub(crate) fn json_string(value: &JsonValue) -> Option<JsonString> {
    value.to_utf16().map(|units| JsonString::from_utf16(&units))
}

pub(crate) fn parse_arguments(raw: &JsonString) -> Option<JsonValue> {
    JsonValue::parse_text(raw).ok()
}

fn argument_string(arguments: &JsonValue, keys: &[&str]) -> Option<JsonString> {
    keys.iter().find_map(|key| {
        arguments
            .get_value(key)
            .and_then(json_string)
            .filter(|value| !value.is_empty())
    })
}

fn relativize_json_to_cwd(text: &JsonString, cwd: Option<&str>) -> JsonString {
    let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty()) else {
        return text.clone();
    };
    let root = cwd.trim_end_matches(['/', '\\']);
    let units = text.utf16_units();
    let root = root.encode_utf16().collect::<Vec<_>>();
    if units.starts_with(&root)
        && units
            .get(root.len())
            .is_some_and(|unit| matches!(*unit, 0x2f | 0x5c))
    {
        JsonString::from_utf16(&units[root.len() + 1..])
    } else {
        text.clone()
    }
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

fn raw_arguments(block: &ToolCallBlock) -> JsonString {
    match block {
        ToolCallBlock::Running { args_raw, .. } => args_raw.clone(),
        ToolCallBlock::Settled { call, .. } => call
            .as_ref()
            .map(|call| call.args_raw.clone())
            .unwrap_or_default(),
    }
}

fn summary(variant: ToolRowVariant, raw: &JsonString) -> JsonString {
    let Some(parsed) = parse_arguments(raw) else {
        return first_line(raw);
    };
    let parsed = JsonValue::parse(parsed.stringify()).expect("stringified JSON is valid");
    let preferred = argument_string(&parsed, summary_keys(variant));
    let fallback = parsed
        .object_entries()
        .map(|entries| {
            entries
                .into_iter()
                .map(|(_, value)| value)
                .collect::<Vec<_>>()
        })
        .or_else(|| parsed.array_items())
        .and_then(|values| {
            values
                .into_iter()
                .filter_map(seekdeep_lossless_json::JsonRef::to_utf16)
                .find(|units| !units.is_empty())
        })
        .map(|units| JsonString::from_utf16(&units));
    preferred
        .or(fallback)
        .as_ref()
        .map_or_else(|| first_line(raw), first_line)
}

fn file_path(variant: ToolRowVariant, raw: &JsonString) -> Option<JsonString> {
    if !matches!(
        variant,
        ToolRowVariant::Read | ToolRowVariant::Write | ToolRowVariant::Edit
    ) {
        return None;
    }
    let arguments = parse_arguments(raw)?;
    argument_string(&arguments, &["path", "file_path"])
        .as_ref()
        .map(first_line)
}

fn body(variant: ToolRowVariant, raw: &JsonString) -> Option<JsonString> {
    if raw.is_empty() {
        return None;
    }
    let Some(parsed) = parse_arguments(raw) else {
        return Some(raw.clone());
    };
    if variant == ToolRowVariant::Code
        && let Some(code) = parsed
            .get_value("code")
            .and_then(json_string)
            .filter(|code| !code.is_empty())
    {
        return Some(code);
    }
    Some(parsed.stringify_pretty().into())
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
        block.call_id().into()
    } else {
        relativize_json_to_cwd(&summary(variant, &raw), cwd)
    };
    let owned_title = tool_title(tool_name);
    let summary =
        if variant == ToolRowVariant::Others && !tool_name.is_empty() && owned_title.is_none() {
            let mut summary = JsonString::from(format!("{tool_name} · "));
            summary.push_utf16(base.utf16_units());
            summary
        } else {
            base
        };
    let text = block
        .settled()
        .then(|| result_text(block))
        .filter(|text| !text.is_empty());
    let error_summary = (state == ToolRowState::Error)
        .then(|| text.as_ref().map(first_line))
        .flatten();
    ToolRowModel {
        variant,
        title: owned_title
            .unwrap_or_else(|| variant_title(variant))
            .to_owned(),
        summary,
        file_path: file_path(variant, &raw),
        body: body(variant, &raw),
        output: text,
        error_summary,
        state,
    }
}
