//! Pure model-argument validation, URI display, result caps, and call presentation.

use indexmap::IndexMap;
use percent_encoding::percent_decode_str;
use seekdeep_lsp::{LspHover, LspLocation, LspOperation, LspPosition};
use seekdeep_tools::{FileLocation, GenericCallView, ToolCallKind, ToolCallView};
use serde::{Deserialize, Serialize};
use url::Url;

/// Closed operation spellings exposed by the model schema.
pub const LSP_OPERATIONS: [&str; 4] = [
    "goToDefinition",
    "findReferences",
    "goToImplementation",
    "hover",
];
/// Default rendered-location count cap.
pub const DEFAULT_MAX_LOCATIONS: usize = 100;
/// Default complete rendered-result UTF-16 character cap.
pub const DEFAULT_MAX_RESULT_CHARS: usize = 16_000;

/// Raw schema-typed model arguments.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LspToolArgs {
    /// Requested operation spelling.
    pub operation: String,
    /// Source file path.
    pub file_path: String,
    /// One-based source line.
    pub line: f64,
    /// One-based UTF-16 column.
    pub character: f64,
}

/// Validated arguments with a zero-based protocol position.
#[derive(Clone, Debug, PartialEq)]
pub struct LspToolInput {
    /// Closed semantic operation.
    pub operation: LspOperation,
    /// Source file path.
    pub file_path: String,
    /// Zero-based UTF-16 position.
    pub position: LspPosition,
}

/// Validates arguments and converts one-based coordinates to zero-based.
///
/// # Errors
///
/// Rejects unknown operations, blank paths, and non-positive or fractional coordinates.
pub fn parse_lsp_args(args: &LspToolArgs) -> anyhow::Result<LspToolInput> {
    let operation = match args.operation.as_str() {
        "goToDefinition" => LspOperation::GoToDefinition,
        "findReferences" => LspOperation::FindReferences,
        "goToImplementation" => LspOperation::GoToImplementation,
        "hover" => LspOperation::Hover,
        _ => anyhow::bail!("operation must be one of {}", LSP_OPERATIONS.join(", ")),
    };
    anyhow::ensure!(
        !args.file_path.trim().is_empty(),
        "file_path must be a non-empty string"
    );
    let line = one_based(args.line, "line")?;
    let character = one_based(args.character, "character")?;
    Ok(LspToolInput {
        operation,
        file_path: args.file_path.clone(),
        position: LspPosition {
            line: line - 1.0,
            character: character - 1.0,
        },
    })
}

fn one_based(value: f64, name: &str) -> anyhow::Result<f64> {
    anyhow::ensure!(
        value.is_finite() && value.fract() == 0.0 && value >= 1.0,
        "{name} must be a positive integer (one-based)"
    );
    Ok(value)
}

/// Renders locations grouped by display path with one-based coordinates and caps.
#[must_use]
pub fn format_locations(
    locations: &[LspLocation],
    workspace_uri: &str,
    max_locations: usize,
    max_result_chars: usize,
) -> String {
    if locations.is_empty() {
        return bound_result("No results.", max_result_chars, "locations");
    }
    let shown = &locations[..locations.len().min(max_locations)];
    let omitted = locations.len() - shown.len();
    let mut grouped = IndexMap::<String, Vec<String>>::new();
    for location in shown {
        let path = render_uri(&location.uri, workspace_uri);
        let line = js_number(location.range.start.line + 1.0);
        let character = js_number(location.range.start.character + 1.0);
        grouped
            .entry(path.clone())
            .or_default()
            .push(format!("{path}:{line}:{character}"));
    }
    let mut lines = grouped.into_values().flatten().collect::<Vec<_>>();
    if omitted > 0 {
        let noun = if omitted == 1 {
            "location"
        } else {
            "locations"
        };
        lines.push(format!(
            "… {omitted} more {noun} omitted (limit {max_locations})."
        ));
    }
    bound_result(&lines.join("\n"), max_result_chars, "locations")
}

/// Renders normalized hover content with a complete-result cap.
#[must_use]
pub fn format_hover(hover: Option<&LspHover>, max_result_chars: usize) -> String {
    let text = hover.map_or("No hover information.", |hover| hover.contents.as_str());
    bound_result(text, max_result_chars, "hover")
}

fn bound_result(text: &str, max_chars: usize, label: &str) -> String {
    if utf16_len(text) <= max_chars {
        return text.to_owned();
    }
    let notice = format!("\n… {label} truncated (limit {max_chars} characters).");
    let notice_len = utf16_len(&notice);
    if notice_len >= max_chars {
        return utf16_prefix(&notice, max_chars);
    }
    format!("{}{}", utf16_prefix(text, max_chars - notice_len), notice)
}

fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

fn utf16_prefix(value: &str, units: usize) -> String {
    let encoded = value.encode_utf16().take(units).collect::<Vec<_>>();
    String::from_utf16_lossy(&encoded)
}

fn js_number(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    if value.is_finite() && value.fract() == 0.0 {
        return format!("{value:.0}");
    }
    value.to_string()
}

