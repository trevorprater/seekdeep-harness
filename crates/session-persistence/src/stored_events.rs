//! Compatibility migration for stored session events that predate current
//! message identities and the current React-loop envelopes.

use std::collections::{HashMap, HashSet};

use seekdeep_core::{
    known_event_types::KNOWN_SESSION_EVENT_TYPES,
    session::{JsonRef, JsonValue, Session, SessionEvent, SessionHeader, SessionId, SurfaceOp},
};
use serde_json::{Value, json};

mod raw_object;
use raw_object::{RawObject, object};

/// A stored envelope failed interpretation after its physical bytes were read.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct StoredEventValidationError(pub String);

/// Classifies cold preparation validation without relabeling physical I/O failures.
#[must_use]
pub fn classify_cold_validation(id: &SessionId, error: anyhow::Error) -> anyhow::Error {
    if error.is::<StoredEventValidationError>() {
        crate::SessionPersistenceCorruptionError(format!(
            "stored session \"{id}\" failed validation: Error: {error}"
        ))
        .into()
    } else {
        error
    }
}

/// Retains the source corruption classification for unreadable recovery fields.
#[must_use]
pub fn classify_cold_repair(id: &SessionId, error: &anyhow::Error) -> anyhow::Error {
    crate::SessionPersistenceCorruptionError(format!(
        "stored session \"{id}\" failed validation: TypeError: {error}"
    ))
    .into()
}

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// Upgrades every supported legacy record into the current in-memory shape.
/// The returned graph is detached from its input.
///
/// # Errors
///
/// Rejects retired or malformed legacy event shapes.
pub fn normalize_stored_events(
    events: &[SessionEvent],
    id: &SessionId,
) -> anyhow::Result<Vec<SessionEvent>> {
    normalize_events(events, id)
        .map_err(|error| StoredEventValidationError(error.to_string()).into())
}

fn normalize_events(events: &[SessionEvent], id: &SessionId) -> anyhow::Result<Vec<SessionEvent>> {
    assert_supported_events(events, id)?;
    let mut message_ids = HashMap::<u64, JsonValue>::new();
    let mut normalized = Vec::with_capacity(events.len());
    for event in events {
        let event = migrate_turn_start(event, id)?;
        let event = migrate_turn_end(&event, id)?;
        let event = migrate_steering(&event, id)?;
        let event = migrate_message(&event, id, &message_ids);
        if let Some(message_id) = event_message_id(&event) {
            message_ids.insert(event.seq, message_id);
        }
        normalized.push(event);
    }
    Ok(normalized)
}

/// Refuses event types this build cannot interpret unless explicitly marked
/// ignorable by the writer.
///
/// # Errors
///
/// Returns an unsupported-event diagnostic for the first required unknown type.
pub fn assert_known_events(events: &[SessionEvent], id: &SessionId) -> anyhow::Result<()> {
    for event in events {
        if KNOWN_SESSION_EVENT_TYPES.contains(event.event_type.as_str())
            || event.ignorable == Some(true)
        {
            continue;
        }
        anyhow::bail!(
            "session \"{id}\" contains event type \"{}\" (seq {}) unknown to this harness and not marked ignorable; refusing to interpret the log — it was likely written by a newer harness",
            event.event_type,
            event.seq
        );
    }
    Ok(())
}

/// Runs current session-envelope, message, and surface validation over a
/// normalized stored log without publishing it.
///
/// # Errors
///
/// Returns current session construction validation failures.
pub fn validate_normalized_events(
    meta: &SessionHeader,
    events: &[SessionEvent],
) -> anyhow::Result<()> {
    let _ =
        Session::create(&meta.id, Some(events.to_vec()), Some(meta.clone())).map_err(|error| {
            let mut message = error.to_string();
            for (index, event) in events.iter().enumerate() {
                message = message.replace(
                    &format!("seed {} at index {index}", event.event_type),
                    &format!("session event at seq {}", event.seq),
                );
            }
            StoredEventValidationError(message)
        })?;
    Ok(())
}

