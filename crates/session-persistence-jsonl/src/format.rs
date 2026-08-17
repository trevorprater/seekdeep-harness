//! JSONL artifact layout, UTF-16-compatible path encoding, and torn-tail scanning.

use std::path::{Path, PathBuf};

use seekdeep_core::{
    chunk_rows::{decode_storage_record, pack_chunk_runs},
    session::{SESSION_FORMAT_VERSION, SessionEvent, SessionHeader, SessionId, SessionOrigin},
};
use seekdeep_session_persistence::{SessionFormatUnsupportedError, session_format_version_refusal};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Physical JSONL artifact encoding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JsonlCompression {
    /// Checksummed concatenated Zstandard frames.
    #[default]
    Zstd,
    /// Plain UTF-8 JSON Lines.
    None,
}

impl JsonlCompression {
    /// Physical artifact suffix.
    #[must_use]
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::Zstd => ".jsonl.zstd",
            Self::None => ".jsonl",
        }
    }

    /// Opposite physical encoding.
    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::Zstd => Self::None,
            Self::None => Self::Zstd,
        }
    }
}

/// Encodes one non-empty string as an injective safe path segment using the
/// source implementation's UTF-16 code-unit escape format.
///
/// # Errors
///
/// Rejects the empty segment.
pub fn encode_segment(raw: &str) -> anyhow::Result<String> {
    encode_segment_units(&raw.encode_utf16().collect::<Vec<_>>())
}

/// Unit-level form retaining the source's defined behavior for lone UTF-16
/// surrogates, which a Rust `str` itself cannot contain.
///
/// # Errors
///
/// Rejects an empty unit slice.
pub fn encode_segment_units(units: &[u16]) -> anyhow::Result<String> {
    anyhow::ensure!(!units.is_empty(), "cannot encode an empty path segment");
    if units == [u16::from(b'.')] {
        return Ok("~002E".to_owned());
    }
    if units == [u16::from(b'.'), u16::from(b'.')] {
        return Ok("~002E~002E".to_owned());
    }
    let mut output = String::new();
    for unit in units {
        if safe_unit(*unit) {
            if let Some(character) = char::from_u32(u32::from(*unit)) {
                output.push(character);
            }
        } else {
            use std::fmt::Write as _;
            write!(output, "~{unit:04X}")?;
        }
    }
    Ok(output)
}

/// Builds the bounded human-readable project directory key.
///
/// # Errors
///
/// Rejects an empty project path.
pub fn project_key(cwd: &str) -> anyhow::Result<String> {
    anyhow::ensure!(!cwd.is_empty(), "cannot encode an empty project path");
    let mut readable = String::new();
    let mut separator_run = false;
    for unit in cwd.encode_utf16() {
        if separator_unit(unit) {
            if !separator_run {
                readable.push('-');
            }
            separator_run = true;
        } else if safe_unit(unit) {
            if let Some(character) = char::from_u32(u32::from(unit)) {
                readable.push(character);
            }
            separator_run = false;
        } else {
            use std::fmt::Write as _;
            write!(readable, "~{unit:04X}")?;
            separator_run = false;
        }
    }
    let slug = readable.trim_start_matches('-');
    let slug = if slug.is_empty() { "root" } else { slug };
    let bounded = &slug[..slug.len().min(251)];
    Ok(format!("--{bounded}--"))
}

fn safe_unit(unit: u16) -> bool {
    unit != u16::from(b'~')
        && ((u16::from(b'A')..=u16::from(b'Z')).contains(&unit)
            || (u16::from(b'a')..=u16::from(b'z')).contains(&unit)
            || (u16::from(b'0')..=u16::from(b'9')).contains(&unit)
            || matches!(unit, 0x002E | 0x005F | 0x002D))
}

fn separator_unit(unit: u16) -> bool {
    matches!(unit, 0x002F | 0x005C | 0x003A)
}

/// Project directory below the configured root.
///
/// # Errors
///
/// Returns invalid project-key failures.
pub fn project_dir(root: &Path, cwd: Option<&str>) -> anyhow::Result<PathBuf> {
    Ok(match cwd {
        Some(cwd) => root.join(project_key(cwd)?),
        None => root.join("_no-cwd"),
    })
}

/// Per-session directory below its project grouping.
///
/// # Errors
///
/// Returns path-segment or project-key failures.
pub fn session_dir(root: &Path, cwd: Option<&str>, id: &SessionId) -> anyhow::Result<PathBuf> {
    Ok(project_dir(root, cwd)?.join(encode_segment(id.as_str())?))
}

/// Exact physical artifact path.
///
/// # Errors
///
/// Returns path-segment or project-key failures.
pub fn log_path(
    root: &Path,
    cwd: Option<&str>,
    id: &SessionId,
    compression: JsonlCompression,
) -> anyhow::Result<PathBuf> {
    Ok(session_dir(root, cwd, id)?.join(format!("session{}", compression.suffix())))
}

