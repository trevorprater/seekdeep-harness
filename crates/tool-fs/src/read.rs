//! Model-facing UTF-8 read tool: validation, caps, and registration.

use std::sync::Arc;

use futures::StreamExt as _;
use seekdeep_cordis::Context;
use seekdeep_fs::{FS, FsObservation};
use seekdeep_llm::ContentBlock;
use seekdeep_system_prompt::{PromptSection, PromptText, SYSTEM_PROMPT};
use seekdeep_tools::{
    DefineToolOptions, DefineToolOutput, FileLocation, GenericCallView, ReadFileLine,
    ReadResultView, TOOLS, ToolCallKind, ToolCallView, ToolResult, ToolResultView, define_tool,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::read_render::{
    FileReadOutcome, FileTextLine, ReadWindow, build_window, format_read_output, lang_from_path,
    read_meta_from_meta,
};
use crate::read_target::{emit_fs_observed, resolve_regular_read_target};

/// Default and maximum number of lines returned by one read call.
pub const READ_LIMIT: u64 = 2000;

/// Default streaming threshold in bytes.
pub const STREAM_MIN_SIZE: u64 = 10 * 1024 * 1024;

/// Resolved read-tool caps — plugin config after defaulting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadToolCaps {
    /// Default and maximum number of lines returned by one call.
    pub limit: u64,
    /// Maximum characters returned for a single line.
    pub max_line_length: usize,
    /// Maximum bytes returned for selected file lines.
    pub max_bytes: usize,
    /// Files at or above this size stream.
    pub stream_min_size: u64,
}

/// Raw schema-validated read arguments.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadArgsRaw {
    /// Path to read.
    pub file_path: String,
    /// 1-based first line to return.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<f64>,
    /// Maximum number of lines to return.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
}

/// Validated read arguments after defaulting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadInput {
    /// Path to read.
    pub file_path: String,
    /// 1-based first line to return.
    pub offset: u64,
    /// Maximum number of lines to return.
    pub limit: u64,
}

fn parse_positive_integer(value: f64, name: &str) -> anyhow::Result<u64> {
    if !value.is_finite() || value.fract() != 0.0 || value < 1.0 {
        anyhow::bail!("{name} must be a positive integer");
    }
    // The guard above proves the value is a positive finite integer, so the
    // saturating float-to-int cast is exact within the representable range.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(value as u64)
}

/// Validates value constraints the schema cannot express and applies defaults.
///
/// # Errors
///
/// Returns a blank-path, non-integer, non-positive, or over-limit failure.
pub fn parse_read_args(args: &ReadArgsRaw, max_limit: u64) -> anyhow::Result<ReadInput> {
    if args.file_path.trim().is_empty() {
        anyhow::bail!("file_path must be a non-empty string");
    }
    let offset = args
        .offset
        .map_or(Ok(1), |value| parse_positive_integer(value, "offset"))?;
    let limit = args.limit.map_or(Ok(max_limit), |value| {
        parse_positive_integer(value, "limit")
    })?;
    if limit > max_limit {
        anyhow::bail!("limit must be less than or equal to {max_limit}");
    }
    Ok(ReadInput {
        file_path: args.file_path.clone(),
        offset,
        limit,
    })
}

/// Canonical successful read outcome matching the tool's output schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadOutcome {
    /// Resolved model-facing path.
    pub path: String,
    /// 1-based first line requested.
    pub offset: u64,
    /// Returned line window.
    pub lines: Vec<FileTextLine>,
    /// Exact total line count.
    pub total_lines: u64,
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn number_to_u64(value: f64) -> u64 {
    value as u64
}

/// Extracts the body between the read envelope's `<content>` fences.
fn extract_read_body(text: &str) -> Option<&str> {
    use std::sync::OnceLock;
    static READ_BODY: OnceLock<regex::Regex> = OnceLock::new();
    let read_body = READ_BODY.get_or_init(|| {
        regex::Regex::new(
            r"^<path>[^
]*</path>
<type>file</type>
<content>
([sS]*)
</content>$",
        )
        .expect("read body envelope regex is constant")
    });
    read_body.captures(text)?.get(1).map(|m| m.as_str())
}