/// Rejects retired event forms without interpreting or normalizing accepted data.
///
/// # Errors
/// Preserves the source's retired-type priority and null header-data refusal.
pub fn assert_supported_events(events: &[SessionEvent], id: &SessionId) -> anyhow::Result<()> {
    for retired in ["request/header-delta", "mode/set"] {
        if let Some(event) = events.iter().find(|event| event.event_type == retired) {
            anyhow::bail!(
                "session \"{id}\" contains unsupported legacy {retired} event at seq {}",
                event.seq
            );
        }
    }
    for event in events {
        if event.event_type == "request/header" {
            anyhow::ensure!(
                !event.data.as_ref().is_null(),
                "Cannot read properties of null (reading 'reason')"
            );
            if event
                .data
                .get("reason")
                .and_then(|value| value.deserialize::<String>().ok())
                .as_deref()
                == Some("fallback")
            {
                anyhow::bail!(
                    "session \"{id}\" contains unsupported legacy request/header reason \"fallback\" at seq {}",
                    event.seq
                )
            }
        }
    }
    Ok(())
}

fn migrate_steering(event: &SessionEvent, id: &SessionId) -> anyhow::Result<SessionEvent> {
    if event.event_type != "steering/message" {
        return Ok(event.clone());
    }
    let Some(data) = RawObject::from_value(&event.data) else {
        return malformed(id, event, "steering/message");
    };
    if integer(data.get("turn")).is_some()
        && has_only_keys(&data, &["turn", "message"], &[])
        && data
            .get("message")
            .is_some_and(|value| value.as_ref().is_object())
    {
        let mut migrated = event.clone();
        "user/message".clone_into(&mut migrated.event_type);
        migrated.data = data["message"].clone();
        return Ok(migrated);
    }
    if integer(data.get("turn")).is_none()
        || !has_only_keys(&data, &["turn", "content", "source"], &[])
    {
        return malformed(id, event, "steering/message");
    }
    let mut message = data.clone();
    message.remove("turn");
    message.insert("id", Value::String(legacy_message_id(id, event.seq)));
    message.insert("role", Value::String("user".to_owned()));
    let mut migrated = event.clone();
    "user/message".clone_into(&mut migrated.event_type);
    migrated.data = message.into_value();
    Ok(migrated)
}

fn migrate_turn_start(event: &SessionEvent, id: &SessionId) -> anyhow::Result<SessionEvent> {
    if event.event_type != "turn/start" {
        return Ok(event.clone());
    }
    let Some(data) = RawObject::from_value(&event.data) else {
        return Ok(event.clone());
    };
    if !data.contains_key("trigger") {
        return Ok(event.clone());
    }
    let valid = positive_integer(data.get("turn")).is_some()
        && has_only_keys(&data, &["turn", "trigger"], &[])
        && data
            .get("trigger")
            .and_then(|trigger| trigger.get("kind"))
            .and_then(JsonRef::to_utf16)
            .is_some_and(|kind| !kind.is_empty());
    if !valid {
        return malformed(id, event, "turn/start");
    }
    let mut migrated = event.clone();
    migrated.data = object([("turn", data["turn"].clone())]);
    Ok(migrated)
}

