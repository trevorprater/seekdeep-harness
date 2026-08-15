//! Lossless storage packing for consecutive assistant delta chunks.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use thiserror::Error;

use crate::session::SessionEvent;

const MIN_RUN: usize = 3;
const MAX_SAFE_INTEGER: i128 = 9_007_199_254_740_991;

/// Malformed durable packed-row diagnostic.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("malformed {tag} storage row: {reason}")]
pub struct ChunkRowError {
    tag: String,
    reason: String,
}

/// Shared payload for text and reasoning delta runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextRunData {
    /// Turn value copied from the event payload.
    pub turn: Value,
    /// Step value copied from the event payload.
    pub step: Value,
    /// Stream block index.
    pub index: Value,
    /// Timestamp gaps between members.
    pub dt: Vec<i64>,
    /// Exact unjoined token fragments.
    pub texts: Vec<String>,
}

/// Payload for tool-call argument delta runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCallRunData {
    /// Turn value copied from the event payload.
    pub turn: Value,
    /// Step value copied from the event payload.
    pub step: Value,
    /// Stream block index.
    pub index: Value,
    /// Provider call identity.
    pub id: String,
    /// Uniform optional tool name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Timestamp gaps between members.
    pub dt: Vec<i64>,
    /// Exact raw argument fragments.
    pub args: Vec<String>,
}

/// A compact durable representation of one consecutive delta run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ChunkRow {
    /// Text deltas.
    TextChunks {
        /// First event sequence.
        seq0: u64,
        /// First event timestamp.
        time0: i64,
        /// Run data.
        data: TextRunData,
    },
    /// Reasoning deltas.
    ReasoningChunks {
        /// First event sequence.
        seq0: u64,
        /// First event timestamp.
        time0: i64,
        /// Run data.
        data: TextRunData,
    },
    /// Tool-call argument deltas.
    ToolCallChunks {
        /// First event sequence.
        seq0: u64,
        /// First event timestamp.
        time0: i64,
        /// Run data.
        data: ToolCallRunData,
    },
}

/// One storage line: a verbatim event or a compact chunk row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StorageRecord {
    /// Ordinary session event.
    Event(SessionEvent),
    /// Packed delta run.
    ChunkRow(ChunkRow),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeltaKind {
    Text,
    Reasoning,
    ToolCall,
}

#[derive(Clone, Debug)]
struct DeltaMember {
    event: SessionEvent,
    kind: DeltaKind,
    turn: Value,
    step: Value,
    index: Value,
    payload: String,
    id: Option<String>,
    name: Option<String>,
    name_present: bool,
}

/// Packs every eligible run of at least three consecutive deltas.
#[must_use]
pub fn pack_chunk_runs(events: &[SessionEvent]) -> Vec<StorageRecord> {
    let mut output = Vec::new();
    let mut run: Vec<DeltaMember> = Vec::new();
    for event in events {
        let Some(member) = classify(event) else {
            flush_run(&mut output, &mut run);
            output.push(StorageRecord::Event(event.clone()));
            continue;
        };
        if run
            .last()
            .is_some_and(|previous| continues(previous, &member))
        {
            run.push(member);
        } else {
            flush_run(&mut output, &mut run);
            run.push(member);
        }
    }
    flush_run(&mut output, &mut run);
    output
}

fn flush_run(output: &mut Vec<StorageRecord>, run: &mut Vec<DeltaMember>) {
    if run.len() >= MIN_RUN {
        output.push(StorageRecord::ChunkRow(build_row(run)));
    } else {
        output.extend(
            run.iter()
                .map(|member| StorageRecord::Event(member.event.clone())),
        );
    }
    run.clear();
}