/// Serializes one header line without its trailing newline.
///
/// # Errors
///
/// Returns JSON serialization failures.
pub fn header_line(header: &SessionHeader) -> anyhow::Result<String> {
    let mut line = Map::new();
    line.insert("type".to_owned(), Value::String("session".to_owned()));
    line.insert("version".to_owned(), Value::from(header.version));
    line.insert(
        "id".to_owned(),
        Value::String(header.id.as_str().to_owned()),
    );
    line.insert("createdAt".to_owned(), Value::from(header.created_at));
    if let Some(cwd) = &header.cwd {
        line.insert("cwd".to_owned(), Value::String(cwd.clone()));
    }
    if let Some(parent) = &header.parent_session {
        line.insert(
            "parentSession".to_owned(),
            Value::String(parent.as_str().to_owned()),
        );
    }
    if let Some(seed_length) = header.seed_length {
        line.insert("seedLength".to_owned(), Value::from(seed_length));
    }
    if let Some(origin) = header.origin {
        line.insert(
            "origin".to_owned(),
            Value::String(match origin {
                SessionOrigin::Subagent => "subagent".to_owned(),
            }),
        );
    }
    line.insert(
        "delegationDepth".to_owned(),
        Value::from(header.delegation_depth.unwrap_or(0)),
    );
    if let Some(agent_preset) = &header.agent_preset {
        line.insert(
            "agentPreset".to_owned(),
            Value::String(agent_preset.clone()),
        );
    }
    Ok(serde_json::to_string(&Value::Object(line))?)
}

/// Parses a header line, returning `None` for malformed current-format shape.
/// A foreign numeric version is refused before current-shape validation.
///
/// # Errors
///
/// Returns an unsupported-version error.
pub fn parse_header_meta(line: &str) -> anyhow::Result<Option<SessionHeader>> {
    let Ok(Value::Object(object)) = serde_json::from_str::<Value>(line) else {
        return Ok(None);
    };
    let version = object.get("version");
    if let Some(version) = version
        && version.is_number()
        && version.as_f64() != Some(f64::from(SESSION_FORMAT_VERSION))
    {
        let id = object.get("id").and_then(Value::as_str).unwrap_or_default();
        return Err(SessionFormatUnsupportedError::new(
            session_format_version_refusal(id, version),
            None,
        )
        .into());
    }
    if object.get("type").and_then(Value::as_str) != Some("session") {
        return Ok(None);
    }
    if object.contains_key("sandboxMode") || object.contains_key("approvalPolicy") {
        anyhow::bail!("session header uses retired policy baseline fields");
    }
    let Some(version) = object
        .get("version")
        .and_then(non_negative_integer)
        .and_then(|value| u32::try_from(value).ok())
    else {
        return Ok(None);
    };
    let Some(id) = object.get("id").and_then(Value::as_str) else {
        return Ok(None);
    };
    let Some(created_at) = safe_non_negative(object.get("createdAt")) else {
        return Ok(None);
    };
    let Some(delegation_depth) = safe_non_negative(object.get("delegationDepth")) else {
        return Ok(None);
    };
    let cwd = optional_string(&object, "cwd")?;
    let parent_session = optional_string(&object, "parentSession")?.map(SessionId::new);
    let seed_length = match object.get("seedLength") {
        Some(value) => Some(safe_non_negative(Some(value)).ok_or_else(|| {
            anyhow::anyhow!("session header seedLength must be a non-negative safe integer")
        })?),
        None => None,
    };
    let origin = match object.get("origin") {
        None => None,
        Some(Value::String(value)) if value == "subagent" => Some(SessionOrigin::Subagent),
        Some(_) => return Ok(None),
    };
    let agent_preset = optional_string(&object, "agentPreset")?;
    Ok(Some(SessionHeader {
        version,
        id: SessionId::new(id),
        created_at,
        cwd,
        parent_session,
        seed_length,
        origin,
        delegation_depth: Some(delegation_depth),
        agent_preset,
    }))
}

fn optional_string(object: &Map<String, Value>, key: &str) -> anyhow::Result<Option<String>> {
    match object.get(key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => anyhow::bail!("session header {key} must be a string"),
    }
}

fn safe_non_negative(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    let number = value.as_f64()?;
    if number == 0.0 && number.is_sign_negative() {
        return None;
    }
    non_negative_integer(value).filter(|value| *value <= 9_007_199_254_740_991)
}

fn non_negative_integer(value: &Value) -> Option<u64> {
    let number = value.as_f64()?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(number as u64)
}

/// Serializes an event batch without a trailing newline.
///
/// # Errors
///
/// Returns JSON serialization failures.
pub fn event_lines(events: &[SessionEvent], pack_chunks: bool) -> anyhow::Result<String> {
    let records = if pack_chunks {
        pack_chunk_runs(events)
    } else {
        events
            .iter()
            .cloned()
            .map(seekdeep_core::chunk_rows::StorageRecord::Event)
            .collect()
    };
    records
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .map(|lines| lines.join("\n"))
        .map_err(Into::into)
}

/// Valid committed prefix and safe byte truncation point.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionLogScan {
    /// Parsed immutable header.
    pub meta: SessionHeader,
    /// Expanded contiguous events.
    pub events: Vec<SessionEvent>,
    /// Bytes through the last safe newline-terminated record.
    pub committed_bytes: usize,
}

