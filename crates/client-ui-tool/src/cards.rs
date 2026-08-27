//! Defensive diff, read, Web, search, and terminal card narrowing.

use serde_json::Value;

use crate::{ToolCallBlock, relativize_to_cwd};

/// Chat-row line cap shared by diff and read summaries.
pub const CHAT_CARD_MAX_LINES: usize = 8;

/// One diff hunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffHunk {
    /// File path.
    pub path: String,
    /// Previous text; absent for file creation.
    pub old_text: Option<String>,
    /// New text.
    pub new_text: String,
}

/// Diff card model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffCardModel {
    /// Valid non-empty hunks.
    pub diffs: Vec<DiffHunk>,
}

fn card<'a>(
    value: Option<&'a Value>,
    expected: &str,
) -> Option<&'a serde_json::Map<String, Value>> {
    value
        .and_then(Value::as_object)
        .filter(|view| view.get("card").and_then(Value::as_str) == Some(expected))
}

fn narrow_diffs(value: Option<&Value>) -> Option<Vec<DiffHunk>> {
    let values = value?.as_array()?;
    if values.is_empty() {
        return None;
    }
    values
        .iter()
        .map(|value| {
            let value = value.as_object()?;
            Some(DiffHunk {
                path: value.get("path")?.as_str()?.to_owned(),
                old_text: match value.get("oldText")? {
                    Value::Null => None,
                    Value::String(text) => Some(text.clone()),
                    _ => return None,
                },
                new_text: value.get("newText")?.as_str()?.to_owned(),
            })
        })
        .collect()
}

/// Derives a running intended diff or settled applied diff.
#[must_use]
pub fn diff_card_model(block: &ToolCallBlock) -> Option<DiffCardModel> {
    let view = if block.settled() {
        block.result_view()
    } else {
        block.call_view()
    };
    let view = card(view, "diff")?;
    Some(DiffCardModel {
        diffs: narrow_diffs(view.get("diffs"))?,
    })
}

/// One read-result line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadLine {
    /// One-based source line number.
    pub number: u64,
    /// Line text.
    pub text: String,
}

/// Read card model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadCardModel {
    /// Replacement title or relative path.
    pub label: String,
    /// Detached line rows.
    pub lines: Vec<ReadLine>,
    /// Complete file line count.
    pub total_lines: u64,
    /// Optional syntax language.
    pub lang: Option<String>,
}