fn classify(event: &SessionEvent) -> Option<DeltaMember> {
    if event.event_type != "assistant/chunk"
        || event.surface_op.is_some()
        || event.source_event_seqs.is_some()
        || event.ignorable.is_some()
        || event.seq > u64::try_from(MAX_SAFE_INTEGER).ok()?
        || i128::from(event.time).unsigned_abs() > MAX_SAFE_INTEGER.unsigned_abs()
    {
        return None;
    }
    let data = exact_object(&event.data, &["turn", "step", "chunk"])?;
    let turn = data.get("turn")?.clone();
    let step = data.get("step")?.clone();
    if !turn.is_number() || !step.is_number() {
        return None;
    }
    let chunk = data.get("chunk")?.as_object()?;
    let kind = chunk.get("type")?.as_str()?;
    let index = chunk.get("index")?.clone();
    if !index.is_number() {
        return None;
    }
    let (kind, payload, id, name, name_present) = match kind {
        "text-delta" | "reasoning-delta" if has_exact_keys(chunk, &["type", "index", "text"]) => (
            if kind == "text-delta" {
                DeltaKind::Text
            } else {
                DeltaKind::Reasoning
            },
            chunk.get("text")?.as_str()?.to_owned(),
            None,
            None,
            false,
        ),
        "tool-call-delta" => {
            let name_present = chunk.contains_key("name");
            let keys = if name_present {
                &["type", "index", "id", "name", "argumentsDelta"][..]
            } else {
                &["type", "index", "id", "argumentsDelta"][..]
            };
            if !has_exact_keys(chunk, keys) {
                return None;
            }
            (
                DeltaKind::ToolCall,
                chunk.get("argumentsDelta")?.as_str()?.to_owned(),
                Some(chunk.get("id")?.as_str()?.to_owned()),
                if name_present {
                    Some(chunk.get("name")?.as_str()?.to_owned())
                } else {
                    None
                },
                name_present,
            )
        }
        _ => return None,
    };
    Some(DeltaMember {
        event: event.clone(),
        kind,
        turn,
        step,
        index,
        payload,
        id,
        name,
        name_present,
    })
}

fn continues(previous: &DeltaMember, next: &DeltaMember) -> bool {
    let gap = i128::from(next.event.time) - i128::from(previous.event.time);
    next.kind == previous.kind
        && next.event.seq == previous.event.seq.saturating_add(1)
        && gap.unsigned_abs() <= MAX_SAFE_INTEGER.unsigned_abs()
        && next.turn == previous.turn
        && next.step == previous.step
        && next.index == previous.index
        && (next.kind != DeltaKind::ToolCall
            || next.id == previous.id
                && next.name_present == previous.name_present
                && next.name == previous.name)
}

fn build_row(run: &[DeltaMember]) -> ChunkRow {
    let first = &run[0];
    let dt = run
        .windows(2)
        .map(|pair| pair[1].event.time - pair[0].event.time)
        .collect();
    match first.kind {
        DeltaKind::Text | DeltaKind::Reasoning => {
            let data = TextRunData {
                turn: first.turn.clone(),
                step: first.step.clone(),
                index: first.index.clone(),
                dt,
                texts: run.iter().map(|member| member.payload.clone()).collect(),
            };
            if first.kind == DeltaKind::Text {
                ChunkRow::TextChunks {
                    seq0: first.event.seq,
                    time0: first.event.time,
                    data,
                }
            } else {
                ChunkRow::ReasoningChunks {
                    seq0: first.event.seq,
                    time0: first.event.time,
                    data,
                }
            }
        }
        DeltaKind::ToolCall => ChunkRow::ToolCallChunks {
            seq0: first.event.seq,
            time0: first.event.time,
            data: ToolCallRunData {
                turn: first.turn.clone(),
                step: first.step.clone(),
                index: first.index.clone(),
                id: first.id.clone().expect("classified tool call has id"),
                name: first.name.clone(),
                dt,
                args: run.iter().map(|member| member.payload.clone()).collect(),
            },
        },
    }
}

/// Decodes one parsed storage-line value.
///
/// Non-row tags pass through without validation. A recognized but malformed
/// row fails rather than silently discarding a run.
///
/// # Errors
///
/// Returns [`ChunkRowError`] for malformed recognized rows.
pub fn decode_storage_record(value: Value) -> Result<Vec<Value>, ChunkRowError> {
    let Some(object) = value.as_object() else {
        return Ok(vec![value]);
    };
    let Some(tag) = object.get("type").and_then(Value::as_str) else {
        return Ok(vec![value]);
    };
    if !matches!(tag, "text-chunks" | "reasoning-chunks" | "tool-call-chunks") {
        return Ok(vec![value]);
    }
    validate_and_expand(object, tag)
}

