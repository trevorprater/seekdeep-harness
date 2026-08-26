//! Canonical packed-row layout helpers for repository Session fixtures.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use seekdeep_core::{
    chunk_rows::{ChunkRow, StorageRecord, decode_storage_record, pack_chunk_runs},
    session::SessionEvent,
};
use serde_json::Value;

/// One repository Session fixture and its canonical packed representation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionFixtureLayout {
    /// Repository-relative path with `/` separators.
    pub path: String,
    /// Current UTF-8 bytes.
    pub source: String,
    /// Canonical packed UTF-8 bytes.
    pub canonical: String,
}

#[derive(Clone, Debug)]
struct RecordLine<'source> {
    line: usize,
    text: &'source str,
}

#[derive(Clone, Debug)]
struct DecodedEvent {
    event: SessionEvent,
    raw_text: Option<String>,
}

fn record_lines(content: &str) -> Vec<RecordLine<'_>> {
    content
        .lines()
        .enumerate()
        .filter_map(|(index, text)| {
            (!text.trim().is_empty()).then_some(RecordLine {
                line: index + 1,
                text,
            })
        })
        .collect()
}

fn parse_record(line: &RecordLine<'_>, label: &str) -> anyhow::Result<Value> {
    serde_json::from_str(line.text)
        .map_err(|error| anyhow::anyhow!("{label}:{}: invalid JSON: {error}", line.line))
}

fn decode_body_records(lines: &[RecordLine<'_>], label: &str) -> anyhow::Result<Vec<DecodedEvent>> {
    let mut events = Vec::new();
    for line in lines {
        let record = parse_record(line, label)?;
        let decoded = decode_storage_record(record.clone()).map_err(|error| {
            anyhow::anyhow!(
                "{label}:{}: invalid session storage record: {error}",
                line.line
            )
        })?;
        let verbatim =
            (decoded.len() == 1 && decoded.first() == Some(&record)).then(|| line.text.to_owned());
        for event in decoded {
            events.push(DecodedEvent {
                event: serde_json::from_value(event).map_err(|error| {
                    anyhow::anyhow!(
                        "{label}:{}: invalid session storage record: {error}",
                        line.line
                    )
                })?,
                raw_text: verbatim.clone(),
            });
        }
    }
    Ok(events)
}

#[cfg(test)]
fn decode_body(lines: &[RecordLine<'_>], label: &str) -> anyhow::Result<Vec<SessionEvent>> {
    Ok(decode_body_records(lines, label)?
        .into_iter()
        .map(|event| event.event)
        .collect())
}

fn render_fixture(header: &str, events: &[DecodedEvent]) -> anyhow::Result<String> {
    let mut rendered = String::new();
    rendered.push_str(header);
    rendered.push('\n');
    let logical = events
        .iter()
        .map(|event| event.event.clone())
        .collect::<Vec<_>>();
    let mut cursor = 0;
    for record in pack_chunk_runs(&logical) {
        let line = match &record {
            StorageRecord::Event(event) => {
                let source = events
                    .get(cursor)
                    .ok_or_else(|| anyhow::anyhow!("packed rewrite emitted an extra event"))?;
                anyhow::ensure!(source.event == *event, "packed rewrite reordered an event");
                cursor += 1;
                source
                    .raw_text
                    .clone()
                    .unwrap_or(render_json(&serde_json::to_value(event)?)?)
            }
            StorageRecord::ChunkRow(row) => {
                cursor += chunk_row_len(row);
                render_json(&serde_json::to_value(row)?)?
            }
        };
        rendered.push_str(&line);
        rendered.push('\n');
    }
    anyhow::ensure!(
        cursor == events.len(),
        "packed rewrite consumed {cursor}/{} events",
        events.len()
    );
    Ok(rendered)
}

fn chunk_row_len(row: &ChunkRow) -> usize {
    match row {
        ChunkRow::TextChunks { data, .. } | ChunkRow::ReasoningChunks { data, .. } => {
            data.texts.len()
        }
        ChunkRow::ToolCallChunks { data, .. } => data.args.len(),
    }
}

fn render_json(value: &Value) -> anyhow::Result<String> {
    fn write(value: &Value, output: &mut String) -> anyhow::Result<()> {
        match value {
            Value::Null => output.push_str("null"),
            Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
            Value::Number(value) => {
                if let Some(value) = value.as_i64() {
                    output.push_str(&value.to_string());
                } else if let Some(value) = value.as_u64() {
                    output.push_str(&value.to_string());
                } else {
                    let value = value
                        .as_f64()
                        .ok_or_else(|| anyhow::anyhow!("JSON number is not finite"))?;
                    output.push_str(ryu_js::Buffer::new().format_finite(value));
                }
            }
            Value::String(value) => output.push_str(&serde_json::to_string(value)?),
            Value::Array(values) => {
                output.push('[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        output.push(',');
                    }
                    write(value, output)?;
                }
                output.push(']');
            }
            Value::Object(values) => {
                output.push('{');
                for (index, (key, value)) in values.iter().enumerate() {
                    if index > 0 {
                        output.push(',');
                    }
                    output.push_str(&serde_json::to_string(key)?);
                    output.push(':');
                    write(value, output)?;
                }
                output.push('}');
            }
        }
        Ok(())
    }

    let mut output = String::new();
    write(value, &mut output)?;
    Ok(output)
}

/// Canonicalizes a JSONL document whose first record is a Session header.
///
/// The header line remains byte-identical. Non-Session JSONL returns `None`.
///
/// # Errors
///
/// Returns labeled body JSON, storage-row, event-envelope, losslessness, or
/// idempotence failures.
pub fn canonical_session_fixture(content: &str, label: &str) -> anyhow::Result<Option<String>> {
    let lines = record_lines(content);
    let Some(header) = lines.first() else {
        return Ok(None);
    };
    let Ok(header_value) = serde_json::from_str::<Value>(header.text) else {
        return Ok(None);
    };
    if header_value.get("type").and_then(Value::as_str) != Some("session") {
        return Ok(None);
    }
    let events = decode_body_records(&lines[1..], label)?;
    let canonical = render_fixture(header.text, &events)?;
    let canonical_lines = record_lines(&canonical);
    let decoded = decode_body_records(&canonical_lines[1..], label)?;
    let event_values = events.iter().map(|event| &event.event).collect::<Vec<_>>();
    let decoded_values = decoded.iter().map(|event| &event.event).collect::<Vec<_>>();
    anyhow::ensure!(
        decoded_values == event_values,
        "{label}: packed rewrite changed the decoded event stream"
    );
    anyhow::ensure!(
        render_fixture(header.text, &decoded)? == canonical,
        "{label}: packed rewrite is not idempotent"
    );
    Ok(Some(canonical))
}

fn discover_jsonl_files(root: &Path) -> anyhow::Result<Vec<String>> {
    let output = Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            "*.jsonl",
        ])
        .current_dir(root)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "git ls-files failed with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let mut paths = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8(path.to_vec()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.retain(|path| root.join(path).is_file());
    paths.sort();
    Ok(paths)
}

