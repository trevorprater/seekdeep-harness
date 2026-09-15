//! Pure read presentation: turn provider-decoded text into a bounded,
//! line-numbered window and a model-facing envelope.

use seekdeep_fs::{FsError, FsErrorCode};
use seekdeep_lossless_json::{JsonRef, JsonString, JsonValue};
use serde::{Deserialize, Serialize};

/// Default maximum UTF-16 code units returned for a single line.
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
    /// Maximum UTF-16 code units returned for a single line.
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
    pub text: JsonString,
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

fn truncate_line(line: &[u16], max_line_length: usize) -> JsonString {
    if line.len() > max_line_length {
        let mut text = JsonString::from_utf16(&line[..max_line_length]);
        text.push_str(&format!("... (line truncated to {max_line_length} chars)"));
        text
    } else {
        JsonString::from_utf16(line)
    }
}

fn line_byte_size(line: &JsonString, current_line_count: usize) -> usize {
    line.len_utf8() + usize::from(current_line_count > 0)
}

fn consume_line(acc: &mut WindowAccumulator, raw_line: &[u16], request: &ReadWindow) {
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
                "offset {} is out of range for \"{}\" ({} lines)",
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
    chunks: impl IntoIterator<Item = impl Into<JsonString>>,
    request: &ReadWindow,
    display_path: &str,
) -> anyhow::Result<WindowResult> {
    let mut builder = ReadWindowBuilder::new(request);
    for chunk in chunks {
        builder.push_units(chunk.into().utf16_units().iter().copied());
    }
    builder.finish(display_path)
}

pub(crate) struct ReadWindowBuilder<'a> {
    request: &'a ReadWindow,
    accumulator: WindowAccumulator,
    line_buffer: Vec<u16>,
}

impl<'a> ReadWindowBuilder<'a> {
    pub(crate) fn new(request: &'a ReadWindow) -> Self {
        Self {
            request,
            accumulator: WindowAccumulator::default(),
            line_buffer: Vec::new(),
        }
    }

    pub(crate) fn push_str(&mut self, chunk: &str) {
        self.push_units(chunk.encode_utf16());
    }

    fn push_units(&mut self, units: impl IntoIterator<Item = u16>) {
        // One unit beyond the cap proves overflow, including at a surrogate boundary.
        let cap = self.request.max_line_length.saturating_add(1);
        for unit in units {
            if unit == u16::from(b'\n') {
                self.flush_line();
            } else if self.line_buffer.len() < cap {
                self.line_buffer.push(unit);
            }
        }
    }

    fn flush_line(&mut self) {
        if self.line_buffer.last() == Some(&u16::from(b'\r')) {
            self.line_buffer.pop();
        }
        consume_line(&mut self.accumulator, &self.line_buffer, self.request);
        self.line_buffer.clear();
    }

    pub(crate) fn finish(mut self, display_path: &str) -> anyhow::Result<WindowResult> {
        if !self.line_buffer.is_empty() {
            self.flush_line();
        }
        finish(self.accumulator, self.request, display_path)
    }
}

/// Formats a read outcome as one OpenCode-style line-numbered text block body.
#[must_use]
pub fn format_read_output(display_path: &str, outcome: &FileReadOutcome) -> JsonString {
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
    let mut rendered = JsonString::from(format!(
        "<path>{display_path}</path>\n<type>file</type>\n<content>\n"
    ));
    for (index, line) in outcome.lines.iter().enumerate() {
        if index > 0 {
            rendered.push_str("\n");
        }
        rendered.push_str(&format!("{}: ", line.number));
        rendered.push_utf16(line.text.utf16_units());
    }
    if !outcome.lines.is_empty() {
        rendered.push_str("\n\n");
    }
    rendered.push_str(&footer);
    rendered.push_str("\n</content>");
    rendered
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
    let ext_lower = ext.to_lowercase();
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
    pub path: JsonString,
    /// The 1-based first line the window requested.
    pub offset: u64,
    /// The returned window's lines.
    pub lines: Vec<FileTextLine>,
    /// Exact total line count in the file.
    pub total_lines: u64,
    /// Syntax-highlighting language hint, or omitted for plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<JsonString>,
}