fn validate_and_expand(
    object: &Map<String, Value>,
    tag: &str,
) -> Result<Vec<Value>, ChunkRowError> {
    if !has_exact_keys(object, &["type", "seq0", "time0", "data"]) {
        return malformed(tag, "envelope must be exactly {type, seq0, time0, data}");
    }
    let seq0 = safe_non_negative(object.get("seq0"))
        .ok_or_else(|| error(tag, "seq0 must be a non-negative safe integer"))?;
    let time0 = safe_integer(object.get("time0"))
        .ok_or_else(|| error(tag, "time0 must be a safe integer"))?;
    let data = object
        .get("data")
        .and_then(Value::as_object)
        .ok_or_else(|| error(tag, "data must be an object"))?;
    let tool = tag == "tool-call-chunks";
    let name_present = data.contains_key("name");
    let expected: &[&str] = if tool && name_present {
        &["turn", "step", "index", "id", "name", "dt", "args"]
    } else if tool {
        &["turn", "step", "index", "id", "dt", "args"]
    } else {
        &["turn", "step", "index", "dt", "texts"]
    };
    if !has_exact_keys(data, expected) {
        let shape = if tool {
            "data must be exactly {turn, step, index, id, name?, dt, args}"
        } else {
            "data must be exactly {turn, step, index, dt, texts}"
        };
        return malformed(tag, shape);
    }
    if !data.get("turn").is_some_and(Value::is_number)
        || !data.get("step").is_some_and(Value::is_number)
        || !data.get("index").is_some_and(Value::is_number)
    {
        return malformed(tag, "turn/step/index must be numbers");
    }
    if tool
        && (data.get("id").and_then(Value::as_str).is_none()
            || name_present && data.get("name").and_then(Value::as_str).is_none())
    {
        return malformed(tag, "id (and name when present) must be strings");
    }
    let payload_key = if tool { "args" } else { "texts" };
    let payload = data
        .get(payload_key)
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty() && items.iter().all(Value::is_string))
        .ok_or_else(|| {
            error(
                tag,
                &format!("{payload_key} must be a non-empty string array"),
            )
        })?;
    let gaps = data
        .get("dt")
        .and_then(Value::as_array)
        .ok_or_else(|| error(tag, "dt must be an array of safe integers"))?;
    let gaps = gaps
        .iter()
        .map(|gap| {
            safe_integer(Some(gap))
                .ok_or_else(|| error(tag, "dt must be an array of safe integers"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if gaps.len() != payload.len() - 1 {
        return malformed(
            tag,
            &format!(
                "dt length {} does not match {} members",
                gaps.len(),
                payload.len()
            ),
        );
    }
    let last_seq = i128::from(seq0) + i128::try_from(payload.len() - 1).unwrap_or(i128::MAX);
    if last_seq > MAX_SAFE_INTEGER {
        return malformed(tag, "member seqs must stay safe integers");
    }
    let mut time = i128::from(time0);
    for gap in &gaps {
        time += i128::from(*gap);
        if time.unsigned_abs() > MAX_SAFE_INTEGER.unsigned_abs() {
            return malformed(tag, "member times must stay safe integers");
        }
    }
    Ok(expand_row(
        data,
        tag,
        seq0,
        time0,
        payload,
        &gaps,
        name_present,
    ))
}

fn expand_row(
    data: &Map<String, Value>,
    tag: &str,
    seq0: u64,
    time0: i64,
    payload: &[Value],
    gaps: &[i64],
    name_present: bool,
) -> Vec<Value> {
    let mut events = Vec::with_capacity(payload.len());
    let mut time = time0;
    for (offset, item) in payload.iter().enumerate() {
        if offset > 0 {
            time += gaps[offset - 1];
        }
        let chunk = match tag {
            "text-chunks" => {
                json!({"type": "text-delta", "index": data["index"].clone(), "text": item})
            }
            "reasoning-chunks" => {
                json!({"type": "reasoning-delta", "index": data["index"].clone(), "text": item})
            }
            "tool-call-chunks" => {
                let mut chunk = Map::new();
                chunk.insert(
                    "type".to_owned(),
                    Value::String("tool-call-delta".to_owned()),
                );
                chunk.insert("index".to_owned(), data["index"].clone());
                chunk.insert("id".to_owned(), data["id"].clone());
                if name_present {
                    chunk.insert("name".to_owned(), data["name"].clone());
                }
                chunk.insert("argumentsDelta".to_owned(), item.clone());
                Value::Object(chunk)
            }
            _ => unreachable!("recognized row tag"),
        };
        events.push(json!({
            "type": "assistant/chunk",
            "seq": seq0 + u64::try_from(offset).unwrap_or(u64::MAX),
            "time": time,
            "data": {"turn": data["turn"].clone(), "step": data["step"].clone(), "chunk": chunk},
        }));
    }
    events
}

fn exact_object<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Map<String, Value>> {
    let object = value.as_object()?;
    has_exact_keys(object, keys).then_some(object)
}

fn has_exact_keys(object: &Map<String, Value>, keys: &[&str]) -> bool {
    object.len() == keys.len() && keys.iter().all(|key| object.contains_key(*key))
}

fn safe_non_negative(value: Option<&Value>) -> Option<u64> {
    value?
        .as_u64()
        .filter(|number| i128::from(*number) <= MAX_SAFE_INTEGER)
}

fn safe_integer(value: Option<&Value>) -> Option<i64> {
    let number = value?.as_i64()?;
    (i128::from(number).unsigned_abs() <= MAX_SAFE_INTEGER.unsigned_abs()).then_some(number)
}

fn error(tag: &str, reason: &str) -> ChunkRowError {
    ChunkRowError {
        tag: tag.to_owned(),
        reason: reason.to_owned(),
    }
}

fn malformed<T>(tag: &str, reason: &str) -> Result<T, ChunkRowError> {
    Err(error(tag, reason))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{SessionEvent, SessionId};

    fn event(seq: u64, time: i64, kind: &str, text: &str) -> SessionEvent {
        SessionEvent {
            event_type: "assistant/chunk".to_owned(),
            seq,
            time,
            data: json!({"turn": 1, "step": 1, "chunk": {"type": kind, "index": 0, "text": text}}),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        }
    }

    #[test]
    fn text_run_round_trips_exactly() {
        let events = (0..5)
            .map(|seq| {
                event(
                    seq,
                    1_000 + i64::try_from(seq).expect("small") * 10,
                    "text-delta",
                    &format!("t{seq}"),
                )
            })
            .collect::<Vec<_>>();
        let records = pack_chunk_runs(&events);
        assert_eq!(records.len(), 1);
        let value = serde_json::to_value(&records[0]).expect("row JSON");
        let decoded = decode_storage_record(value).expect("decode");
        let expected = events
            .iter()
            .map(|event| serde_json::to_value(event).expect("event JSON"))
            .collect::<Vec<_>>();
        assert_eq!(decoded, expected);
    }

    #[test]
    fn short_runs_remain_verbatim() {
        let events = vec![
            event(0, 1, "text-delta", "a"),
            event(1, 2, "text-delta", "b"),
        ];
        assert_eq!(
            pack_chunk_runs(&events),
            events
                .into_iter()
                .map(StorageRecord::Event)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn recognized_malformed_row_fails_loudly() {
        let error = decode_storage_record(
            json!({"type": "text-chunks", "seq0": -1, "time0": 1, "data": {}}),
        )
        .expect_err("malformed");
        assert!(
            error
                .to_string()
                .starts_with("malformed text-chunks storage row:")
        );
    }

    #[test]
    fn unrelated_values_pass_through() {
        let value = json!({"type": "turn/start", "session": SessionId::new("unused")});
        assert_eq!(
            decode_storage_record(value.clone()).expect("pass"),
            vec![value]
        );
    }
}