/// Inspects every tracked and unignored-untracked Session JSONL fixture.
///
/// # Errors
///
/// Returns Git, filesystem, UTF-8, or canonicalization failures.
pub fn inspect_session_fixture_layouts(root: &Path) -> anyhow::Result<Vec<SessionFixtureLayout>> {
    let mut fixtures = Vec::new();
    for path in discover_jsonl_files(root)? {
        let source = std::fs::read_to_string(root.join(&path))?;
        if let Some(canonical) = canonical_session_fixture(&source, &path)? {
            fixtures.push(SessionFixtureLayout {
                path,
                source,
                canonical,
            });
        }
    }
    Ok(fixtures)
}

/// Check or rewrite every repository Session fixture.
///
/// # Errors
///
/// Returns discovery/canonicalization/write failures, or a noncanonical check result.
pub fn run(root: &Path, rewrite: bool) -> anyhow::Result<()> {
    let fixtures = inspect_session_fixture_layouts(root)?;
    let changed = fixtures
        .iter()
        .filter(|fixture| fixture.source != fixture.canonical)
        .collect::<Vec<_>>();
    if rewrite {
        for fixture in &changed {
            std::fs::write(root.join(&fixture.path), &fixture.canonical)?;
            println!("{}", fixture.path);
        }
        println!(
            "packed session fixtures: {} rewritten, {} inspected",
            changed.len(),
            fixtures.len()
        );
        return Ok(());
    }
    anyhow::ensure!(
        changed.is_empty(),
        "noncanonical Session fixtures: {}\nRun `cargo xtask session-fixture-layout --rewrite` and commit the mechanical fixture rewrite.",
        changed
            .iter()
            .map(|fixture| fixture.path.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "packed session fixtures: 0 rewrites required, {} inspected",
        fixtures.len()
    );
    Ok(())
}

/// Resolves a repository-relative fixture path.
#[must_use]
pub fn fixture_path(root: &Path, fixture: &SessionFixtureLayout) -> PathBuf {
    root.join(&fixture.path)
}

#[cfg(test)]
mod tests {
    use seekdeep_core::session::SessionEvent;
    use serde_json::json;

    use super::*;

    const HEADER: &str =
        r#"  {"type":"session","version":0,"id":"fixture","createdAt":1,"delegationDepth":0}  "#;

    fn chunk_run() -> Vec<SessionEvent> {
        (0..4)
            .map(|index| SessionEvent {
                event_type: "assistant/chunk".to_owned(),
                seq: index,
                time: 10 + i64::try_from(index).unwrap(),
                data: json!({
                    "turn": 1,
                    "step": 1,
                    "chunk": {"type":"text-delta","index":0,"text":format!("part-{index}")}
                }),
                source_event_seqs: None,
                surface_op: None,
                ignorable: None,
            })
            .collect()
    }

    fn unpacked_fixture() -> String {
        let mut value = format!("{HEADER}\n");
        for event in chunk_run() {
            value.push_str(&serde_json::to_string(&event).unwrap());
            value.push('\n');
        }
        value
    }

    fn decoded_body(content: &str) -> Vec<SessionEvent> {
        decode_body(&record_lines(content)[1..], "fixture").unwrap()
    }

    #[test]
    fn preserves_header_and_packs_losslessly_and_idempotently() {
        let unpacked = unpacked_fixture();
        let canonical = canonical_session_fixture(&unpacked, "fixture.jsonl")
            .unwrap()
            .unwrap();
        assert_eq!(canonical.lines().next(), Some(HEADER));
        assert_eq!(
            serde_json::from_str::<Value>(canonical.lines().nth(1).unwrap()).unwrap()["type"],
            "text-chunks"
        );
        assert_eq!(decoded_body(&canonical), chunk_run());
        assert_eq!(
            canonical_session_fixture(&canonical, "fixture.jsonl")
                .unwrap()
                .as_deref(),
            Some(canonical.as_str())
        );
    }

    #[test]
    fn ignores_non_session_and_malformed_first_records() {
        assert!(
            canonical_session_fixture("{\"type\":\"session_event\"}\n", "other")
                .unwrap()
                .is_none()
        );
        assert!(
            canonical_session_fixture("{not-json}\n", "other")
                .unwrap()
                .is_none()
        );
        assert!(canonical_session_fixture("\n", "other").unwrap().is_none());
    }

    #[test]
    fn body_failures_name_the_fixture_and_physical_line() {
        let error = canonical_session_fixture(&format!("{HEADER}\n{{not-json}}\n"), "broken.jsonl")
            .unwrap_err();
        assert!(error.to_string().contains("broken.jsonl:2: invalid JSON"));
        let error = canonical_session_fixture(
            &format!("{HEADER}\n{{\"type\":\"text-chunks\"}}\n"),
            "broken.jsonl",
        )
        .unwrap_err();
        assert!(error.to_string().contains(
            "broken.jsonl:2: invalid session storage record: malformed text-chunks storage row"
        ));
    }

    #[test]
    fn canonical_json_preserves_object_order_and_verbatim_event_numbers() {
        let source = r#"{"surfaceOp":"append","sourceEventSeqs":[4]}"#;
        let value: Value = serde_json::from_str(source).unwrap();
        assert_eq!(render_json(&value).unwrap(), source);
        let event =
            r#"{"type":"hook/result","seq":0,"time":0,"data":{"durationMs":7.9223749999998745}}"#;
        let fixture = format!("{HEADER}\n{event}\n");
        assert_eq!(
            canonical_session_fixture(&fixture, "float.jsonl")
                .unwrap()
                .as_deref(),
            Some(fixture.as_str())
        );
    }

    #[test]
    fn git_discovery_is_sorted_and_rewrite_updates_only_session_jsonl() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            Command::new("git")
                .arg("init")
                .current_dir(root.path())
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(root.path().join("z.jsonl"), unpacked_fixture()).unwrap();
        std::fs::write(root.path().join("a.jsonl"), unpacked_fixture()).unwrap();
        std::fs::write(root.path().join("other.jsonl"), "{\"type\":\"other\"}\n").unwrap();
        assert!(
            Command::new("git")
                .args(["add", "z.jsonl"])
                .current_dir(root.path())
                .status()
                .unwrap()
                .success()
        );
        let fixtures = inspect_session_fixture_layouts(root.path()).unwrap();
        assert_eq!(
            fixtures
                .iter()
                .map(|fixture| fixture.path.as_str())
                .collect::<Vec<_>>(),
            ["a.jsonl", "z.jsonl"]
        );
        run(root.path(), true).unwrap();
        assert!(
            inspect_session_fixture_layouts(root.path())
                .unwrap()
                .iter()
                .all(|fixture| fixture.source == fixture.canonical)
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("other.jsonl")).unwrap(),
            "{\"type\":\"other\"}\n"
        );
    }

    #[test]
    fn repository_session_fixtures_are_canonical() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let noncanonical = inspect_session_fixture_layouts(root)
            .unwrap()
            .into_iter()
            .filter(|fixture| fixture.source != fixture.canonical)
            .map(|fixture| fixture.path)
            .collect::<Vec<_>>();
        assert!(noncanonical.is_empty(), "{noncanonical:?}");
    }
}
