//! Pure ACP transcript and Session-log normalization.

use std::{cmp::Reverse, sync::OnceLock};

use indexmap::IndexMap;
use regex::{Captures, Regex};
use serde_json::{Map, Value};

const SESSION_ID: &str = "{{sessionId}}";
const CWD: &str = "{{cwd}}";
const SYSTEM: &str = "{{system}}";
const TOOLS: &str = "{{tools}}";
const EVENT_TIME: &str = "{{eventTime}}";
const EVENT_OMITTED_BYTES: &str = "{{eventOmittedBytes}}";

/// Inputs needed to recognize one run's volatile values.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NormalizeContext {
    /// Session identities issued by the run.
    pub session_ids: Vec<String>,
    /// Generated workspace used by the run.
    pub cwd: String,
    /// Other filesystem spellings of the same workspace.
    pub cwd_aliases: Vec<String>,
}

/// Separator representation used after a generated workspace is tokenized.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CwdPathMode {
    /// Canonical shared-golden `/` separators.
    #[default]
    Canonical,
    /// Preserve captured native separators.
    Native,
}

/// Optional normalization controls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NormalizeOptions {
    /// Cwd-rooted separator policy.
    pub cwd_path_mode: CwdPathMode,
}

/// Failures from malformed JSONL and workspace tokenization.
#[derive(Debug, thiserror::Error)]
pub enum NormalizeError {
    /// One nonempty line was not valid JSON.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// The fixture header exposed no usable workspace basename.
    #[error("acp-snapshot: cannot tokenize a cwd without a basename")]
    MissingCwdBasename,
}

/// Extracts snapshot-mode spill paths by filename, with the last path winning.
#[must_use]
pub fn extract_snapshot_spill_paths(content: &str) -> IndexMap<String, String> {
    let mut result = IndexMap::new();
    for captures in snapshot_spill_path_re().captures_iter(content) {
        let (Some(name), Some(path)) = (captures.name("name"), captures.name("path")) else {
            continue;
        };
        result.insert(name.as_str().to_owned(), path.as_str().to_owned());
    }
    result
}

