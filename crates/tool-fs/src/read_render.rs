//! Pure read presentation: turn provider-decoded text into a bounded,
//! line-numbered window and a model-facing envelope.

use seekdeep_fs::{FsError, FsErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Default maximum characters returned for a single line.
pub const READ_MAX_LINE_LENGTH: usize = 2000;

/// Default maximum bytes returned for selected file lines.
pub const READ_MAX_BYTES: usize = 50 * 1024;

/// Resolved read window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadWindow {
    /// 1-based first line to return.
    pub offset: u64,
    /// Maximum number of lines to return.
    pub limit: usize,
    /// Maximum characters returned for a single line.
    pub max_line_length: usize,
    /// Maximum bytes of selected output.
    pub max_bytes: usize,
}

/// One line returned from a text file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTextLine {
    /// 1-based line number in the file.
    pub number: u64,
    /// Line text without its trailing newline.
    pub text: String,
}

/// The windowed result a build produces from a file's decoded text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowResult {
    /// Returned lines, already numbered.
    pub lines: Vec<FileTextLine>,
    /// Exact total line count in the file.
    pub total_lines: u64,
    /// Whether selected output hit the byte cap.
    pub truncated_by_bytes: bool,
}

/// Outcome of a bounded text read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileReadOutcome {
    /// 1-based first line requested.
    pub offset: u64,
    /// Returned lines, already numbered.
    pub lines: Vec<FileTextLine>,
    /// Exact total line count in the file.
    pub total_lines: u64,
    /// Whether selected output hit the byte cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated_by_bytes: Option<bool>,
}

#[derive(Default)]
struct WindowAccumulator {
    lines: Vec<FileTextLine>,
    total_lines: u64,
    output_bytes: usize,
    truncated_by_bytes: bool,
}

fn truncate_line(line: &str, max_line_length: usize) -> String {
    if line.chars().count() > max_line_length {
        format!(
            "{}... (line truncated to {max_line_length} chars)",
            line.chars().take(max_line_length).collect::<String>()
        )
    } else {
        line.to_owned()
    }
}

fn line_byte_size(line: &str, current_line_count: usize) -> usize {
    line.len() + usize::from(current_line_count > 0)
}

fn consume_line(acc: &mut WindowAccumulator, raw_line: &str, request: &ReadWindow) {
    acc.total_lines += 1;
    if acc.truncated_by_bytes
        || acc.total_lines < request.offset
        || acc.lines.len() >= request.limit
    {
        return;
    }
    let text = truncate_line(raw_line, request.max_line_length);
    let bytes = line_byte_size(&text, acc.lines.len());
    if acc.output_bytes + bytes > request.max_bytes {
        acc.truncated_by_bytes = true;
        return;
    }
    acc.output_bytes += bytes;
    acc.lines.push(FileTextLine {
        number: acc.total_lines,
        text,
    });
}

fn strip_carriage_return(line: &str) -> &str {
    line.strip_suffix('\r').unwrap_or(line)
}

fn finish(
    acc: WindowAccumulator,
    request: &ReadWindow,
    display_path: &str,
) -> anyhow::Result<WindowResult> {
    if !acc.truncated_by_bytes
        && request.offset > acc.total_lines
        && !(acc.total_lines == 0 && request.offset == 1)
    {
        return Err(FsError::new(
            format!(
                "offset {} is out of range for {:?} ({} lines)",
                request.offset, display_path, acc.total_lines
            ),
            FsErrorCode::FsNotFound,
        )
        .into());
    }
    Ok(WindowResult {
        lines: acc.lines,
        total_lines: acc.total_lines,
        truncated_by_bytes: acc.truncated_by_bytes,
    })
}