#[allow(clippy::too_many_lines)]
fn migrate_turn_end(event: &SessionEvent, id: &SessionId) -> anyhow::Result<SessionEvent> {
    if event.event_type != "turn/end" {
        return Ok(event.clone());
    }
    let Some(data) = RawObject::from_value(&event.data) else {
        return Ok(event.clone());
    };
    let Some(reason) = data.get("reason").and_then(RawObject::from_value) else {
        return malformed(id, event, "turn/end");
    };
    if positive_integer(data.get("turn")).is_none()
        || !has_only_keys(&data, &["turn", "reason"], &[])
        || !reason.get("kind").is_some_and(is_string)
    {
        return malformed(id, event, "turn/end");
    }
    let kind = reason["kind"].deserialize::<String>().ok();
    let replacement = match kind.as_deref().unwrap_or_default() {
        "completed" | "blocked" | "max-tokens" | "interrupted" => {
            if !has_only_keys(&reason, &["kind"], &[]) {
                return malformed(id, event, "turn/end");
            }
            return Ok(event.clone());
        }
        "aborted" if reason.contains_key("reason") => return Ok(event.clone()),
        "aborted" => {
            if !has_only_keys(&reason, &["kind"], &[]) {
                return malformed(id, event, "turn/end");
            }
            JsonValue::from(json!({"kind": "aborted", "reason": {"kind": "legacy"}}))
        }
        "disposed" => {
            if !has_only_keys(&reason, &["kind"], &[]) {
                return malformed(id, event, "turn/end");
            }
            JsonValue::from(json!({"kind": "aborted", "reason": {"kind": "disposed"}}))
        }
        "error" if reason.contains_key("error") => return Ok(event.clone()),
        "error" => migrate_legacy_error_reason(&reason, id, event)?,
        _ => return Ok(event.clone()),
    };
    let mut migrated_data = data.clone();
    migrated_data.insert("reason", replacement);
    let mut migrated = event.clone();
    migrated.data = migrated_data.into_value();
    Ok(migrated)
}

fn migrate_legacy_error_reason(
    reason: &RawObject,
    id: &SessionId,
    event: &SessionEvent,
) -> anyhow::Result<JsonValue> {
    let valid_step = integer(reason.get("step")).is_some();
    if !valid_step {
        return malformed(id, event, "turn/end");
    }
    if let Some(failure) = reason.get("failure").and_then(RawObject::from_value) {
        let valid_failure = has_only_keys(reason, &["kind", "step", "failure"], &[])
            && has_only_keys(
                &failure,
                &["message", "code"],
                &["status", "providerRetryAfterMs", "requestId"],
            )
            && failure.get("message").is_some_and(is_string)
            && failure.get("code").is_some_and(is_string)
            && optional_number(&failure, "status")
            && optional_number(&failure, "providerRetryAfterMs")
            && optional_string(&failure, "requestId");
        if valid_failure {
            return Ok(object([
                ("kind", json!("error").into()),
                ("error", failure.into_value()),
            ]));
        }
    }
    let has_code = reason.contains_key("code");
    let required = if has_code {
        &["kind", "step", "message", "code"][..]
    } else {
        &["kind", "step", "message"][..]
    };
    if !has_only_keys(reason, required, &[])
        || !reason.get("message").is_some_and(is_string)
        || (has_code && !reason.get("code").is_some_and(is_string))
    {
        return malformed(id, event, "turn/end");
    }
    Ok(object([
        ("kind", json!("error").into()),
        (
            "error",
            object([
                ("message", reason["message"].clone()),
                (
                    "code",
                    reason
                        .get("code")
                        .cloned()
                        .unwrap_or_else(|| json!("UNKNOWN").into()),
                ),
            ]),
        ),
    ]))
}

fn has_legacy_message_shape(event: &SessionEvent) -> bool {
    let data = event.data.as_ref();
    match event.event_type.as_str() {
        "user/message" => {
            data.get("id").is_none()
                && data.get("role").is_none()
                && data.get("message").is_none()
                && data.get("content").is_some()
                && data.get("source").is_some()
        }
        "assistant/message" => {
            data.get("message").is_none()
                && data.get("content").is_some()
                && data.get("provenance").is_some()
        }
        "tool/result" => {
            data.get("message").is_none()
                && data.get("callId").is_some()
                && data.get("content").is_some()
                && data.get("isError").is_some()
        }
        _ => false,
    }
}

