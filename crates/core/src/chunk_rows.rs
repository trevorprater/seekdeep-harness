//! Lossless storage packing for consecutive assistant delta chunks.

use seekdeep_lossless_json::{JsonRef, JsonString, JsonValue};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use serde_json::Value;
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
    pub texts: Vec<JsonString>,
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
    /// Timestamp gaps between members.
    pub dt: Vec<i64>,
    /// Provider call identity.
    pub id: JsonString,
    /// Uniform optional tool name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<JsonString>,
    /// Exact raw argument fragments.
    pub args: Vec<JsonString>,
}

/// A compact durable representation of one consecutive delta run.
#[derive(Clone, Debug, PartialEq, Serialize)]
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
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum StorageRecord {
    /// Ordinary session event.
    Event(SessionEvent),
    /// Packed delta run.
    ChunkRow(ChunkRow),
}

impl<'de> Deserialize<'de> for StorageRecord {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <JsonValue as Deserialize>::deserialize(deserializer)?;
        let tag = value
            .get("type")
            .and_then(|value| value.deserialize::<String>().ok());
        if matches!(
            tag.as_deref(),
            Some("text-chunks" | "reasoning-chunks" | "tool-call-chunks")
        ) {
            value
                .deserialize()
                .map(Self::ChunkRow)
                .map_err(D::Error::custom)
        } else {
            value
                .deserialize()
                .map(Self::Event)
                .map_err(D::Error::custom)
        }
    }
}