/// Builds one window from ordered chunks, enforcing line and byte caps while
/// still scanning to an exact total line count.
///
/// # Errors
///
/// Returns an out-of-range offset failure.
pub fn build_window(
    chunks: impl IntoIterator<Item = String>,
    request: &ReadWindow,
    display_path: &str,
) -> anyhow::Result<WindowResult> {
    let mut acc = WindowAccumulator::default();
    let line_buffer_cap = request.max_line_length + 1;
    let mut line_buffer = String::new();

    for chunk in chunks {
        let mut start = 0;
        while let Some(relative) = chunk[start..].find('\n') {
            let newline = start + relative;
            append_to_line_buffer(&mut line_buffer, &chunk[start..newline], line_buffer_cap);
            let raw = strip_carriage_return(&line_buffer).to_owned();
            consume_line(&mut acc, &raw, request);
            line_buffer.clear();
            start = newline + 1;
        }
        append_to_line_buffer(&mut line_buffer, &chunk[start..], line_buffer_cap);
    }
    if !line_buffer.is_empty() {
        let raw = strip_carriage_return(&line_buffer).to_owned();
        consume_line(&mut acc, &raw, request);
    }
    finish(acc, request, display_path)
}

fn append_to_line_buffer(buffer: &mut String, segment: &str, cap: usize) {
    if buffer.len() >= cap {
        return;
    }
    buffer.push_str(segment);
    if buffer.len() > cap {
        buffer.truncate(cap);
    }
}

/// Formats a read outcome as one OpenCode-style line-numbered text block body.
#[must_use]
pub fn format_read_output(display_path: &str, outcome: &FileReadOutcome) -> String {
    let end_line = outcome
        .lines
        .last()
        .map_or_else(|| outcome.offset.saturating_sub(1), |line| line.number);
    let footer = if outcome.truncated_by_bytes == Some(true) {
        format!(
            "(Output capped. Showing lines {}-{end_line}. Use offset={} to continue.)",
            outcome.offset,
            end_line + 1
        )
    } else if end_line < outcome.total_lines {
        format!(
            "(Showing lines {}-{end_line} of {}. Use offset={} to continue.)",
            outcome.offset,
            outcome.total_lines,
            end_line + 1
        )
    } else {
        format!("(End of file - total {} lines)", outcome.total_lines)
    };
    let body = if outcome.lines.is_empty() {
        footer
    } else {
        let numbered = outcome
            .lines
            .iter()
            .map(|line| format!("{}: {}", line.number, line.text))
            .collect::<Vec<_>>()
            .join("\n");
        format!("{numbered}\n\n{footer}")
    };
    format!("<path>{display_path}</path>\n<type>file</type>\n<content>\n{body}\n</content>")
}

/// Lowercased file-extension to syntax-highlighting language hint.
#[must_use]
pub fn lang_from_path(path: &str) -> Option<&'static str> {
    let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let dot = base.rfind('.')?;
    if dot == 0 {
        return None;
    }
    let ext = &base[dot + 1..];
    let ext_lower = ext.to_ascii_lowercase();
    let hint = match ext_lower.as_str() {
        "ts" | "mts" | "cts" => "ts",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" => "js",
        "jsx" => "jsx",
        "json" | "jsonc" => "json",
        "py" => "py",
        "rb" => "rb",
        "go" => "go",
        "rs" => "rs",
        "java" => "java",
        "c" | "h" => "c",
        "cc" | "cpp" | "hpp" | "cxx" => "cpp",
        "cs" => "cs",
        "kt" => "kotlin",
        "swift" => "swift",
        "php" => "php",
        "sh" | "bash" | "zsh" => "sh",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "ini" => "ini",
        "md" | "markdown" => "md",
        "mdx" => "mdx",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" => "scss",
        "less" => "less",
        "sql" => "sql",
        "xml" => "xml",
        "lua" => "lua",
        _ => return None,
    };
    Some(hint)
}

/// The read tool's private result meta payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsReadMeta {
    /// The read file's model-facing path.
    pub path: String,
    /// The 1-based first line the window requested.
    pub offset: u64,
    /// The returned window's lines.
    pub lines: Vec<FileTextLine>,
    /// Exact total line count in the file.
    pub total_lines: u64,
    /// Syntax-highlighting language hint, or omitted for plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
}