fn migrate_message(
    event: &SessionEvent,
    id: &SessionId,
    message_ids: &HashMap<u64, JsonValue>,
) -> SessionEvent {
    if !has_legacy_message_shape(event) {
        return event.clone();
    }
    let Some(data) = RawObject::from_value(&event.data) else {
        return event.clone();
    };
    match event.event_type.as_str() {
        "user/message" => {
            let mut message = data;
            message.insert("id", Value::String(legacy_message_id(id, event.seq)));
            message.insert("role", Value::String("user".to_owned()));
            replace_data(event, message.into_value())
        }
        "assistant/message" => {
            let mut event_data = data;
            let content = event_data
                .remove("content")
                .unwrap_or_else(|| Value::Null.into());
            let provenance = event_data
                .remove("provenance")
                .unwrap_or_else(|| Value::Null.into());
            let mut source = RawObject::from_value(&provenance).unwrap_or_default();
            source.insert("kind", Value::String("model".to_owned()));
            event_data.insert(
                "message",
                object([
                    ("id", json!(legacy_message_id(id, event.seq)).into()),
                    ("role", json!("assistant").into()),
                    ("content", content),
                    ("source", source.into_value()),
                ]),
            );
            replace_data(event, event_data.into_value())
        }
        "tool/result" => {
            let mut event_data = data;
            let call_id = event_data
                .remove("callId")
                .unwrap_or_else(|| Value::Null.into());
            let content = event_data
                .remove("content")
                .unwrap_or_else(|| Value::Null.into());
            let is_error = event_data
                .remove("isError")
                .unwrap_or_else(|| Value::Null.into());
            let inherited = replacement_start(event).and_then(|seq| message_ids.get(&seq));
            let mut message = RawObject::default();
            if let Some(message_id) = inherited {
                message.insert("id", message_id.clone());
            } else if replacement_start(event).is_none() {
                message.insert("id", Value::String(legacy_message_id(id, event.seq)));
            }
            message.insert("role", Value::String("user".to_owned()));
            message.insert(
                "content",
                JsonValue::array(&[object([
                    ("type", json!("tool-result").into()),
                    ("toolCallId", call_id.clone()),
                    ("content", content),
                    ("isError", is_error),
                ])]),
            );
            message.insert(
                "source",
                object([("kind", json!("tool").into()), ("callId", call_id)]),
            );
            event_data.insert("message", message.into_value());
            replace_data(event, event_data.into_value())
        }
        _ => event.clone(),
    }
}

fn replace_data(event: &SessionEvent, data: JsonValue) -> SessionEvent {
    let mut migrated = event.clone();
    migrated.data = data;
    migrated
}

fn event_message_id(event: &SessionEvent) -> Option<JsonValue> {
    let id = match event.event_type.as_str() {
        "user/message" => event.data.get("id"),
        "assistant/message" | "tool/result" => event
            .data
            .get("message")
            .and_then(|message| message.get("id")),
        _ => None,
    }?;
    id.is_string().then(|| id.to_owned())
}

fn replacement_start(event: &SessionEvent) -> Option<u64> {
    match &event.surface_op {
        Some(SurfaceOp::Replace(replacement)) if replacement.op == "replace" => {
            Some(replacement.start)
        }
        _ => None,
    }
}

fn legacy_message_id(id: &SessionId, seq: u64) -> String {
    format!("legacy-message:{id}:{seq}")
}

fn has_only_keys(object: &RawObject, required: &[&str], optional: &[&str]) -> bool {
    let allowed = required
        .iter()
        .chain(optional)
        .map(|key| key.encode_utf16().collect::<Vec<_>>())
        .collect::<HashSet<_>>();
    object.keys().all(|key| allowed.contains(key))
        && required.iter().all(|key| object.contains_key(key))
}

fn integer(value: Option<&JsonValue>) -> Option<i64> {
    let number = value?.as_ref().as_f64()?;
    if !number.is_finite()
        || number.fract() != 0.0
        || !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&number)
    {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)]
    Some(number as i64)
}

fn positive_integer(value: Option<&JsonValue>) -> Option<i64> {
    integer(value).filter(|value| *value >= 1)
}

fn optional_number(object: &RawObject, key: &str) -> bool {
    object
        .get(key)
        .is_none_or(|value| matches!(value.as_raw().as_bytes().first(), Some(b'-' | b'0'..=b'9')))
}

