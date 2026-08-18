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
    parse_current_header_shape(&object)
}

fn parse_header_record(line: &str) -> anyhow::Result<SessionHeader> {
    let parsed: Value = serde_json::from_str(line)
        .map_err(|_| anyhow::anyhow!("corrupt session log: header line is not valid JSON"))?;
    if let Value::Object(object) = &parsed
        && let Some(version) = object.get("version")
        && version.is_number()
        && version.as_f64() != Some(f64::from(SESSION_FORMAT_VERSION))
    {
        let id = object
            .get("id")
            .map_or_else(|| "undefined".to_owned(), value_as_javascript_string);
        return Err(SessionFormatUnsupportedError::new(
            session_format_version_refusal(&id, version),
            None,
        )
        .into());
    }
    let Value::Object(object) = parsed else {
        anyhow::bail!("corrupt session log: first line is not a session header");
    };
    parse_current_header_shape(&object)?
        .ok_or_else(|| anyhow::anyhow!("corrupt session log: first line is not a session header"))
}

fn value_as_javascript_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(values) => values
            .iter()
            .map(|value| match value {
                Value::Null => String::new(),
                other => value_as_javascript_string(other),
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

fn parse_current_header_shape(
    object: &Map<String, Value>,
) -> anyhow::Result<Option<SessionHeader>> {
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
    let cwd = optional_string(object, "cwd")?;
    let parent_session = optional_string(object, "parentSession")?.map(SessionId::new);
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
    let agent_preset = optional_string(object, "agentPreset")?;
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

/// Incremental scanner progress before a recoverable torn-frame prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionLogCheckpoint {
    /// Total plaintext bytes supplied, including the header.
    pub input_bytes: usize,
    /// Bytes through the last safe complete record.
    pub committed_bytes: usize,
    /// Expanded contiguous event count.
    pub event_count: usize,
}

/// Incremental plaintext JSONL scanner used by streaming decoders.
pub struct SessionLogScanner {
    meta: SessionHeader,
    events: Vec<SessionEvent>,
    fragment: Vec<u8>,
    input_bytes: usize,
    committed_bytes: usize,
    event_line: usize,
    issue: Option<String>,
    finished: bool,
}

impl std::fmt::Debug for SessionLogScanner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionLogScanner")
            .field("meta", &self.meta)
            .field("event_count", &self.events.len())
            .field("fragment_bytes", &self.fragment.len())
            .field("input_bytes", &self.input_bytes)
            .field("committed_bytes", &self.committed_bytes)
            .field("event_line", &self.event_line)
            .field("issue", &self.issue)
            .field("finished", &self.finished)
            .finish()
    }
}

impl SessionLogScanner {
    /// Creates a scanner from exactly one newline-terminated header record.
    ///
    /// # Errors
    ///
    /// Returns header framing, corruption, or unsupported-version failures.
    pub fn new(header_record: &[u8]) -> anyhow::Result<Self> {
        if header_record.is_empty()
            || header_record.last() != Some(&b'\n')
            || header_record[..header_record.len() - 1].contains(&b'\n')
        {
            anyhow::bail!("empty or header-less session log");
        }
        let header = std::str::from_utf8(&header_record[..header_record.len() - 1])
            .map_err(|_| anyhow::anyhow!("corrupt session log: header line is not valid JSON"))?;
        let meta = parse_header_record(header)?;
        Ok(Self {
            meta,
            events: Vec::new(),
            fragment: Vec::new(),
            input_bytes: header_record.len(),
            committed_bytes: header_record.len(),
            event_line: 0,
            issue: None,
            finished: false,
        })
    }