/// Stores one generated workspace as `{{cwd}}` while retaining every other value.
///
/// # Errors
///
/// Returns malformed JSON or the source diagnostic when the first record has no cwd basename.
pub fn tokenize_session_fixture_cwd(raw_log: &str) -> Result<String, NormalizeError> {
    let lines = raw_log.split('\n').collect::<Vec<_>>();
    let first_line = lines.iter().find(|line| !line.trim().is_empty()).copied();
    let header = first_line.map(serde_json::from_str::<Value>).transpose()?;
    let cwd = header
        .as_ref()
        .and_then(|header| header.get("cwd"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let basename = cwd.rsplit(['/', '\\']).next().unwrap_or_default();
    if basename.is_empty() {
        return Err(NormalizeError::MissingCwdBasename);
    }
    let context = NormalizeContext {
        session_ids: Vec::new(),
        cwd: cwd.to_owned(),
        cwd_aliases: Vec::new(),
    };
    lines
        .into_iter()
        .map(|line| {
            if line.trim().is_empty() {
                return Ok(line.to_owned());
            }
            let mut value: Value = serde_json::from_str(line)?;
            tokenize_fixture_value(&mut value, &context, basename);
            serde_json::to_string(&value).map_err(NormalizeError::from)
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|lines| lines.join("\n"))
}

/// Normalizes an NDJSON ACP stdout transcript and validates stdout purity.
///
/// # Errors
///
/// Returns when any nonempty stdout line is not valid JSON.
pub fn normalize_stdout(
    raw_stdout: &str,
    context: &NormalizeContext,
    options: NormalizeOptions,
) -> Result<String, NormalizeError> {
    let mut id_sequence = IndexMap::<String, u64>::new();
    let mut frames = Vec::new();
    for line in raw_stdout
        .split('\n')
        .filter(|line| !line.trim().is_empty())
    {
        let mut frame: Value = serde_json::from_str(line)?;
        if let Some(object) = frame.as_object_mut()
            && let Some(id) = object.get("id")
            && !id.is_null()
        {
            let key = serde_json::to_string(id)?;
            let next = u64::try_from(id_sequence.len())
                .unwrap_or(u64::MAX)
                .saturating_add(1);
            let stable = *id_sequence.entry(key).or_insert(next);
            object.insert("id".to_owned(), Value::from(stable));
        }
        scrub_value(&mut frame, context, options.cwd_path_mode, None);
        frames.push(serde_json::to_string(&frame)?);
    }
    Ok(format!("{}\n", frames.join("\n")))
}

/// Normalizes a raw Session JSONL log while preserving deterministic sequence values.
///
/// # Errors
///
/// Returns when any nonempty log line is not valid JSON.
pub fn normalize_session_log(
    raw_log: &str,
    context: &NormalizeContext,
    options: NormalizeOptions,
) -> Result<String, NormalizeError> {
    let mut records = Vec::new();
    for line in raw_log.split('\n').filter(|line| !line.trim().is_empty()) {
        let mut record: Value = serde_json::from_str(line)?;
        normalize_record_timing(&mut record);
        scrub_value(&mut record, context, options.cwd_path_mode, None);
        records.push(serde_json::to_string(&record)?);
    }
    Ok(format!("{}\n", records.join("\n")))
}

/// Replaces system-prompt content in request headers while retaining presence.
///
/// # Errors
///
/// Returns when a nonempty JSONL line is invalid.
pub fn scrub_system_prompts(raw_log: &str) -> Result<String, NormalizeError> {
    scrub_header_content(raw_log, HeaderScrubOptions::SYSTEM)
}

/// Replaces tool schemas in request headers while retaining prompts and presence.
///
/// # Errors
///
/// Returns when a nonempty JSONL line is invalid.
pub fn scrub_tool_schemas(raw_log: &str) -> Result<String, NormalizeError> {
    scrub_header_content(raw_log, HeaderScrubOptions::TOOLS)
}

/// Replaces prompt and tool-schema bulk in request headers with stable tokens.
///
/// # Errors
///
/// Returns when a nonempty JSONL line is invalid.
pub fn scrub_request_headers(raw_log: &str) -> Result<String, NormalizeError> {
    scrub_header_content(raw_log, HeaderScrubOptions::ALL)
}

fn normalize_record_timing(record: &mut Value) {
    let Some(object) = record.as_object_mut() else {
        return;
    };
    if object.get("type").and_then(Value::as_str) == Some("session") {
        if object.contains_key("createdAt") {
            object.insert("createdAt".to_owned(), Value::from(0));
        }
        return;
    }
    if object.contains_key("time0") {
        object.insert("time0".to_owned(), Value::from(0));
        if let Some(gaps) = object
            .get_mut("data")
            .and_then(Value::as_object_mut)
            .and_then(|data| data.get_mut("dt"))
            .and_then(Value::as_array_mut)
        {
            for gap in gaps {
                *gap = Value::from(0);
            }
        }
        return;
    }
    if !object.contains_key("time") {
        return;
    }
    object.insert("time".to_owned(), Value::from(0));
    if object.get("type").and_then(Value::as_str) == Some("hook/result")
        && let Some(data) = object.get_mut("data").and_then(Value::as_object_mut)
        && data.contains_key("durationMs")
    {
        data.insert("durationMs".to_owned(), Value::from(0));
    }
}

fn scrub_value(
    value: &mut Value,
    context: &NormalizeContext,
    cwd_path_mode: CwdPathMode,
    key: Option<&str>,
) {
    match value {
        Value::String(string) => {
            let mut scrubbed = scrub_string(string, context, cwd_path_mode);
            if cwd_path_mode == CwdPathMode::Canonical && key == Some("path") {
                scrubbed = scrubbed.replace('\\', "/");
            }
            *string = scrubbed;
        }
        Value::Array(values) => {
            for value in values {
                scrub_value(value, context, cwd_path_mode, None);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                scrub_value(value, context, cwd_path_mode, Some(key));
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn scrub_string(value: &str, context: &NormalizeContext, cwd_path_mode: CwdPathMode) -> String {
    let mut output = replace_cwd(value, context, CWD).replace(&format!("/private{CWD}"), CWD);
    if cwd_path_mode == CwdPathMode::Canonical {
        output = cwd_rooted_path_re()
            .replace_all(&output, |captures: &Captures<'_>| {
                captures[0].replace('\\', "/")
            })
            .into_owned();
        output = canonicalize_embedded_paths(&output);
    }
    output = replace_spill_paths(&output, local_spill_path_re());
    output = replace_spill_paths(&output, snapshot_spill_path_re());
    output = scrub_event_read_text(&output);
    for session_id in &context.session_ids {
        output = output.replace(session_id, SESSION_ID);
    }
    uuid_re().replace_all(&output, SESSION_ID).into_owned()
}

fn canonicalize_embedded_paths(value: &str) -> String {
    let output = path_tag_re()
        .replace_all(value, |captures: &Captures<'_>| {
            format!(
                "{}{}{}",
                &captures[1],
                captures[2].replace('\\', "/"),
                &captures[3]
            )
        })
        .into_owned();
    additional_instructions_path_re()
        .replace_all(&output, |captures: &Captures<'_>| {
            format!("{}{}", &captures[1], captures[2].replace('\\', "/"))
        })
        .into_owned()
}

fn scrub_event_read_text(value: &str) -> String {
    let Some(captures) = event_read_target_region_re().captures(value) else {
        return value.to_owned();
    };
    let Some(target) = captures.name("target") else {
        return value.to_owned();
    };
    let normalized = embedded_event_time_re()
        .replace_all(target.as_str(), |captures: &Captures<'_>| {
            format!("{}{}{}", &captures[1], EVENT_TIME, &captures[2])
        })
        .into_owned();
    let output = format!(
        "{}{}{}",
        &value[..target.start()],
        normalized,
        &value[target.end()..]
    );
    event_read_omitted_bytes_re()
        .replace_all(&output, |captures: &Captures<'_>| {
            format!("{}{}{}", &captures[1], EVENT_OMITTED_BYTES, &captures[2])
        })
        .into_owned()
}

fn replace_spill_paths(value: &str, expression: &Regex) -> String {
    expression
        .replace_all(value, |captures: &Captures<'_>| {
            format!(
                "{{{{spillLocator:{}}}}}{}",
                captures.name("name").map_or("", |name| name.as_str()),
                captures
                    .name("boundary")
                    .map_or("", |boundary| boundary.as_str())
            )
        })
        .into_owned()
}

fn cwd_spellings(context: &NormalizeContext) -> Vec<String> {
    let mut spellings = Vec::<String>::new();
    for spelling in std::iter::once(&context.cwd).chain(context.cwd_aliases.iter()) {
        if !spelling.is_empty() && !spellings.contains(spelling) {
            spellings.push(spelling.clone());
        }
    }
    let mac_aliases = spellings
        .iter()
        .filter(|spelling| spelling.starts_with('/') && !spelling.starts_with("/private/"))
        .map(|spelling| format!("/private{spelling}"))
        .collect::<Vec<_>>();
    for alias in mac_aliases {
        if !spellings.contains(&alias) {
            spellings.push(alias);
        }
    }
    spellings.sort_by_key(|spelling| Reverse(spelling.len()));
    spellings
}

fn replace_cwd(value: &str, context: &NormalizeContext, replacement: &str) -> String {
    cwd_spellings(context)
        .into_iter()
        .fold(value.to_owned(), |output, spelling| {
            replace_cwd_spelling(&output, &spelling, replacement)
        })
}

fn replace_cwd_spelling(value: &str, spelling: &str, replacement: &str) -> String {
    let mut cursor = 0;
    let mut output = String::new();
    while cursor < value.len() {
        let Some(offset) = value[cursor..].find(spelling) else {
            output.push_str(&value[cursor..]);
            return output;
        };
        let start = cursor + offset;
        let end = start + spelling.len();
        if is_cwd_match(value, start, spelling.len()) {
            output.push_str(&value[cursor..start]);
            output.push_str(replacement);
        } else {
            output.push_str(&value[cursor..end]);
        }
        cursor = end;
    }
    output
}

fn is_cwd_match(value: &str, start: usize, length: usize) -> bool {
    let before = value[..start].chars().next_back();
    let tail = &value[start + length..];
    let mut after = tail.chars();
    let first = after.next();
    let second = after.next();
    let starts_at_boundary = before.is_none_or(path_text_boundary)
        || file_uri_path_prefix_re().is_match(&value[..start]);
    let ends_at_boundary = first.is_none()
        || matches!(first, Some('/' | '\\'))
        || first.is_some_and(path_text_boundary)
        || first == Some('.') && second.is_none_or(path_text_boundary);
    starts_at_boundary && ends_at_boundary
}

fn path_text_boundary(character: char) -> bool {
    character.is_whitespace()
        || matches!(
            character,
            '<' | '>'
                | '\''
                | '"'
                | '`'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | ','
                | ';'
                | ':'
                | '!'
                | '?'
                | '='
        )
}

fn tokenize_fixture_value(value: &mut Value, context: &NormalizeContext, basename: &str) {
    match value {
        Value::String(string) => *string = tokenize_fixture_string(string, context, basename),
        Value::Array(values) => {
            for value in values {
                tokenize_fixture_value(value, context, basename);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                tokenize_fixture_value(value, context, basename);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn tokenize_fixture_string(value: &str, context: &NormalizeContext, basename: &str) -> String {
    let exact = replace_cwd(value, context, CWD);
    let expression = Regex::new(&format!(
        r#"(?P<path>(?:[A-Za-z]:)?[\\/](?:[^\\/\s<>"]+[\\/])*{})(?P<boundary>[\\/\s<>'"()\[\]{{}},;:!?=]|\z)"#,
        regex::escape(basename)
    ))
    .expect("escaped cwd basename makes a valid expression");
    expression
        .replace_all(&exact, |captures: &Captures<'_>| {
            format!(
                "{}{}",
                CWD,
                captures
                    .name("boundary")
                    .map_or("", |boundary| boundary.as_str())
            )
        })
        .replace(&format!("/private{CWD}"), CWD)
}

#[derive(Clone, Copy)]
struct HeaderScrubOptions {
    system: bool,
    tools: bool,
}

impl HeaderScrubOptions {
    const SYSTEM: Self = Self {
        system: true,
        tools: false,
    };
    const TOOLS: Self = Self {
        system: false,
        tools: true,
    };
    const ALL: Self = Self {
        system: true,
        tools: true,
    };
}

fn scrub_header_content(
    raw_log: &str,
    options: HeaderScrubOptions,
) -> Result<String, NormalizeError> {
    raw_log
        .split('\n')
        .map(|line| {
            if line.trim().is_empty() {
                return Ok(line.to_owned());
            }
            let mut record: Value = serde_json::from_str(line)?;
            let Some(object) = record.as_object_mut() else {
                return Ok(line.to_owned());
            };
            if object.get("type").and_then(Value::as_str) != Some("request/header") {
                return Ok(line.to_owned());
            }
            let Some(header) = object
                .get_mut("data")
                .and_then(Value::as_object_mut)
                .and_then(|data| data.get_mut("header"))
                .and_then(Value::as_object_mut)
            else {
                return Ok(line.to_owned());
            };
            let touched = scrub_header_fields(header, options);
            if touched {
                Ok(serde_json::to_string(&record)?)
            } else {
                Ok(line.to_owned())
            }
        })
        .collect::<Result<Vec<_>, NormalizeError>>()
        .map(|lines| lines.join("\n"))
}

fn scrub_header_fields(header: &mut Map<String, Value>, options: HeaderScrubOptions) -> bool {
    let mut touched = false;
    if options.system && header.contains_key("system") {
        header.insert("system".to_owned(), Value::String(SYSTEM.to_owned()));
        touched = true;
    }
    if options.tools && header.contains_key("tools") {
        header.insert("tools".to_owned(), Value::String(TOOLS.to_owned()));
        touched = true;
    }
    touched
}

fn regex(cache: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cache.get_or_init(|| Regex::new(pattern).expect("static ACP snapshot expression is valid"))
}

fn snapshot_spill_path_re() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    regex(
        &VALUE,
        r"(?P<path>(?:[A-Za-z]:)?[\\/](?:tmp|t)[\\/](?:(?:dsh|seekdeep)-acp-snap-[0-9a-f]{9}|(?:dsh|seekdeep)-acp-snapshot-spill)[\\/]session-[0-9a-f]{12}[\\/][0-9a-f]{12}-(?P<name>[A-Za-z0-9._~-]+?))(?P<boundary>\. Use read with offset/limit|[\s)]|$)",
    )
}

fn local_spill_path_re() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    regex(
        &VALUE,
        r"(?P<path>\{\{cwd\}\}[\\/]\.spill[\\/]session-[0-9a-f]{12}[\\/][0-9a-f]{12}-(?P<name>[A-Za-z0-9._~-]+?))(?P<boundary>\. Use read with offset/limit|[\s)]|$)",
    )
}

fn cwd_rooted_path_re() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    regex(&VALUE, r#"\{\{cwd\}\}(?:[\\/][^\s<>"'`]+)+"#)
}

fn path_tag_re() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    regex(&VALUE, r"(<path>)([^<]*)(</path>)")
}

fn additional_instructions_path_re() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    regex(&VALUE, r"(Additional instructions from: )([^\r\n]+)")
}

fn event_read_target_region_re() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    regex(
        &VALUE,
        r"(?s)\A(?P<target>Session [^\r\n]+ — [^\r\n]+\r?\nTarget event seq \d+:\r?\n```json\r?\n\{\r?\n.*?)(?:\r?\n```(?:\r?\n|\z)|\r?\n\r?\n\(Omitted )",
    )
}

fn embedded_event_time_re() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    regex(&VALUE, r#"(?m)^(  "time": )\d+(,\r?)$"#)
}

fn event_read_omitted_bytes_re() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    regex(&VALUE, r"(\r?\n\r?\n\(Omitted )\d+( bytes\.)")
}

fn file_uri_path_prefix_re() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    regex(&VALUE, r"(?i)(?:^|[^a-z0-9+.-])file://(?:/)?$")
}

fn uuid_re() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    regex(
        &VALUE,
        r"(?i)[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
    )
}