fn optional_string(object: &RawObject, key: &str) -> bool {
    object.get(key).is_none_or(is_string)
}

fn is_string(value: &JsonValue) -> bool {
    value.as_ref().is_string()
}

fn malformed<T>(id: &SessionId, event: &SessionEvent, kind: &str) -> anyhow::Result<T> {
    anyhow::bail!(
        "session \"{id}\" contains malformed pre-react-loop {kind} at seq {}",
        event.seq
    )
}

#[cfg(test)]
mod tests {
    use seekdeep_core::session::{SurfaceOp, SurfaceReplace};
    use serde_json::json;

    use super::*;

    fn event(event_type: &str, seq: u64, data: Value) -> SessionEvent {
        SessionEvent {
            event_type: event_type.to_owned(),
            seq,
            time: 1,
            data: data.into(),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        }
    }

    #[test]
    fn migrates_pre_react_loop_turn_and_steering_shapes() {
        let id = SessionId::new("legacy-loop");
        let mut steering = event(
            "steering/message",
            1,
            json!({
                "turn": 1,
                "content": [{"type": "text", "text": "steer"}],
                "source": {"kind": "user"}
            }),
        );
        steering.surface_op = Some(SurfaceOp::append());
        let events = normalize_stored_events(
            &[
                event(
                    "turn/start",
                    0,
                    json!({"turn": 1, "trigger": {"kind": "prompt"}}),
                ),
                steering,
                event(
                    "turn/end",
                    2,
                    json!({"turn": 1, "reason": {"kind": "disposed"}}),
                ),
            ],
            &id,
        )
        .expect("normalize");
        assert_eq!(events[0].data, json!({"turn": 1}));
        assert_eq!(events[1].event_type, "user/message");
        assert_eq!(
            events[1].data.as_serde_json().unwrap()["id"],
            "legacy-message:legacy-loop:1"
        );
        assert_eq!(
            events[2].data,
            json!({"turn": 1, "reason": {"kind": "aborted", "reason": {"kind": "disposed"}}})
        );
    }

    #[test]
    fn migrates_message_identities_and_preserves_replacement_identity() {
        let id = SessionId::new("legacy-messages");
        let mut user = event(
            "user/message",
            0,
            json!({"content": [], "source": {"kind": "user"}}),
        );
        user.surface_op = Some(SurfaceOp::append());
        let mut result = event(
            "tool/result",
            1,
            json!({"callId": "call", "content": [], "isError": false}),
        );
        result.surface_op = Some(SurfaceOp::Replace(SurfaceReplace {
            op: "replace".to_owned(),
            start: 0,
            end: 0,
        }));
        result.source_event_seqs = Some(vec![0]);
        let events = normalize_stored_events(&[user, result], &id).expect("normalize");
        assert_eq!(
            events[0].data.as_serde_json().unwrap()["id"],
            "legacy-message:legacy-messages:0"
        );
        assert_eq!(
            events[1].data.as_serde_json().unwrap()["message"]["id"],
            "legacy-message:legacy-messages:0"
        );
    }

