//! Source-compatible HMR build diagnostics and Babel code frames.
//!
//! The token expression follows js-tokens 4 and the color/frame rules follow
//! @babel/code-frame 7; their licenses are in this crate's `licenses` directory.

use std::{fmt::Write as _, io::IsTerminal as _, path::Path, sync::LazyLock};

use regress::Regex;
use serde_json::{Value, json};

/// A source location carried by an esbuild diagnostic.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceLocation {
    /// File as reported by the compiler, relative to its working directory.
    pub file: String,
    /// One-based source line.
    pub line: i64,
    /// Column passed unchanged to the source code-frame renderer.
    pub column: i64,
}

/// One compiler message that can be formatted without losing its location.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BuildDiagnostic {
    /// Compiler-provided diagnostic text.
    pub text: String,
    /// Optional source location.
    pub location: Option<SourceLocation>,
}

/// An import failure with the esbuild-compatible structured error payload.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct BuildFailure {
    /// Ordered compiler messages.
    pub errors: Vec<BuildDiagnostic>,
}

impl std::fmt::Display for BuildFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, error) in self.errors.iter().enumerate() {
            if index != 0 {
                formatter.write_str("\n")?;
            }
            formatter.write_str(&error.text)?;
        }
        Ok(())
    }
}

impl std::error::Error for BuildFailure {}

/// Logs a Loader failure using captured compiler fields when available.
///
/// # Errors
///
/// Returns the malformed-input failure from [`handle_error`].
pub fn handle_loader_error(
    context: &seekdeep_cordis::Context,
    error: &seekdeep_loader::LoaderError,
) -> anyhow::Result<()> {
    match error.structured_error() {
        Some(error) => handle_error(context, error),
        None => handle_error(context, &Value::String(error.to_string())),
    }
}

/// Logs every error in the source order, preserving unstructured errors.
/// Each warning is emitted before reading the next diagnostic's source file.
///
/// # Errors
///
/// A null member in an `errors` array raises the same malformed-input failure
/// as the source's direct `error.text` access.
pub fn handle_error(context: &seekdeep_cordis::Context, error: &Value) -> anyhow::Result<()> {
    match build_errors(error)? {
        Some(errors) => {
            let color = color_supported();
            for error in errors {
                context.logger(None).warn([format_diagnostic(error, color)]);
            }
        }
        None => context.logger(None).warn([error.clone()]),
    }
    Ok(())
}

/// Formats warning arguments emitted by the source HMR error handler.
///
/// # Errors
///
/// Returns an error when a compiler error array contains null.
pub fn format_error(error: &Value, color: bool) -> anyhow::Result<Vec<Value>> {
    Ok(match build_errors(error)? {
        Some(errors) => errors
            .iter()
            .map(|error| format_diagnostic(error, color))
            .collect(),
        None => vec![error.clone()],
    })
}

fn build_errors(error: &Value) -> anyhow::Result<Option<&[Value]>> {
    let Some(errors) = error.get("errors").and_then(Value::as_array) else {
        return Ok(None);
    };
    for entry in errors {
        anyhow::ensure!(
            !entry.is_null(),
            "Cannot read properties of null (reading 'text')"
        );
        if !entry.get("text").is_some_and(truthy) {
            return Ok(None);
        }
    }
    Ok(Some(errors))
}

fn format_diagnostic(error: &Value, color: bool) -> Value {
    let text = error.get("text").expect("validated compiler diagnostic");
    let Some(location) = error.get("location").filter(|location| truthy(location)) else {
        return text.clone();
    };
    let Some(file) = location.get("file").and_then(Value::as_str) else {
        return json!({"name":"TypeError", "code":"ERR_INVALID_ARG_TYPE", "message":"The \"path\" argument must be of type string or an instance of Buffer or URL. Received undefined"});
    };
    let source = match std::fs::read(file) {
        Ok(source) => String::from_utf8_lossy(&source).into_owned(),
        Err(error) => return read_failure(file, &error),
    };
    let line = location.get("line").and_then(Value::as_i64).unwrap_or(0);
    let column = location.get("column").and_then(Value::as_i64).unwrap_or(0);
    let frame = code_frame(&source, line, column, &js_string(text), color);
    Value::String(format!("File: {file}:{line}:{column}\n{frame}"))
}