/// Converts a provider-world file URI to a relative or absolute display path.
#[must_use]
pub fn render_uri(uri: &str, workspace_uri: &str) -> String {
    if !uri.starts_with("file:") {
        return uri.to_owned();
    }
    let (Ok(target), Ok(workspace)) = (Url::parse(uri), Url::parse(workspace_uri)) else {
        return uri.to_owned();
    };
    if workspace.scheme() != "file"
        || !valid_percent_path(target.path())
        || !valid_percent_path(workspace.path())
    {
        return uri.to_owned();
    }
    let windows_world = is_windows_file_url(&workspace);
    let target_windows_world = windows_world && is_windows_file_url(&target);
    let Some(workspace_path) = file_path(&workspace, windows_world) else {
        return uri.to_owned();
    };
    let Some(target_path) = file_path(&target, target_windows_world) else {
        return uri.to_owned();
    };
    if windows_world != target_windows_world {
        return target_path;
    }
    let relative = if windows_world {
        relative_windows(&workspace_path, &target_path)
    } else {
        relative_posix(&workspace_path, &target_path)
    };
    let outside = relative == ".."
        || relative.starts_with("../")
        || relative.starts_with("..\\")
        || is_absolute_windows(&relative)
        || relative.starts_with('/');
    let rendered = if relative.is_empty() {
        ".".to_owned()
    } else if outside {
        target_path
    } else {
        relative
    };
    if windows_world {
        rendered.replace('\\', "/")
    } else {
        rendered
    }
}

fn is_windows_file_url(url: &Url) -> bool {
    url.host_str().is_some_and(|host| !host.is_empty()) || is_drive_path(url.path())
}

fn is_drive_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && (bytes[2] == b':'
            || path
                .get(2..5)
                .is_some_and(|value| value.eq_ignore_ascii_case("%3a")))
}

fn valid_percent_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return false;
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    true
}

fn file_path(url: &Url, windows: bool) -> Option<String> {
    let lower = url.path().to_ascii_lowercase();
    if lower.contains("%2f") || (windows && lower.contains("%5c")) {
        return None;
    }
    let decoded = percent_decode_str(url.path())
        .decode_utf8()
        .ok()?
        .into_owned();
    if decoded.contains('\0') {
        return None;
    }
    if !windows {
        return Some(decoded);
    }
    if let Some(host) = url.host_str().filter(|host| !host.is_empty()) {
        return Some(format!("\\\\{}{}", host, decoded.replace('/', "\\")));
    }
    if !is_drive_path(url.path()) {
        return None;
    }
    Some(decoded.trim_start_matches('/').replace('/', "\\"))
}

fn relative_posix(from: &str, to: &str) -> String {
    relative_components(from, to, false)
}

fn relative_windows(from: &str, to: &str) -> String {
    let (from_root, from_parts) = windows_parts(from);
    let (to_root, to_parts) = windows_parts(to);
    if !from_root.eq_ignore_ascii_case(&to_root) {
        return to.to_owned();
    }
    relative_parts(&from_parts, &to_parts, true).join("\\")
}

fn relative_components(from: &str, to: &str, insensitive: bool) -> String {
    let from = from
        .split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let to = to
        .split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    relative_parts(&from, &to, insensitive).join("/")
}

fn windows_parts(path: &str) -> (String, Vec<String>) {
    if let Some(rest) = path.strip_prefix("\\\\") {
        let mut parts = rest
            .split('\\')
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let root_len = parts.len().min(2);
        let root = format!("//{}", parts[..root_len].join("/"));
        return (root, parts.split_off(root_len));
    }
    let root = path.get(..2).unwrap_or(path).to_owned();
    let parts = path
        .get(2..)
        .unwrap_or_default()
        .split('\\')
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect();
    (root, parts)
}

fn relative_parts(from: &[String], to: &[String], insensitive: bool) -> Vec<String> {
    let equal = |left: &str, right: &str| {
        if insensitive {
            left.eq_ignore_ascii_case(right)
        } else {
            left == right
        }
    };
    let common = from
        .iter()
        .zip(to)
        .take_while(|(left, right)| equal(left, right))
        .count();
    std::iter::repeat_n("..".to_owned(), from.len() - common)
        .chain(to[common..].iter().cloned())
        .collect()
}

fn is_absolute_windows(path: &str) -> bool {
    path.starts_with("\\\\")
        || path
            .as_bytes()
            .get(1)
            .is_some_and(|separator| *separator == b':')
}

/// Builds the replay-safe generic search presentation for a pending call.
#[must_use]
pub fn present_lsp_call(args: &LspToolArgs) -> ToolCallView {
    ToolCallView::Generic(GenericCallView {
        title: format!(
            "LSP {} {}:{}:{}",
            args.operation,
            args.file_path,
            js_number(args.line),
            js_number(args.character)
        ),
        kind: Some(ToolCallKind::Search),
        raw_input: None,
        content: None,
        locations: Some(vec![FileLocation {
            path: args.file_path.clone(),
            line: Some(args.line),
        }]),
    })
}