/// Registers the `read` tool and its system-prompt guidance.
///
/// # Errors
///
/// Returns missing-service, prompt-registration, or tool-registration failures.
#[allow(clippy::too_many_lines)]
pub fn apply_read_tool(ctx: &Context, caps: &ReadToolCaps) -> anyhow::Result<()> {
    let prompt = ctx
        .get(SYSTEM_PROMPT)
        .ok_or_else(|| anyhow::anyhow!("tool-fs requires systemPrompt"))?;
    prompt.section(
        ctx,
        PromptSection::new(
            "tool:read",
            100.0,
            PromptText::Static(
                "Use the read tool — not shell commands like cat — to inspect text files. Results include line numbers. Use offset and limit to continue reading large files."
                    .to_owned(),
            ),
        ),
    )?;

    let tools = ctx
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("tool-fs requires tools"))?;
    let caps = *caps;
    let execute_ctx = ctx.clone();
    let output = DefineToolOutput::new(
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "path": {"type": "string", "required": true},
                "offset": {"type": "integer", "required": true},
                "lines": {
                    "type": "array",
                    "required": true,
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "number": {"type": "integer", "required": true},
                            "text": {"type": "string", "required": true},
                        },
                    },
                },
                "totalLines": {"type": "integer", "required": true},
            },
        }),
        Arc::new(move |args: &ReadArgsRaw, value: &ReadOutcome| {
            let input = parse_read_args(args, caps.limit)?;
            let end_line = value
                .lines
                .last()
                .map_or_else(|| value.offset.saturating_sub(1), |line| line.number);
            let truncated_by_bytes = value.lines.len()
                < usize::try_from(input.limit).unwrap_or(usize::MAX)
                && end_line < value.total_lines;
            Ok(vec![ContentBlock::Text {
                text: format_read_output(
                    &value.path,
                    &FileReadOutcome {
                        offset: value.offset,
                        lines: value.lines.clone(),
                        total_lines: value.total_lines,
                        truncated_by_bytes: truncated_by_bytes.then_some(true),
                    },
                ),
            }])
        }),
    )
    .presentation_meta(Arc::new(move |_args: &ReadArgsRaw, value: &ReadOutcome| {
        Ok(serde_json::to_value(crate::read_render::FsReadMeta {
            path: value.path.clone(),
            offset: value.offset,
            lines: value.lines.clone(),
            total_lines: value.total_lines,
            lang: lang_from_path(&value.path).map(str::to_owned),
        })?)
    }));

    let definition = define_tool(
        DefineToolOptions::new(
            "read",
            "Read a UTF-8 text file and return line-numbered content.",
            json!({
                "file_path": {"type": "string", "required": true, "description": "Path to read, resolved by the filesystem backend."},
                "offset": {"type": "number", "description": "1-based first line to return. Defaults to 1."},
                "limit": {"type": "number", "description": format!("Maximum number of lines to return. Defaults to {}.", caps.limit)},
            }),
            output,
            Arc::new(move |args: ReadArgsRaw, execution| {
                let ctx = execute_ctx.clone();
                Box::pin(async move {
                    let input = parse_read_args(&args, caps.limit)?;
                    let (target, info) =
                        resolve_regular_read_target(&ctx, &execution, &input.file_path).await?;
                    let filesystem = ctx
                        .get(FS)
                        .ok_or_else(|| anyhow::anyhow!("tool-fs requires fs"))?
                        .filesystem();
                    let signal = execution.signal();
                    let chunks = if info.size.is_none()
                        || info.size.is_some_and(|size| size >= caps.stream_min_size)
                    {
                        let stream = filesystem.stream_text(&target, Some(&signal)).await?;
                        stream.collect::<Vec<String>>().await
                    } else {
                        vec![filesystem.read_text(&target, Some(&signal)).await?]
                    };
                    let window = build_window(
                        chunks,
                        &ReadWindow {
                            offset: input.offset,
                            limit: usize::try_from(input.limit).unwrap_or(usize::MAX),
                            max_line_length: caps.max_line_length,
                            max_bytes: caps.max_bytes,
                        },
                        &target.display_path,
                    )?;
                    let outcome = ReadOutcome {
                        path: target.display_path.clone(),
                        offset: input.offset,
                        lines: window.lines,
                        total_lines: window.total_lines,
                    };
                    emit_fs_observed(
                        &ctx,
                        &target,
                        FsObservation::Present {
                            version: info.version,
                        },
                        &execution,
                    )?;
                    Ok(outcome)
                })
            }),
        )
        .concurrency_safe(Arc::new(|_args: &ReadArgsRaw| true))
        .present_call(Arc::new(|args: &ReadArgsRaw| {
            let offset = args.offset.unwrap_or(1.0);
            let window = if let Some(limit) = args.limit {
                if limit > 0.0 {
                    format!(" ({} - {})", offset, offset + limit - 1.0)
                } else {
                    String::new()
                }
            } else if args.offset.is_some() {
                format!(" (from line {offset})")
            } else {
                String::new()
            };
            Some(ToolCallView::Generic(GenericCallView {
                title: format!("Read {}{}", args.file_path, window),
                kind: Some(ToolCallKind::Read),
                raw_input: None,
                content: None,
                locations: Some(vec![FileLocation {
                    path: args.file_path.clone(),
                    line: Some(number_to_u64(offset)),
                }]),
            }))
        }))
        .present_result(Arc::new(
            move |_args: &ReadArgsRaw, result: &ToolResult| {
                if result.is_error {
                    return None;
                }
                let meta = read_meta_from_meta(result.meta.as_ref()?)?;
                let only = (result.content.len() == 1).then(|| &result.content[0]);
                let text = match only {
                    Some(ContentBlock::Text { text }) => Some(text),
                    _ => None,
                }?;
                let body = extract_read_body(text)?;
                Some(ToolResultView::Read(ReadResultView {
                    title: None,
                    path: meta.path,
                    offset: meta.offset,
                    lines: meta
                        .lines
                        .into_iter()
                        .map(|line| ReadFileLine {
                            number: line.number,
                            text: line.text,
                        })
                        .collect(),
                    total_lines: meta.total_lines,
                    lang: meta.lang,
                    content: Some(vec![ContentBlock::Text {
                        text: body.to_owned(),
                    }]),
                }))
            },
        )),
    )?;
    tools.register(ctx, definition)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(file_path: &str, offset: Option<f64>, limit: Option<f64>) -> ReadArgsRaw {
        ReadArgsRaw {
            file_path: file_path.to_owned(),
            offset,
            limit,
        }
    }

    #[test]
    fn defaults_offset_and_limit() {
        let input = parse_read_args(&raw("a.txt", None, None), 100).expect("defaults");
        assert_eq!(input.offset, 1);
        assert_eq!(input.limit, 100);
    }

    #[test]
    fn rejects_blank_path_and_invalid_numbers() {
        assert!(parse_read_args(&raw("  ", None, None), 100).is_err());
        assert!(parse_read_args(&raw("a", Some(0.0), None), 100).is_err());
        assert!(parse_read_args(&raw("a", Some(1.5), None), 100).is_err());
        assert!(parse_read_args(&raw("a", None, Some(101.0)), 100).is_err());
    }

    #[test]
    fn accepts_valid_explicit_values() {
        let input = parse_read_args(&raw("a", Some(5.0), Some(10.0)), 100).expect("valid");
        assert_eq!(input.offset, 5);
        assert_eq!(input.limit, 10);
    }
}