fn read_failure(file: &str, error: &std::io::Error) -> Value {
    let (code, operation, reason) = match error.kind() {
        std::io::ErrorKind::NotFound => ("ENOENT", "open", "no such file or directory"),
        std::io::ErrorKind::PermissionDenied => ("EACCES", "open", "permission denied"),
        std::io::ErrorKind::IsADirectory => ("EISDIR", "read", "illegal operation on a directory"),
        _ => ("UNKNOWN", "read", "input/output error"),
    };
    let message = if operation == "open" {
        format!("{code}: {reason}, {operation} '{file}'")
    } else {
        format!("{code}: {reason}, {operation}")
    };
    json!({"name":"Error", "code":code, "message":message})
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(values) => values
            .iter()
            .map(|value| {
                if value.is_null() {
                    String::new()
                } else {
                    js_string(value)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

fn color_supported() -> bool {
    let force = std::env::var("FORCE_COLOR").ok();
    if force
        .as_deref()
        .is_some_and(|value| value == "0" || value == "false")
    {
        return false;
    }
    let arguments: Vec<_> = std::env::args().collect();
    std::env::var_os("NO_COLOR").is_none()
        && !arguments.iter().any(|argument| argument == "--no-color")
        && (force.is_some()
            || arguments.iter().any(|argument| argument == "--color")
            || cfg!(windows)
            || (std::io::stdout().is_terminal() && std::env::var("TERM").as_deref() != Ok("dumb"))
            || std::env::var_os("CI").is_some())
}

fn lines(source: &str) -> Vec<&str> {
    static NEWLINES: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\r\n|[\n\r\u2028\u2029]").expect("valid newline expression"));
    let mut lines = Vec::new();
    let mut start = 0;
    for found in NEWLINES.find_iter(source) {
        lines.push(&source[start..found.range.start]);
        start = found.range.end;
    }
    lines.push(&source[start..]);
    lines
}

fn paint(value: &str, open: &str, close: &str, enabled: bool) -> String {
    if !enabled {
        return value.to_owned();
    }
    // Picocolors reopens a color after an embedded closing sequence.
    let value = value.replace(close, open);
    format!("{open}{value}{close}")
}

fn red_bold(value: &str, color: bool) -> String {
    paint(
        &paint(value, "\x1b[1m", "\x1b[22m", color),
        "\x1b[31m",
        "\x1b[39m",
        color,
    )
}

fn gray(value: &str, color: bool) -> String {
    paint(value, "\x1b[90m", "\x1b[39m", color)
}

/// Formats the exact single-location code frame used by HMR.
#[must_use]
pub fn code_frame(source: &str, line: i64, column: i64, message: &str, color: bool) -> String {
    let source_lines = lines(source);
    let total = i64::try_from(source_lines.len()).unwrap_or(i64::MAX);
    let start = if line == -1 {
        0
    } else {
        line.saturating_sub(3).max(0)
    };
    let end = if line == -1 {
        total
    } else {
        line.saturating_add(3).min(total)
    };
    let width = end.to_string().len();
    let highlighted = if color {
        highlight(source)
    } else {
        source.to_owned()
    };
    let highlighted = lines(&highlighted);
    let mut rendered = Vec::new();
    for (index, source) in highlighted.iter().enumerate() {
        let position = i64::try_from(index).unwrap_or(i64::MAX);
        if position < start || position >= end {
            continue;
        }
        let number = position + 1;
        let padded = format!(" {number}");
        let padded = &padded[padded.len().saturating_sub(width)..];
        let gutter = format!(" {padded} |");
        let text = if source.is_empty() {
            String::new()
        } else {
            format!(" {source}")
        };
        if number == line {
            let mut row = format!("{}{}{text}", red_bold(">", color), gray(&gutter, color));
            if column != 0 {
                let spacing: String = source
                    .encode_utf16()
                    .take(usize::try_from(column.saturating_sub(1).max(0)).unwrap_or(usize::MAX))
                    .map(|unit| if unit == u16::from(b'\t') { '\t' } else { ' ' })
                    .collect();
                let gutter: String = gutter
                    .chars()
                    .map(|character| {
                        if character.is_ascii_digit() {
                            ' '
                        } else {
                            character
                        }
                    })
                    .collect();
                let _ = write!(
                    row,
                    "\n {} {spacing}{}",
                    gray(&gutter, color),
                    red_bold("^", color)
                );
                if !message.is_empty() {
                    let _ = write!(row, " {}", red_bold(message, color));
                }
            }
            rendered.push(row);
        } else {
            rendered.push(format!(" {}{text}", gray(&gutter, color)));
        }
    }
    paint(&rendered.join("\n"), "\x1b[0m", "\x1b[0m", color)
}

fn highlight(source: &str) -> String {
    static TOKENS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"((['"])(?:(?!\2|\\).|\\(?:\r\n|[\s\S]))*(\2)?|`(?:[^`\\$]|\\[\s\S]|\$(?!\{)|\$\{(?:[^{}]|\{[^}]*\}?)*\}?)*(`)?)|(\/\/.*)|(\/\*(?:[^*]|\*(?!\/))*(\*\/)?)|(\/(?!\*)(?:\[(?:(?![\]\\]).|\\.)*\]|(?![\/\]\\]).|\\.)+\/(?:(?!\s*(?:\b|[\u0080-\uFFFF$\\'"~({]|[+\-!](?!=)|\.?\d))|[gmiyus]{1,6}\b(?![\u0080-\uFFFF$\\]|\s*(?:[+\-*%&|^<>!=?({]|\/(?![\/*])))))|(0[xX][\da-fA-F]+|0[oO][0-7]+|0[bB][01]+|(?:\d*\.\d+|\d+\.?)(?:[eE][+-]?\d+)?)|((?!\d)(?:(?!\s)[$\w\u0080-\uFFFF]|\\u[\da-fA-F]{4}|\\u\{[\da-fA-F]+\})+)|(--|\+\+|&&|\|\||=>|\.{3}|(?:[+\-\/%&|^]|\*{1,2}|<{1,2}|>{1,3}|!=?|={1,2})=?|[?~.,:;[\](){}])|(\s+)|(^$|[\s\S])"#).expect("js-tokens expression is valid")
    });
    if source.is_empty() {
        return String::new();
    }
    let mut output = String::new();
    let units: Vec<_> = source.encode_utf16().collect();
    for token in TOKENS.find_from_ucs2(&units, 0) {
        let value = String::from_utf16_lossy(&units[token.range.clone()]);
        let value = value.as_str();
        let category = if token.group(1).is_some() {
            Some(32)
        } else if token.group(5).is_some() || token.group(6).is_some() {
            Some(90)
        } else if token.group(8).is_some() || token.group(9).is_some() {
            Some(35)
        } else if token.group(10).is_some() {
            if is_keyword(value) {
                Some(36)
            } else if (token.range.start > 0 && units[token.range.start - 1] == u16::from(b'<'))
                || (token.range.start > 1
                    && units[token.range.start - 2..token.range.start]
                        == [u16::from(b'<'), u16::from(b'/')])
                || value.chars().next().is_some_and(|character| {
                    character.to_lowercase().to_string() != character.to_string()
                })
            {
                Some(33)
            } else {
                None
            }
        } else if token.group(11).is_some() {
            if value.chars().count() == 1 && "()[]{}".contains(value) {
                None
            } else {
                Some(33)
            }
        } else if token.group(12).is_some() {
            None
        } else if matches!(value, "@" | "#") {
            Some(33)
        } else {
            Some(0)
        };
        if let Some(category) = category {
            let painted = lines(value)
                .into_iter()
                .map(|line| {
                    if category == 0 {
                        paint(
                            &paint(
                                &paint(line, "\x1b[1m", "\x1b[22m", true),
                                "\x1b[41m",
                                "\x1b[49m",
                                true,
                            ),
                            "\x1b[37m",
                            "\x1b[39m",
                            true,
                        )
                    } else {
                        paint(line, &format!("\x1b[{category}m"), "\x1b[39m", true)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            output.push_str(&painted);
        } else {
            output.push_str(value);
        }
    }
    output
}

fn is_keyword(value: &str) -> bool {
    matches!(
        value,
        "break"
            | "case"
            | "catch"
            | "continue"
            | "debugger"
            | "default"
            | "do"
            | "else"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "return"
            | "switch"
            | "throw"
            | "try"
            | "var"
            | "const"
            | "while"
            | "with"
            | "new"
            | "this"
            | "super"
            | "class"
            | "extends"
            | "export"
            | "import"
            | "null"
            | "true"
            | "false"
            | "in"
            | "instanceof"
            | "typeof"
            | "void"
            | "delete"
            | "enum"
            | "implements"
            | "interface"
            | "let"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "static"
            | "yield"
            | "await"
            | "as"
            | "async"
            | "from"
            | "get"
            | "of"
            | "set"
    )
}

/// Formats a compiler error after resolving relative file names at its boundary.
///
/// # Errors
///
/// Returns malformed structured compiler errors.
pub fn format_build_failure(
    failure: &BuildFailure,
    working_directory: &Path,
    color: bool,
) -> anyhow::Result<Vec<Value>> {
    let mut failure = failure.clone();
    for diagnostic in &mut failure.errors {
        if let Some(location) = &mut diagnostic.location
            && !Path::new(&location.file).is_absolute()
        {
            location.file = working_directory
                .join(&location.file)
                .to_string_lossy()
                .into_owned();
        }
    }
    format_error(&serde_json::to_value(failure)?, color)
}