    /// Consumes the next contiguous plaintext chunk.
    ///
    /// # Errors
    ///
    /// Returns when called after finish or when a later committed turn proves
    /// an earlier corrupt/gapped row belonged to committed history.
    pub fn write(&mut self, chunk: &[u8]) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.finished,
            "cannot write to a finished session log scanner"
        );
        let chunk_start = self.input_bytes;
        self.input_bytes = self.input_bytes.saturating_add(chunk.len());
        let mut line_start = 0;
        while let Some(relative_newline) =
            chunk[line_start..].iter().position(|byte| *byte == b'\n')
        {
            let newline = line_start + relative_newline;
            let line = if self.fragment.is_empty() {
                chunk[line_start..newline].to_vec()
            } else {
                self.fragment.extend_from_slice(&chunk[line_start..newline]);
                std::mem::take(&mut self.fragment)
            };
            self.consume_event_line(&line, chunk_start + newline + 1)?;
            line_start = newline + 1;
        }
        if line_start < chunk.len() {
            self.fragment.extend_from_slice(&chunk[line_start..]);
        }
        Ok(())
    }

    /// Snapshots byte and expanded-event progress.
    #[must_use]
    pub fn checkpoint(&self) -> SessionLogCheckpoint {
        SessionLogCheckpoint {
            input_bytes: self.input_bytes,
            committed_bytes: self.committed_bytes,
            event_count: self.events.len(),
        }
    }

    /// Finishes scanning, ignoring a final record without a newline.
    #[must_use]
    pub fn finish(&mut self) -> SessionLogScan {
        self.finished = true;
        SessionLogScan {
            meta: self.meta.clone(),
            events: self.events.clone(),
            committed_bytes: self.committed_bytes,
        }
    }

    fn consume_event_line(&mut self, line: &[u8], end_byte: usize) -> anyhow::Result<()> {
        self.event_line += 1;
        let decoded = (|| -> anyhow::Result<Vec<SessionEvent>> {
            let value: Value = serde_json::from_slice(line)?;
            decode_storage_record(value)?
                .into_iter()
                .map(|value| serde_json::from_value(value).map_err(Into::into))
                .collect()
        })();
        let Ok(decoded) = decoded else {
            self.issue.get_or_insert_with(|| {
                format!(
                    "corrupt session log: unparsable committed event at line {}",
                    self.event_line
                )
            });
            return Ok(());
        };
        if let Some(issue) = &self.issue {
            if decoded.iter().any(|event| event.event_type == "turn/end") {
                anyhow::bail!(issue.clone());
            }
            return Ok(());
        }

        let row_start = self.events.len();
        for event in &decoded {
            if event.seq != u64::try_from(self.events.len()).unwrap_or(u64::MAX) {
                let expected = self.events.len();
                self.events.truncate(row_start);
                self.issue = Some(format!(
                    "corrupt session log: seq gap in committed region at line {} (expected {expected}, got {})",
                    self.event_line, event.seq
                ));
                break;
            }
            self.events.push(event.clone());
        }
        if let Some(issue) = &self.issue {
            if decoded.iter().any(|event| event.event_type == "turn/end") {
                anyhow::bail!(issue.clone());
            }
        } else {
            self.committed_bytes = end_byte;
        }
        Ok(())
    }
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
    let mut scanner = SessionLogScanner::new(&buffer[..=header_end])?;
    scanner.write(&buffer[header_end + 1..])?;
    Ok(scanner.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_record(id: &str) -> Vec<u8> {
        format!(
            "{}\n",
            serde_json::json!({
                "type": "session",
                "version": 0,
                "id": id,
                "createdAt": 1,
                "delegationDepth": 0,
            })
        )
        .into_bytes()
    }

    fn event_value(event_type: &str, seq: u64, time: u64) -> Value {
        serde_json::json!({
            "type": event_type,
            "seq": seq,
            "time": time,
            "data": {"turn": 1},
        })
    }

    fn event(event_type: &str, seq: u64, time: u64) -> SessionEvent {
        serde_json::from_value(event_value(event_type, seq, time)).expect("event")
    }

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
    fn header_metadata_round_trips_while_log_open_refuses_foreign_version_first() {
        let mut header = SessionHeader::new(SessionId::new("id"));
        header.cwd = Some("/tmp/project".to_owned());
        header.delegation_depth = Some(3);
        header.agent_preset = Some("coding".to_owned());
        let line = header_line(&header).expect("serialize");
        assert_eq!(parse_header_meta(&line).expect("parse"), Some(header));
        let future =
            r#"{"type":"session","version":99,"id":"x","createdAt":1,"delegationDepth":0}"#;
        assert_eq!(
            parse_header_meta(future)
                .expect("future metadata")
                .expect("future header")
                .version,
            99
        );
        let error = scan_log(format!("{future}\n").as_bytes()).expect_err("foreign version");
        assert!(error.to_string().contains("upgrade the harness"));
        let older = scan_log(
            concat!(
                r#"{"type":"session","version":-1,"id":"x","createdAt":1,"delegationDepth":0}"#,
                "\n"
            )
            .as_bytes(),
        )
        .expect_err("older version");
        assert!(older.to_string().contains("ships no upgrade path"));
        let numeric =
            scan_log(concat!(r#"{"type":"session","version":42,"id":123}"#, "\n").as_bytes())
                .expect_err("numeric foreign id");
        assert!(
            numeric
                .to_string()
                .contains("session \"123\" uses log format v42")
        );
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

    #[test]
    fn incremental_scanner_handles_framing_fragments_checkpoints_and_finish() {
        let header = header_record("incremental");
        assert!(SessionLogScanner::new(&[]).is_err());
        assert!(SessionLogScanner::new(&header[..header.len() - 1]).is_err());
        let mut doubled = header.clone();
        doubled.extend_from_slice(&header);
        assert!(SessionLogScanner::new(&doubled).is_err());

        let first = serde_json::to_vec(&event_value("turn/start", 0, 1)).expect("first");
        let second = serde_json::to_vec(&serde_json::json!({
            "type": "user/message",
            "seq": 1,
            "time": 2,
            "data": {"text": "你好"},
        }))
        .expect("second");
        let mut body = first.clone();
        body.push(b'\n');
        body.extend_from_slice(&second);
        body.push(b'\n');
        body.extend_from_slice(b"ignored torn tail");
        let split = first.len()
            + 1
            + second
                .windows("你".len())
                .position(|window| window == "你".as_bytes())
                .expect("unicode marker")
            + 1;

        let mut scanner = SessionLogScanner::new(&header).expect("scanner");
        scanner.write(&[]).expect("empty write");
        scanner.write(&body[..split]).expect("first chunk");
        assert_eq!(
            scanner.checkpoint(),
            SessionLogCheckpoint {
                input_bytes: header.len() + split,
                committed_bytes: header.len() + first.len() + 1,
                event_count: 1,
            }
        );
        scanner.write(&body[split..]).expect("second chunk");
        let result = scanner.finish();
        assert_eq!(
            result
                .events
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            [0, 1]
        );
        assert_eq!(
            result.committed_bytes,
            header.len() + first.len() + second.len() + 2
        );
        assert!(scanner.write(b"\n").is_err());

        let mut complete = header;
        complete.extend_from_slice(&body);
        assert_eq!(result, scan_log(&complete).expect("compatibility scan"));
    }

    #[test]
    fn scanner_rejects_invalid_headers_and_preserves_header_only_logs() {
        for bytes in [b"".as_slice(), b"not json\n", b"{\"type\":\"event\"}\n"] {
            assert!(scan_log(bytes).is_err());
        }
        for header in [
            r#"{"type":"session","version":0,"id":"x","createdAt":1.5,"delegationDepth":0}"#,
            r#"{"type":"session","version":0,"id":"x","createdAt":-1,"delegationDepth":0}"#,
            r#"{"type":"session","version":0,"id":"x","createdAt":9007199254740992,"delegationDepth":0}"#,
            r#"{"type":"session","version":0,"id":"x","createdAt":-0,"delegationDepth":0}"#,
            r#"{"type":"session","version":0,"id":"x","createdAt":1}"#,
            r#"{"type":"session","version":0,"id":"x","createdAt":1,"delegationDepth":"1"}"#,
            r#"{"type":"session","version":0,"id":"x","createdAt":1,"delegationDepth":1.5}"#,
            r#"{"type":"session","version":0,"id":"x","createdAt":1,"delegationDepth":-1}"#,
            r#"{"type":"session","version":0,"id":"x","createdAt":1,"delegationDepth":-0}"#,
            r#"{"type":"session","version":0,"id":"x","createdAt":1,"delegationDepth":0,"agentPreset":7}"#,
        ] {
            assert!(
                scan_log(format!("{header}\n").as_bytes()).is_err(),
                "{header}"
            );
        }
        let header = header_record("header-only");
        let scanned = scan_log(&header).expect("header-only log");
        assert!(scanned.events.is_empty());
        assert_eq!(scanned.committed_bytes, header.len());
    }

    #[test]
    fn scanner_distinguishes_torn_tail_from_corrupt_committed_history() {
        let header = header_record("recovery");
        let start = serde_json::to_vec(&event_value("turn/start", 0, 1)).expect("start");
        let gap = serde_json::to_vec(&event_value("step/start", 2, 2)).expect("gap");
        let end = serde_json::to_vec(&event_value("turn/end", 3, 3)).expect("end");

        let mut tolerated = header.clone();
        tolerated.extend_from_slice(&start);
        tolerated.push(b'\n');
        let expected_commit = tolerated.len();
        tolerated.extend_from_slice(&gap);
        tolerated.push(b'\n');
        let scanned = scan_log(&tolerated).expect("tolerated gap");
        assert_eq!(
            scanned
                .events
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            [0]
        );
        assert_eq!(scanned.committed_bytes, expected_commit);

        let mut committed_gap = tolerated;
        committed_gap.extend_from_slice(&end);
        committed_gap.push(b'\n');
        assert!(
            scan_log(&committed_gap)
                .expect_err("committed gap")
                .to_string()
                .contains("seq gap in committed region")
        );

        let mut corrupt = header.clone();
        corrupt.extend_from_slice(b"{not json\n");
        corrupt.extend_from_slice(&serde_json::to_vec(&event_value("turn/end", 1, 2)).unwrap());
        corrupt.push(b'\n');
        assert!(
            scan_log(&corrupt)
                .expect_err("committed corruption")
                .to_string()
                .contains("unparsable committed event")
        );

        let mut completed_then_gap = header;
        completed_then_gap
            .extend_from_slice(&serde_json::to_vec(&event_value("turn/start", 0, 1)).unwrap());
        completed_then_gap.push(b'\n');
        completed_then_gap
            .extend_from_slice(&serde_json::to_vec(&event_value("turn/end", 1, 2)).unwrap());
        completed_then_gap.push(b'\n');
        completed_then_gap
            .extend_from_slice(&serde_json::to_vec(&event_value("step/start", 9, 3)).unwrap());
        completed_then_gap.push(b'\n');
        let scanned = scan_log(&completed_then_gap).expect("uncommitted tail");
        assert_eq!(
            scanned
                .events
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            [0, 1]
        );
    }

    #[test]
    fn packed_rows_advance_as_one_atomic_scan_record() {
        let header = header_record("packed");
        let start = event_value("turn/start", 0, 1);
        let packed = serde_json::json!({
            "type": "text-chunks",
            "seq0": 1,
            "time0": 2,
            "data": {"turn": 1, "step": 1, "index": 0, "dt": [1, 1], "texts": ["a", "b", "c"]},
        });
        let end = event_value("turn/end", 4, 5);
        let mut log = header.clone();
        for row in [&start, &packed, &end] {
            log.extend_from_slice(&serde_json::to_vec(row).expect("row"));
            log.push(b'\n');
        }
        let scanned = scan_log(&log).expect("packed scan");
        assert_eq!(
            scanned
                .events
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            [0, 1, 2, 3, 4]
        );
        assert_eq!(scanned.events[2].event_type, "assistant/chunk");
        assert_eq!(scanned.events[2].data["chunk"]["text"], "b");

        let malformed = serde_json::json!({
            "type": "text-chunks",
            "seq0": 0,
            "time0": 1,
            "data": {"turn": 1, "step": 1, "index": 0, "dt": [], "texts": ["a", "b"]},
        });
        let mut corrupt = header.clone();
        corrupt.extend_from_slice(&serde_json::to_vec(&malformed).unwrap());
        corrupt.push(b'\n');
        corrupt.extend_from_slice(&serde_json::to_vec(&event_value("turn/end", 2, 3)).unwrap());
        corrupt.push(b'\n');
        assert!(
            scan_log(&corrupt)
                .expect_err("malformed packed row")
                .to_string()
                .contains("unparsable committed event")
        );

        let events = [event("turn/start", 0, 1), event("turn/end", 1, 2)];
        assert_eq!(
            event_lines(&events, false).expect("unpacked lines"),
            events
                .iter()
                .map(|event| serde_json::to_string(event).expect("event line"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}