/// Narrows opaque live or replayed result metadata to a structured read window.
#[must_use]
pub fn read_meta_from_meta(meta: &JsonValue) -> Option<FsReadMeta> {
    let path = meta.get("path")?.deserialize().ok()?;
    let offset = json_line_number(meta.get("offset")?)?;
    if offset < 1 {
        return None;
    }
    let total_lines = json_line_number(meta.get("totalLines")?)?;
    let lines = meta.get("lines")?.array_items()?;
    let lang = meta
        .get("lang")
        .map(JsonRef::deserialize::<JsonString>)
        .transpose()
        .ok()?;
    let mut previous = offset - 1;
    let mut decoded = Vec::with_capacity(lines.len());
    for line in lines {
        let number = json_line_number(line.get("number")?)?;
        let text = line.get("text")?.deserialize().ok()?;
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

fn json_line_number(value: JsonRef<'_>) -> Option<u64> {
    let number = value.as_f64()?;
    if number.fract() != 0.0 || !(0.0..18_446_744_073_709_551_616.0).contains(&number) {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(number as u64)
}

#[cfg(test)]
mod tests {
    #[test]
    fn line_caps_count_utf16_units_and_preserve_a_split_surrogate() {
        let window = super::ReadWindow {
            offset: 1,
            limit: 10,
            max_line_length: 1000,
            max_bytes: usize::MAX,
        };
        let accented = "\u{e9}".repeat(1500);
        let result = super::build_window(vec![accented], &window, "accents.txt").unwrap();
        assert_eq!(
            result.lines[0].text,
            format!(
                "{}... (line truncated to 1000 chars)",
                "\u{e9}".repeat(1000)
            )
        );
        let exact = "\u{e9}".repeat(1000);
        let result = super::build_window(vec![exact.clone()], &window, "exact.txt").unwrap();
        assert_eq!(result.lines[0].text, exact);
        // Two-unit characters fill an even cap exactly.
        let emoji = "\u{1F600}".repeat(600);
        let result = super::build_window(vec![emoji], &window, "emoji.txt").unwrap();
        assert_eq!(
            result.lines[0].text,
            format!(
                "{}... (line truncated to 1000 chars)",
                "\u{1F600}".repeat(500)
            )
        );
        let mixed = format!("{}\u{1F600}", "a".repeat(999));
        let result = super::build_window(vec![mixed], &window, "mixed.txt").unwrap();
        let mut expected = JsonString::from("a".repeat(999));
        expected.push_utf16(&[0xd83d]);
        expected.push_str("... (line truncated to 1000 chars)");
        assert_eq!(result.lines[0].text, expected);
    }

    use super::*;

    #[test]
    fn read_meta_ignores_raw_extensions_and_uses_final_properties() {
        let meta = JsonValue::parse(
            r#"{"path":"old","path":"a.txt","offset":1e0,"lines":[{"number":1.0,"text":"before","text":"after","\ud800":"\udfff"}],"totalLines":1e0,"\udfff":{"value":"\ud800"}}"#.to_owned(),
        ).unwrap();
        let read = read_meta_from_meta(&meta).unwrap();
        assert_eq!(read.path, "a.txt");
        assert_eq!(
            read.lines,
            vec![FileTextLine {
                number: 1,
                text: "after".into()
            }]
        );
    }

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
            [(2, Some("two")), (3, Some("three"))]
        );
    }

    #[test]
    fn window_rejects_out_of_range_offset() {
        let err = build_window(["one\n".to_owned()], &window(5, 10), "/f").expect_err("range");
        let fs = err.downcast::<FsError>().expect("FsError");
        assert_eq!(fs.code, FsErrorCode::FsNotFound);
    }

    #[test]
    fn long_multibyte_lines_do_not_split_a_character() {
        // The buffered line cap lands on a byte offset that can fall inside a
        // multi-byte character; truncating there must not panic or split it.
        let line = "\u{e9}".repeat(4000);
        let result = build_window(
            [format!("{line}\n")],
            &ReadWindow {
                offset: 1,
                limit: 10,
                max_line_length: 2000,
                max_bytes: 50 * 1024,
            },
            "/f",
        )
        .expect("window");
        assert_eq!(
            result.lines[0].text,
            format!(
                "{}... (line truncated to 2000 chars)",
                "\u{e9}".repeat(2000)
            )
        );
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
                text: "hello".into(),
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

    #[test]
    fn incremental_window_retains_only_the_capped_line_prefix() {
        let request = window(1, 1);
        let mut builder = ReadWindowBuilder::new(&request);
        let chunk = "x".repeat(8192);
        for _ in 0..2048 {
            builder.push_str(&chunk);
            assert_eq!(builder.line_buffer.len(), request.max_line_length + 1);
            assert!(builder.accumulator.lines.is_empty());
        }
        builder.push_str("\nsecond\nthird");
        let result = builder.finish("large.txt").unwrap();
        assert_eq!(result.total_lines, 3);
        assert_eq!(result.lines.len(), 1);
        assert_eq!(
            result.lines[0].text,
            format!("{}... (line truncated to 2000 chars)", "x".repeat(2000))
        );
    }

    #[test]
    fn carriage_return_is_stripped_after_capping_the_line_buffer() {
        let request = ReadWindow {
            max_line_length: 3,
            ..window(1, 10)
        };
        for chunks in [vec!["abc\rdef\n"], vec!["abc", "\r", "def", "\n"]] {
            let result = build_window(chunks, &request, "cr.txt").unwrap();
            assert_eq!(result.lines[0].text, "abc");
        }
    }

    #[test]
    fn metadata_keeps_arbitrary_utf16_strings() {
        let meta = JsonValue::parse(
            r#"{"path":"\ud800.rs","offset":1,"lines":[{"number":1,"text":"\udfff"}],"totalLines":1,"lang":"\ud800"}"#.to_owned(),
        ).unwrap();
        let read = read_meta_from_meta(&meta).unwrap();
        assert_eq!(JsonValue::from_serialize(&read).unwrap(), meta);
    }
}