fn is_file_text_line(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let number = object
        .get("number")
        .and_then(Value::as_u64)
        .is_some_and(|number| number >= 1);
    let text = object.get("text").is_some_and(Value::is_string);
    number && text
}

/// Narrows opaque live or replayed result metadata to a structured read window.
#[must_use]
pub fn read_meta_from_meta(meta: &Value) -> Option<FsReadMeta> {
    let object = meta.as_object()?;
    let path = object.get("path")?.as_str()?.to_owned();
    let offset = object.get("offset")?.as_u64()?;
    if offset < 1 {
        return None;
    }
    let total_lines = object.get("totalLines")?.as_u64()?;
    let lines = object.get("lines")?.as_array()?;
    if !lines.iter().all(is_file_text_line) {
        return None;
    }
    let lang = object.get("lang").map_or(Some(None), |value| {
        value.as_str().map(|s| Some(s.to_owned()))
    })?;
    let mut previous = offset - 1;
    let mut decoded = Vec::with_capacity(lines.len());
    for line in lines {
        let number = line.get("number")?.as_u64()?;
        let text = line.get("text")?.as_str()?.to_owned();
        if number <= previous || number > total_lines {
            return None;
        }
        previous = number;
        decoded.push(FileTextLine { number, text });
    }
    Some(FsReadMeta {
        path,
        offset,
        lines: decoded,
        total_lines,
        lang,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(offset: u64, limit: usize) -> ReadWindow {
        ReadWindow {
            offset,
            limit,
            max_line_length: 2000,
            max_bytes: 50 * 1024,
        }
    }

    #[test]
    fn window_numbers_and_bounds_lines() {
        let result = build_window(
            ["one\n".to_owned(), "two\nthree\nfour\n".to_owned()],
            &window(2, 2),
            "/f",
        )
        .expect("window");
        assert_eq!(result.total_lines, 4);
        assert_eq!(
            result
                .lines
                .iter()
                .map(|line| (line.number, line.text.as_str()))
                .collect::<Vec<_>>(),
            [(2, "two"), (3, "three")]
        );
    }

    #[test]
    fn window_rejects_out_of_range_offset() {
        let err = build_window(["one\n".to_owned()], &window(5, 10), "/f").expect_err("range");
        let fs = err.downcast::<FsError>().expect("FsError");
        assert_eq!(fs.code, FsErrorCode::FsNotFound);
    }

    #[test]
    fn window_caps_bytes() {
        let result = build_window(
            ["aaaa\nbbbb\n".to_owned()],
            &ReadWindow {
                offset: 1,
                limit: 10,
                max_line_length: 2000,
                max_bytes: 6,
            },
            "/f",
        )
        .expect("window");
        assert!(result.truncated_by_bytes);
        assert_eq!(result.lines.len(), 1);
    }

    #[test]
    fn truncates_long_lines() {
        let result = build_window(
            ["abcdef\n".to_owned()],
            &ReadWindow {
                offset: 1,
                limit: 10,
                max_line_length: 3,
                max_bytes: 50 * 1024,
            },
            "/f",
        )
        .expect("window");
        assert_eq!(result.lines[0].text, "abc... (line truncated to 3 chars)");
    }

    #[test]
    fn format_read_output_renders_envelope_and_footer() {
        let outcome = FileReadOutcome {
            offset: 1,
            lines: vec![FileTextLine {
                number: 1,
                text: "hello".to_owned(),
            }],
            total_lines: 1,
            truncated_by_bytes: None,
        };
        let rendered = format_read_output("/f", &outcome);
        assert!(rendered.contains("<path>/f</path>"));
        assert!(rendered.contains("1: hello"));
        assert!(rendered.contains("(End of file - total 1 lines)"));
    }

    #[test]
    fn lang_from_path_maps_known_extensions() {
        assert_eq!(lang_from_path("a/b/c.rs"), Some("rs"));
        assert_eq!(lang_from_path("a/b.tsx"), Some("tsx"));
        assert_eq!(lang_from_path(".gitignore"), None);
        assert_eq!(lang_from_path("noext"), None);
    }
}