/// Scans a plaintext JSONL artifact, ignoring only a final record without a
/// newline and rejecting corruption proven to lie in a committed turn.
///
/// # Errors
///
/// Returns header, JSON, packed-row, or committed sequence corruption.
pub fn scan_log(buffer: &[u8]) -> anyhow::Result<SessionLogScan> {
    let Some(header_end) = buffer.iter().position(|byte| *byte == b'\n') else {
        anyhow::bail!("empty or header-less session log");
    };
    let header = std::str::from_utf8(&buffer[..header_end])
        .map_err(|_| anyhow::anyhow!("corrupt session log: header line is not valid JSON"))?;
    let meta = parse_header_meta(header)?.ok_or_else(|| {
        anyhow::anyhow!("corrupt session log: first line is not a session header")
    })?;
    let mut events = Vec::new();
    let mut issue: Option<anyhow::Error> = None;
    let mut committed_bytes = header_end + 1;
    let mut start = committed_bytes;
    let mut line_number = 0;
    while let Some(relative) = buffer[start..].iter().position(|byte| *byte == b'\n') {
        let end = start + relative;
        line_number += 1;
        let decoded = (|| -> anyhow::Result<Vec<SessionEvent>> {
            let value: Value = serde_json::from_slice(&buffer[start..end])?;
            decode_storage_record(value)?
                .into_iter()
                .map(|value| serde_json::from_value(value).map_err(Into::into))
                .collect()
        })();
        let Ok(decoded) = decoded else {
            issue.get_or_insert_with(|| {
                anyhow::anyhow!(
                    "corrupt session log: unparsable committed event at line {line_number}"
                )
            });
            start = end + 1;
            continue;
        };
        if let Some(error) = &issue {
            if decoded.iter().any(|event| event.event_type == "turn/end") {
                anyhow::bail!(error.to_string());
            }
            start = end + 1;
            continue;
        }
        let row_start = events.len();
        for event in &decoded {
            if event.seq != u64::try_from(events.len()).unwrap_or(u64::MAX) {
                let expected = events.len();
                events.truncate(row_start);
                issue = Some(anyhow::anyhow!(
                    "corrupt session log: seq gap in committed region at line {line_number} (expected {expected}, got {})",
                    event.seq
                ));
                break;
            }
            events.push(event.clone());
        }
        if let Some(error) = issue.as_ref() {
            if decoded.iter().any(|event| event.event_type == "turn/end") {
                anyhow::bail!(error.to_string());
            }
        } else {
            committed_bytes = end + 1;
        }
        start = end + 1;
    }
    Ok(SessionLogScan {
        meta,
        events,
        committed_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_encoding_matches_utf16_source_rules() {
        assert_eq!(encode_segment("abc._-09").expect("safe"), "abc._-09");
        assert_eq!(encode_segment("..").expect("dots"), "~002E~002E");
        assert_eq!(encode_segment("a/b~c").expect("escaped"), "a~002Fb~007Ec");
        assert_eq!(encode_segment("😀").expect("astral"), "~D83D~DE00");
        assert_eq!(
            encode_segment_units(&[0xD800]).expect("lone surrogate"),
            "~D800"
        );
        assert!(encode_segment("").is_err());
    }

    #[test]
    fn project_keys_collapse_separators_escape_and_bound() {
        assert_eq!(project_key("/a//b:c").expect("key"), "--a-b-c--");
        assert_eq!(project_key("/").expect("root"), "--root--");
        assert!(project_key("").is_err());
        assert!(project_key(&"a".repeat(400)).expect("bounded").len() <= 255);
    }

    #[test]
    fn header_round_trips_and_refuses_foreign_version_first() {
        let mut header = SessionHeader::new(SessionId::new("id"));
        header.cwd = Some("/tmp/project".to_owned());
        header.delegation_depth = Some(3);
        header.agent_preset = Some("coding".to_owned());
        let line = header_line(&header).expect("serialize");
        assert_eq!(parse_header_meta(&line).expect("parse"), Some(header));
        let error = parse_header_meta(r#"{"type":"future","version":99,"id":"x"}"#)
            .expect_err("foreign version");
        assert!(error.to_string().contains("upgrade the harness"));
        let older = parse_header_meta(r#"{"type":"past","version":-1,"id":"x"}"#)
            .expect_err("older version");
        assert!(older.to_string().contains("ships no upgrade path"));
        assert_eq!(
            parse_header_meta(r#"{"type":"session","version":"0","id":"x"}"#).expect("shape"),
            None
        );
        assert_eq!(
            parse_header_meta(
                r#"{"type":"session","version":0.0,"id":"x","createdAt":1,"delegationDepth":0}"#
            )
            .expect("numeric zero")
            .expect("header")
            .version,
            0
        );
        assert!(
            parse_header_meta(
                r#"{"type":"session","version":0,"id":"x","createdAt":-0,"delegationDepth":0}"#
            )
            .expect("negative zero")
            .is_none()
        );
    }
}