/// Derives a settled result-side read card.
#[must_use]
pub fn read_card_model(block: &ToolCallBlock, cwd: Option<&str>) -> Option<ReadCardModel> {
    if !block.settled() {
        return None;
    }
    let view = card(block.result_view(), "read")?;
    let path = view.get("path")?.as_str()?;
    let lines = view
        .get("lines")?
        .as_array()?
        .iter()
        .map(|line| {
            let line = line.as_object()?;
            Some(ReadLine {
                number: line.get("number")?.as_u64()?,
                text: line.get("text")?.as_str()?.to_owned(),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(ReadCardModel {
        label: view
            .get("title")
            .and_then(Value::as_str)
            .map_or_else(|| relativize_to_cwd(path, cwd), ToOwned::to_owned),
        lines,
        total_lines: view.get("totalLines")?.as_u64()?,
        lang: view
            .get("lang")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    })
}

/// One Web search source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebSource {
    /// Result URL.
    pub url: String,
    /// Optional title.
    pub title: Option<String>,
    /// Optional snippet.
    pub snippet: Option<String>,
    /// Optional publication time.
    pub published_at: Option<String>,
}

/// Web result card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebCardModel {
    /// Search answer and sources.
    Search {
        /// Optional synthesized answer.
        answer: Option<String>,
        /// Detached sources.
        sources: Vec<WebSource>,
        /// Whether results were capped.
        truncated: bool,
    },
    /// Fetch URL/status summary.
    Fetch {
        /// Requested URL.
        url: String,
        /// HTTP status.
        status_code: u16,
        /// Whether body was capped.
        truncated: bool,
    },
}

/// Derives a settled result-side Web card, rejecting unknown wire kinds.
#[must_use]
pub fn web_card_model(block: &ToolCallBlock) -> Option<WebCardModel> {
    if !block.settled() {
        return None;
    }
    let view = card(block.result_view(), "web")?;
    let truncated = view
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    match view.get("kind")?.as_str()? {
        "search" => {
            let sources = view
                .get("sources")?
                .as_array()?
                .iter()
                .map(|source| {
                    let source = source.as_object()?;
                    Some(WebSource {
                        url: source.get("url")?.as_str()?.to_owned(),
                        title: source
                            .get("title")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                        snippet: source
                            .get("snippet")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                        published_at: source
                            .get("publishedAt")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            Some(WebCardModel::Search {
                answer: view
                    .get("answer")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                sources,
                truncated,
            })
        }
        "fetch" => Some(WebCardModel::Fetch {
            url: view.get("url")?.as_str()?.to_owned(),
            status_code: u16::try_from(view.get("statusCode")?.as_u64()?).ok()?,
            truncated,
        }),
        _ => None,
    }
}

/// One line match inside a grouped search result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    /// One-based source line number.
    pub line_number: serde_json::Number,
    /// Matching source line.
    pub line: String,
}

/// One file and its matching lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchFileGroup {
    /// File path.
    pub path: String,
    /// Matching rows.
    pub matches: Vec<SearchMatch>,
}

/// Structured search result body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchCard {
    /// Grep-like results grouped by file.
    Matches {
        /// Detached file groups.
        files: Vec<SearchFileGroup>,
        /// Whether the tool capped the result.
        truncated: bool,
        /// Total match count before capping.
        total: serde_json::Number,
    },
    /// Glob-like path list.
    Paths {
        /// Detached result paths.
        paths: Vec<String>,
        /// Whether the tool capped the result.
        truncated: bool,
        /// Total path count before capping.
        total: serde_json::Number,
    },
}

/// Search result model plus replacement and recovery copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchCardModel {
    /// Result-time replacement title.
    pub title: Option<String>,
    /// Raw recovery locator retained only for capped results.
    pub recovery: Option<String>,
    /// Structured card body.
    pub card: SearchCard,
}

fn settled_content(block: &ToolCallBlock) -> Option<&[Value]> {
    let ToolCallBlock::Settled { content, .. } = block else {
        return None;
    };
    Some(content)
}

fn flattened_text_content(content: &[Value]) -> Option<String> {
    let text = content
        .iter()
        .filter_map(|block| {
            (block.get("type").and_then(Value::as_str) == Some("text"))
                .then(|| block.get("text").and_then(Value::as_str))
                .flatten()
        })
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn narrow_search_files(value: &Value) -> Option<Vec<SearchFileGroup>> {
    value
        .as_array()?
        .iter()
        .map(|file| {
            let file = file.as_object()?;
            let matches = file
                .get("matches")?
                .as_array()?
                .iter()
                .map(|matched| {
                    let matched = matched.as_object()?;
                    Some(SearchMatch {
                        line_number: matched.get("lineNumber")?.as_number()?.clone(),
                        line: matched.get("line")?.as_str()?.to_owned(),
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            Some(SearchFileGroup {
                path: file.get("path")?.as_str()?.to_owned(),
                matches,
            })
        })
        .collect()
}

/// Derives a settled result-side search card and rejects malformed wire shapes.
#[must_use]
pub fn search_card_model(block: &ToolCallBlock) -> Option<SearchCardModel> {
    if !block.settled() {
        return None;
    }
    let view = card(block.result_view(), "search")?;
    let truncated = view.get("truncated")?.as_bool()?;
    let total = view.get("total")?.as_number()?.clone();
    let recovery = truncated
        .then(|| settled_content(block).and_then(flattened_text_content))
        .flatten();
    let card = match view.get("shape")?.as_str()? {
        "matches" => SearchCard::Matches {
            files: narrow_search_files(view.get("files")?)?,
            truncated,
            total,
        },
        "paths" => SearchCard::Paths {
            paths: view
                .get("paths")?
                .as_array()?
                .iter()
                .map(|path| path.as_str().map(ToOwned::to_owned))
                .collect::<Option<Vec<_>>>()?,
            truncated,
            total,
        },
        _ => return None,
    };
    Some(SearchCardModel {
        title: view
            .get("title")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        recovery,
        card,
    })
}

/// Terminal prompt and result body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalCard {
    /// Command shown after the prompt marker.
    pub command: String,
    /// Resolved working directory label.
    pub cwd: Option<String>,
    /// Captured output, absent while running.
    pub output: Option<String>,
    /// Process exit code.
    pub exit_code: Option<serde_json::Number>,
    /// Terminating signal.
    pub signal: Option<String>,
    /// Whether execution is still pending.
    pub running: bool,
}

/// Terminal body plus model-authored description.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalCardModel {
    /// Terminal primitive inputs.
    pub card: TerminalCard,
    /// Call-side description rendered above the card.
    pub description: Option<String>,
}

/// Whether a settled terminal result reports a non-zero exit or signal.
#[must_use]
pub fn terminal_failed(model: &TerminalCardModel) -> bool {
    !model.card.running
        && (model
            .card
            .exit_code
            .as_ref()
            .is_some_and(|code| code.as_f64() != Some(0.0))
            || model.card.signal.is_some())
}

fn is_separator(byte: u8) -> bool {
    matches!(byte, b'/' | b'\\')
}

fn is_host_absolute(path: &str) -> bool {
    path.starts_with('/')
        || path.starts_with(r"\\")
        || matches!(
            path.as_bytes(),
            [drive, b':', separator, ..]
                if drive.is_ascii_alphabetic() && is_separator(*separator)
        )
}

fn resolve_workspace_path(cwd: &str, path: &str) -> String {
    if is_host_absolute(path) {
        return path.to_owned();
    }
    let base = cwd.trim_end_matches(['/', '\\']);
    let relative = path.trim_start_matches(['/', '\\']);
    format!("{base}/{relative}")
}

fn collapse_segments(body: &str, rooted: bool, separator: char) -> String {
    let mut kept = Vec::new();
    for segment in body.split(['/', '\\']) {
        match segment {
            ".." if kept.last().is_some_and(|last| *last != "..") => {
                kept.pop();
            }
            ".." if !rooted => kept.push(segment),
            "" | "." | ".." => {}
            _ => kept.push(segment),
        }
    }
    kept.join(&separator.to_string())
}

fn unc_root(path: &str) -> Option<(&str, &str, usize)> {
    let bytes = path.as_bytes();
    if bytes.len() < 3
        || !is_separator(bytes[0])
        || !is_separator(bytes[1])
        || is_separator(bytes[2])
    {
        return None;
    }
    let server_end = bytes[2..].iter().position(|byte| is_separator(*byte))? + 2;
    let mut share_start = server_end;
    while share_start < bytes.len() && is_separator(bytes[share_start]) {
        share_start += 1;
    }
    if share_start == bytes.len() {
        return None;
    }
    let share_end = bytes[share_start..]
        .iter()
        .position(|byte| is_separator(*byte))
        .map_or(bytes.len(), |offset| share_start + offset);
    Some((
        &path[2..server_end],
        &path[share_start..share_end],
        share_end,
    ))
}

fn normalize_segments(path: &str) -> String {
    if !path
        .split(['/', '\\'])
        .any(|segment| matches!(segment, "." | ".."))
    {
        return path.to_owned();
    }
    if let Some((server, share, matched_end)) = unc_root(path) {
        let root = format!(r"\\{server}\{share}");
        let rest = collapse_segments(&path[matched_end..], true, '/');
        return if rest.is_empty() {
            root
        } else {
            format!(r"{root}\{rest}")
        };
    }
    let backslashed = path.contains('\\') && !path.contains('/');
    let separator = if backslashed { '\\' } else { '/' };
    let rooted = path
        .as_bytes()
        .first()
        .is_some_and(|byte| is_separator(*byte));
    let drive_len = matches!(path.as_bytes(), [drive, b':', ..] if drive.is_ascii_alphabetic())
        .then_some(2)
        .unwrap_or(0);
    let body = collapse_segments(&path[drive_len..], rooted || drive_len != 0, separator);
    let leading = if rooted {
        separator.to_string()
    } else {
        String::new()
    };
    if drive_len == 0 {
        format!("{leading}{body}")
    } else {
        let drive = &path[..drive_len];
        let separator = if rooted {
            leading.as_str()
        } else if backslashed {
            "\\"
        } else {
            "/"
        };
        format!("{drive}{separator}{body}")
    }
}

fn resolve_terminal_cwd(view_cwd: Option<&str>, session_cwd: Option<&str>) -> Option<String> {
    let Some(view_cwd) = view_cwd.filter(|cwd| !cwd.is_empty()) else {
        return session_cwd.map(ToOwned::to_owned);
    };
    match session_cwd.filter(|cwd| !cwd.is_empty()) {
        None => Some(normalize_segments(view_cwd)),
        Some(session_cwd) => Some(normalize_segments(&resolve_workspace_path(
            session_cwd,
            view_cwd,
        ))),
    }
}

/// Derives a call/result terminal card with exact window-truncation behavior.
#[must_use]
pub fn terminal_card_model(
    block: &ToolCallBlock,
    session_cwd: Option<&str>,
) -> Option<TerminalCardModel> {
    let call = card(block.call_view(), "terminal");
    if !block.settled() {
        let call = call?;
        return Some(TerminalCardModel {
            description: call
                .get("description")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            card: TerminalCard {
                command: call.get("title")?.as_str()?.to_owned(),
                cwd: resolve_terminal_cwd(call.get("cwd").and_then(Value::as_str), session_cwd),
                output: None,
                exit_code: None,
                signal: None,
                running: true,
            },
        });
    }
    let result = card(block.result_view(), "terminal")?;
    let command = result
        .get("title")
        .and_then(Value::as_str)
        .or_else(|| call.and_then(|call| call.get("title").and_then(Value::as_str)))
        .unwrap_or_default()
        .to_owned();
    Some(TerminalCardModel {
        description: call
            .and_then(|call| call.get("description"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        card: TerminalCard {
            command,
            cwd: call.and_then(|call| {
                resolve_terminal_cwd(call.get("cwd").and_then(Value::as_str), session_cwd)
            }),
            output: result
                .get("output")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            exit_code: result.get("exitCode").and_then(Value::as_number).cloned(),
            signal: result
                .get("signal")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            running: false,
        },
    })
}