    #[test]
    fn legacy_message_migration_preserves_text_and_arbitrary_utf16_metadata() {
        let id = SessionId::new("legacy-utf16");
        let mut legacy = event("tool/result", 0, Value::Null);
        legacy.data = JsonValue::parse(r#"{"callId":"call","content":[{"type":"text","text":"\ud800"}],"isError":false,"\ud800":{"\udfff":["😀","\\ud800"]}}"#.to_owned()).unwrap();
        legacy.surface_op = Some(SurfaceOp::append());
        let normalized = normalize_stored_events(&[legacy], &id).unwrap();
        let text = normalized[0]
            .data
            .pointer("/message/content/0/content/0/text")
            .unwrap();
        assert_eq!(text.to_utf16().unwrap(), [0xd800]);
        let entries = normalized[0].data.object_entries().unwrap();
        let metadata = entries
            .iter()
            .find(|(key, _)| key.to_utf16() == Some(vec![0xd800]))
            .unwrap()
            .1;
        assert_eq!(
            metadata.object_entries().unwrap()[0].0.to_utf16().unwrap(),
            [0xdfff]
        );
        let session = Session::create(&id, Some(normalized), None).unwrap();
        let messages = JsonValue::from_serialize(&session.derive_messages()).unwrap();
        assert_eq!(
            messages
                .pointer("/0/content/0/content/0/text")
                .unwrap()
                .to_utf16()
                .unwrap(),
            [0xd800]
        );
    }

    #[test]
    fn legacy_error_and_unknown_reason_strings_preserve_every_code_unit() {
        let id = SessionId::new("legacy-error-utf16");
        let mut legacy = event("turn/end", 0, Value::Null);
        legacy.data = JsonValue::parse(r#"{"turn":1,"reason":{"kind":"error","step":1,"failure":{"message":"\ud800","code":"\udfff","requestId":"\ud801"}}}"#.to_owned()).unwrap();
        let normalized = normalize_stored_events(&[legacy], &id).unwrap();
        assert_eq!(
            normalized[0]
                .data
                .pointer("/reason/error/message")
                .unwrap()
                .to_utf16()
                .unwrap(),
            [0xd800]
        );
        assert_eq!(
            normalized[0]
                .data
                .pointer("/reason/error/code")
                .unwrap()
                .to_utf16()
                .unwrap(),
            [0xdfff]
        );
        assert_eq!(
            normalized[0]
                .data
                .pointer("/reason/error/requestId")
                .unwrap()
                .to_utf16()
                .unwrap(),
            [0xd801]
        );

        let mut unknown = event("turn/end", 0, Value::Null);
        unknown.data =
            JsonValue::parse(r#"{"turn":1,"reason":{"kind":"\ud800"}}"#.to_owned()).unwrap();
        assert_eq!(
            normalize_stored_events(&[unknown.clone()], &id).unwrap(),
            [unknown]
        );
    }

    #[test]
    fn migrated_metadata_uses_javascript_own_key_order_and_last_duplicate_values() {
        let id = SessionId::new("legacy-keys");
        let mut legacy = event("tool/result", 0, Value::Null);
        legacy.data = JsonValue::parse(r#"{"callId":"call","content":[],"isError":false,"z":"first","10":"ten","2":"two","01":"leading","4294967295":"ordinary","4294967294":"last-index","\u007a":"last","\ud800":"lone"}"#.to_owned()).unwrap();
        let normalized = normalize_stored_events(&[legacy], &id).unwrap();
        assert_eq!(
            normalized[0].data.as_raw(),
            r#"{"2":"two","10":"ten","4294967294":"last-index","z":"last","01":"leading","4294967295":"ordinary","\ud800":"lone","message":{"id":"legacy-message:legacy-keys:0","role":"user","content":[{"type":"tool-result","toolCallId":"call","content":[],"isError":false}],"source":{"kind":"tool","callId":"call"}}}"#
        );
    }

    #[test]
    fn refuses_retired_unknown_required_and_malformed_legacy_events() {
        let id = SessionId::new("refusal");
        let retired = event("request/header-delta", 0, json!({}));
        assert!(
            normalize_stored_events(&[retired], &id)
                .expect_err("retired")
                .to_string()
                .contains("unsupported legacy")
        );
        let unknown = event("plugin/new", 0, json!({}));
        assert!(
            assert_known_events(&[unknown], &id)
                .expect_err("unknown")
                .to_string()
                .contains("not marked ignorable")
        );
        let malformed = event(
            "turn/end",
            0,
            json!({"turn": 1, "reason": {"kind": "completed", "extra": true}}),
        );
        assert!(
            normalize_stored_events(&[malformed], &id)
                .expect_err("malformed")
                .to_string()
                .contains("malformed pre-react-loop turn/end")
        );
    }
}