impl<'de> Deserialize<'de> for ChunkRow {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <JsonValue as Deserialize>::deserialize(deserializer)?;
        let tag = value
            .get("type")
            .and_then(|value| value.deserialize::<String>().ok())
            .ok_or_else(|| D::Error::custom("chunk row must have a type"))?;
        let seq0 = value
            .get("seq0")
            .and_then(JsonRef::as_u64)
            .ok_or_else(|| D::Error::custom("chunk row must have seq0"))?;
        let time0 = value
            .get("time0")
            .and_then(JsonRef::as_i64)
            .ok_or_else(|| D::Error::custom("chunk row must have time0"))?;
        let data = value
            .get("data")
            .ok_or_else(|| D::Error::custom("chunk row must have data"))?;
        match tag.as_str() {
            "text-chunks" => data
                .deserialize()
                .map(|data| Self::TextChunks { seq0, time0, data })
                .map_err(D::Error::custom),
            "reasoning-chunks" => data
                .deserialize()
                .map(|data| Self::ReasoningChunks { seq0, time0, data })
                .map_err(D::Error::custom),
            "tool-call-chunks" => data
                .deserialize()
                .map(|data| Self::ToolCallChunks { seq0, time0, data })
                .map_err(D::Error::custom),
            _ => Err(D::Error::custom(format!("unknown chunk row type {tag}"))),
        }
    }
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
    payload: JsonString,
    id: Option<JsonString>,
    name: Option<JsonString>,
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
    let data = event.data.as_ref();
    if !has_exact_json_keys(data, &["turn", "step", "chunk"]) {
        return None;
    }
    let turn: Value = data.get("turn")?.deserialize().ok()?;
    let step: Value = data.get("step")?.deserialize().ok()?;
    if !turn.is_number() || !step.is_number() {
        return None;
    }
    let chunk = data.get("chunk").filter(|value| value.is_object())?;
    let kind = chunk.get("type")?.deserialize::<String>().ok()?;
    let index: Value = chunk.get("index")?.deserialize().ok()?;
    if !index.is_number() {
        return None;
    }
    let (kind, payload, id, name, name_present) = match kind.as_str() {
        "text-delta" | "reasoning-delta"
            if has_exact_json_keys(chunk, &["type", "index", "text"]) =>
        {
            (
                if kind == "text-delta" {
                    DeltaKind::Text
                } else {
                    DeltaKind::Reasoning
                },
                chunk.get("text")?.deserialize::<JsonString>().ok()?,
                None,
                None,
                false,
            )
        }
        "tool-call-delta" => {
            let name_present = chunk.get("name").is_some();
            let keys = if name_present {
                &["type", "index", "id", "name", "argumentsDelta"][..]
            } else {
                &["type", "index", "id", "argumentsDelta"][..]
            };
            if !has_exact_json_keys(chunk, keys) {
                return None;
            }
            (
                DeltaKind::ToolCall,
                chunk
                    .get("argumentsDelta")?
                    .deserialize::<JsonString>()
                    .ok()?,
                Some(chunk.get("id")?.deserialize::<JsonString>().ok()?),
                if name_present {
                    Some(chunk.get("name")?.deserialize::<JsonString>().ok()?)
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
#[expect(
    clippy::missing_panics_doc,
    reason = "expansion preserves the scalar string domain of the input"
)]
pub fn decode_storage_record(value: Value) -> Result<Vec<Value>, ChunkRowError> {
    decode_storage_record_json(value.into()).map(|events| {
        events
            .into_iter()
            .map(|event| {
                event
                    .try_into_serde_json()
                    .expect("Unicode-scalar input expands into Unicode-scalar events")
            })
            .collect()
    })
}

/// Decodes a storage record without narrowing strings or object keys to UTF-8.
///
/// # Errors
///
/// Returns [`ChunkRowError`] for malformed recognized rows.
pub fn decode_storage_record_json(value: JsonValue) -> Result<Vec<JsonValue>, ChunkRowError> {
    if !value.as_ref().is_object() {
        return Ok(vec![value]);
    }
    let Some(tag) = value
        .get("type")
        .and_then(|value| value.deserialize::<String>().ok())
    else {
        return Ok(vec![value]);
    };
    if !matches!(
        tag.as_str(),
        "text-chunks" | "reasoning-chunks" | "tool-call-chunks"
    ) {
        return Ok(vec![value]);
    }
    validate_and_expand(value.as_ref(), &tag)
}

fn validate_and_expand(object: JsonRef<'_>, tag: &str) -> Result<Vec<JsonValue>, ChunkRowError> {
    if !has_exact_json_keys(object, &["type", "seq0", "time0", "data"]) {
        return malformed(tag, "envelope must be exactly {type, seq0, time0, data}");
    }
    let seq0 = safe_non_negative(object.get("seq0"))
        .ok_or_else(|| error(tag, "seq0 must be a non-negative safe integer"))?;
    let time0 = safe_integer(object.get("time0"))
        .ok_or_else(|| error(tag, "time0 must be a safe integer"))?;
    let data = object
        .get("data")
        .filter(|value| value.is_object())
        .ok_or_else(|| error(tag, "data must be an object"))?;
    let tool = tag == "tool-call-chunks";
    let name_present = data.get("name").is_some();
    let expected: &[&str] = if tool && name_present {
        &["turn", "step", "index", "id", "name", "dt", "args"]
    } else if tool {
        &["turn", "step", "index", "id", "dt", "args"]
    } else {
        &["turn", "step", "index", "dt", "texts"]
    };
    if !has_exact_json_keys(data, expected) {
        let shape = if tool {
            "data must be exactly {turn, step, index, id, name?, dt, args}"
        } else {
            "data must be exactly {turn, step, index, dt, texts}"
        };
        return malformed(tag, shape);
    }
    if !data.get("turn").is_some_and(is_json_number)
        || !data.get("step").is_some_and(is_json_number)
        || !data.get("index").is_some_and(is_json_number)
    {
        return malformed(tag, "turn/step/index must be numbers");
    }
    if tool
        && (!data.get("id").is_some_and(JsonRef::is_string)
            || name_present && !data.get("name").is_some_and(JsonRef::is_string))
    {
        return malformed(tag, "id (and name when present) must be strings");
    }
    let payload_key = if tool { "args" } else { "texts" };
    let payload = data
        .get(payload_key)
        .and_then(JsonRef::array_items)
        .filter(|items| !items.is_empty() && items.iter().copied().all(JsonRef::is_string))
        .ok_or_else(|| {
            error(
                tag,
                &format!("{payload_key} must be a non-empty string array"),
            )
        })?;
    let gaps = data
        .get("dt")
        .and_then(JsonRef::array_items)
        .ok_or_else(|| error(tag, "dt must be an array of safe integers"))?;
    let gaps = gaps
        .iter()
        .map(|gap| {
            safe_integer(Some(*gap))
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
        &payload,
        &gaps,
        name_present,
    ))
}

fn expand_row(
    data: JsonRef<'_>,
    tag: &str,
    seq0: u64,
    time0: i64,
    payload: &[JsonRef<'_>],
    gaps: &[i64],
    name_present: bool,
) -> Vec<JsonValue> {
    let mut events = Vec::with_capacity(payload.len());
    let mut time = time0;
    for (offset, item) in payload.iter().enumerate() {
        if offset > 0 {
            time += gaps[offset - 1];
        }
        let mut chunk = indexmap::IndexMap::<&str, JsonValue>::new();
        let kind = match tag {
            "text-chunks" => "text-delta",
            "reasoning-chunks" => "reasoning-delta",
            "tool-call-chunks" => "tool-call-delta",
            _ => unreachable!("recognized row tag"),
        };
        chunk.insert("type", Value::String(kind.to_owned()).into());
        chunk.insert(
            "index",
            data.get("index").expect("validated index").to_owned(),
        );
        match tag {
            "text-chunks" | "reasoning-chunks" => {
                chunk.insert("text", (*item).to_owned());
            }
            "tool-call-chunks" => {
                chunk.insert("id", data.get("id").expect("validated id").to_owned());
                if name_present {
                    chunk.insert("name", data.get("name").expect("validated name").to_owned());
                }
                chunk.insert("argumentsDelta", (*item).to_owned());
            }
            _ => unreachable!("recognized row tag"),
        }
        let data = indexmap::IndexMap::from([
            ("turn", data.get("turn").expect("validated turn").to_owned()),
            ("step", data.get("step").expect("validated step").to_owned()),
            (
                "chunk",
                JsonValue::from_serialize(&chunk).expect("validated chunk serializes as JSON"),
            ),
        ]);
        let event = indexmap::IndexMap::from([
            ("type", Value::String("assistant/chunk".to_owned()).into()),
            (
                "seq",
                Value::from(seq0 + u64::try_from(offset).unwrap_or(u64::MAX)).into(),
            ),
            ("time", Value::from(time).into()),
            (
                "data",
                JsonValue::from_serialize(&data).expect("validated data serializes as JSON"),
            ),
        ]);
        events.push(JsonValue::from_serialize(&event).expect("validated event serializes as JSON"));
    }
    events
}

fn has_exact_json_keys(object: JsonRef<'_>, keys: &[&str]) -> bool {
    object.object_entries().is_some_and(|entries| {
        keys.iter().all(|key| object.get(key).is_some())
            && entries.iter().all(|(key, _)| {
                key.deserialize::<String>()
                    .ok()
                    .is_some_and(|key| keys.contains(&key.as_str()))
            })
    })
}

fn is_json_number(value: JsonRef<'_>) -> bool {
    matches!(value.as_raw().as_bytes().first(), Some(b'-' | b'0'..=b'9'))
}

fn safe_non_negative(value: Option<JsonRef<'_>>) -> Option<u64> {
    value?
        .as_u64()
        .filter(|number| i128::from(*number) <= MAX_SAFE_INTEGER)
}

fn safe_integer(value: Option<JsonRef<'_>>) -> Option<i64> {
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
    use serde_json::json;

    fn event(seq: u64, time: i64, kind: &str, text: &str) -> SessionEvent {
        SessionEvent {
            event_type: "assistant/chunk".to_owned(),
            seq,
            time,
            data: json!({"turn": 1, "step": 1, "chunk": {"type": kind, "index": 0, "text": text}})
                .into(),
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
    fn packed_text_preserves_lone_surrogates_through_both_storage_decoders() {
        let events = [r#""\ud800""#, r#""\udfff""#, r#""😀\\ud800""#]
            .into_iter()
            .enumerate()
            .map(|(index, text)| {
                serde_json::from_str::<SessionEvent>(&format!(
                    r#"{{"type":"assistant/chunk","seq":{index},"time":{index},"data":{{"turn":1,"step":1,"chunk":{{"type":"text-delta","index":0,"text":{text}}}}}}}"#
                )).unwrap()
            })
            .collect::<Vec<_>>();
        let records = pack_chunk_runs(&events);
        assert_eq!(records.len(), 1);
        assert!(matches!(records[0], StorageRecord::ChunkRow(_)));
        let encoded = serde_json::to_string(&records[0]).unwrap();
        assert!(encoded.contains(r#""texts":["\ud800","\udfff","😀\\ud800"]"#));
        let record: StorageRecord = serde_json::from_str(&encoded).unwrap();
        assert_eq!(record, records[0]);
        let decoded = decode_storage_record_json(JsonValue::parse(encoded).unwrap()).unwrap();
        let decoded = decoded
            .into_iter()
            .map(|event| event.deserialize::<SessionEvent>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(decoded, events);

        let encoded = serde_json::to_string(&events[0]).unwrap();
        let record: StorageRecord = serde_json::from_str(&encoded).unwrap();
        assert_eq!(record, StorageRecord::Event(events[0].clone()));
    }

    #[test]
    fn packed_tool_fragments_preserve_utf16_identifiers_and_argument_strings() {
        let raw = r#"{"type":"tool-call-chunks","seq0":0,"time0":1,"data":{"turn":1,"step":1,"index":0,"dt":[1,-1],"id":"\ud800","name":"\udfff","args":["\ud800","\\ud800","😀"]}}"#;
        let expanded =
            decode_storage_record_json(JsonValue::parse(raw.to_owned()).unwrap()).unwrap();
        let events = expanded
            .into_iter()
            .map(|event| event.deserialize::<SessionEvent>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            events[0]
                .data
                .pointer("/chunk/id")
                .unwrap()
                .to_utf16()
                .unwrap(),
            [0xd800]
        );
        assert_eq!(
            events[0]
                .data
                .pointer("/chunk/name")
                .unwrap()
                .to_utf16()
                .unwrap(),
            [0xdfff]
        );
        assert_eq!(
            events[0]
                .data
                .pointer("/chunk/argumentsDelta")
                .unwrap()
                .to_utf16()
                .unwrap(),
            [0xd800]
        );
        let packed = pack_chunk_runs(&events);
        assert_eq!(packed.len(), 1);
        assert_eq!(serde_json::to_string(&packed[0]).unwrap(), raw);
    }

    #[test]
    fn tool_call_row_serializes_in_the_source_canonical_field_order() {
        let events = ["a", "b", "c"]
            .into_iter()
            .enumerate()
            .map(|(offset, arguments_delta)| SessionEvent {
                event_type: "assistant/chunk".to_owned(),
                seq: u64::try_from(offset).unwrap(),
                time: 10 + i64::try_from(offset).unwrap(),
                data: json!({
                    "turn": 1,
                    "step": 1,
                    "chunk": {
                        "type": "tool-call-delta",
                        "index": 0,
                        "id": "call",
                        "name": "bash",
                        "argumentsDelta": arguments_delta
                    }
                })
                .into(),
                source_event_seqs: None,
                surface_op: None,
                ignorable: None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            serde_json::to_string(&pack_chunk_runs(&events)[0]).unwrap(),
            r#"{"type":"tool-call-chunks","seq0":0,"time0":10,"data":{"turn":1,"step":1,"index":0,"dt":[1,1],"id":"call","name":"bash","args":["a","b","c"]}}"#
        );
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
