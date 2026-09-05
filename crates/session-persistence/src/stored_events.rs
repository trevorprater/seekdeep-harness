//! Compatibility migration for stored session events that predate current
//! message identities and the current React-loop envelopes.

use std::collections::{HashMap, HashSet};

use seekdeep_core::{
    known_event_types::KNOWN_SESSION_EVENT_TYPES,
    session::{Session, SessionEvent, SessionHeader, SessionId, SurfaceOp},
};
use serde_json::{Map, Value, json};

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
    assert_no_retired_events(events, id)?;
    let mut message_ids = HashMap::<u64, String>::new();
    let mut normalized = Vec::with_capacity(events.len());
    for event in events {
        let event = migrate_turn_start(event, id)?;
        let event = migrate_turn_end(&event, id)?;
        let event = migrate_steering(&event, id)?;
        let event = migrate_message(&event, id, &message_ids);
        if let Some(message_id) = event_message_id(&event) {
            message_ids.insert(event.seq, message_id.to_owned());
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
    let _ = Session::create(&meta.id, Some(events.to_vec()), Some(meta.clone()))?;
    Ok(())
}

fn assert_no_retired_events(events: &[SessionEvent], id: &SessionId) -> anyhow::Result<()> {
    for event in events {
        match event.event_type.as_str() {
            "request/header-delta" => anyhow::bail!(
                "session \"{id}\" contains unsupported legacy request/header-delta event at seq {}",
                event.seq
            ),
            "mode/set" => anyhow::bail!(
                "session \"{id}\" contains unsupported legacy mode/set event at seq {}",
                event.seq
            ),
            "request/header"
                if event.data.get("reason").and_then(Value::as_str) == Some("fallback") =>
            {
                anyhow::bail!(
                    "session \"{id}\" contains unsupported legacy request/header reason \"fallback\" at seq {}",
                    event.seq
                )
            }
            _ => {}
        }
    }
    Ok(())
}

fn migrate_steering(event: &SessionEvent, id: &SessionId) -> anyhow::Result<SessionEvent> {
    if event.event_type != "steering/message" {
        return Ok(event.clone());
    }
    let Some(data) = event.data.as_object() else {
        return malformed(id, event, "steering/message");
    };
    if integer(data.get("turn")).is_some()
        && has_only_keys(data, &["turn", "message"], &[])
        && data.get("message").is_some_and(Value::is_object)
    {
        let mut migrated = event.clone();
        "user/message".clone_into(&mut migrated.event_type);
        migrated.data = data["message"].clone();
        return Ok(migrated);
    }
    if integer(data.get("turn")).is_none()
        || !has_only_keys(data, &["turn", "content", "source"], &[])
    {
        return malformed(id, event, "steering/message");
    }
    let mut message = data.clone();
    message.remove("turn");
    message.insert(
        "id".to_owned(),
        Value::String(legacy_message_id(id, event.seq)),
    );
    message.insert("role".to_owned(), Value::String("user".to_owned()));
    let mut migrated = event.clone();
    "user/message".clone_into(&mut migrated.event_type);
    migrated.data = Value::Object(message);
    Ok(migrated)
}

fn migrate_turn_start(event: &SessionEvent, id: &SessionId) -> anyhow::Result<SessionEvent> {
    if event.event_type != "turn/start" {
        return Ok(event.clone());
    }
    let Some(data) = event.data.as_object() else {
        return Ok(event.clone());
    };
    if !data.contains_key("trigger") {
        return Ok(event.clone());
    }
    let valid = positive_integer(data.get("turn")).is_some()
        && has_only_keys(data, &["turn", "trigger"], &[])
        && data
            .get("trigger")
            .and_then(Value::as_object)
            .and_then(|trigger| trigger.get("kind"))
            .and_then(Value::as_str)
            .is_some_and(|kind| !kind.is_empty());
    if !valid {
        return malformed(id, event, "turn/start");
    }
    let mut migrated = event.clone();
    migrated.data = json!({"turn": data["turn"].clone()});
    Ok(migrated)
}

#[allow(clippy::too_many_lines)]
fn migrate_turn_end(event: &SessionEvent, id: &SessionId) -> anyhow::Result<SessionEvent> {
    if event.event_type != "turn/end" {
        return Ok(event.clone());
    }
    let Some(data) = event.data.as_object() else {
        return Ok(event.clone());
    };
    let Some(reason) = data.get("reason").and_then(Value::as_object) else {
        return malformed(id, event, "turn/end");
    };
    if positive_integer(data.get("turn")).is_none()
        || !has_only_keys(data, &["turn", "reason"], &[])
        || reason.get("kind").and_then(Value::as_str).is_none()
    {
        return malformed(id, event, "turn/end");
    }
    let kind = reason["kind"].as_str().unwrap_or_default();
    let replacement = match kind {
        "completed" | "blocked" | "max-tokens" | "interrupted" => {
            if !has_only_keys(reason, &["kind"], &[]) {
                return malformed(id, event, "turn/end");
            }
            return Ok(event.clone());
        }
        "aborted" if reason.contains_key("reason") => return Ok(event.clone()),
        "aborted" => {
            if !has_only_keys(reason, &["kind"], &[]) {
                return malformed(id, event, "turn/end");
            }
            json!({"kind": "aborted", "reason": {"kind": "legacy"}})
        }
        "disposed" => {
            if !has_only_keys(reason, &["kind"], &[]) {
                return malformed(id, event, "turn/end");
            }
            json!({"kind": "aborted", "reason": {"kind": "disposed"}})
        }
        "error" if reason.contains_key("error") => return Ok(event.clone()),
        "error" => migrate_legacy_error_reason(reason, id, event)?,
        _ => return Ok(event.clone()),
    };
    let mut migrated_data = data.clone();
    migrated_data.insert("reason".to_owned(), replacement);
    let mut migrated = event.clone();
    migrated.data = Value::Object(migrated_data);
    Ok(migrated)
}

fn migrate_legacy_error_reason(
    reason: &Map<String, Value>,
    id: &SessionId,
    event: &SessionEvent,
) -> anyhow::Result<Value> {
    let valid_step = integer(reason.get("step")).is_some();
    if !valid_step {
        return malformed(id, event, "turn/end");
    }
    if let Some(failure) = reason.get("failure").and_then(Value::as_object) {
        let valid_failure = has_only_keys(reason, &["kind", "step", "failure"], &[])
            && has_only_keys(
                failure,
                &["message", "code"],
                &["status", "providerRetryAfterMs", "requestId"],
            )
            && failure.get("message").is_some_and(Value::is_string)
            && failure.get("code").is_some_and(Value::is_string)
            && optional_number(failure, "status")
            && optional_number(failure, "providerRetryAfterMs")
            && optional_string(failure, "requestId");
        if valid_failure {
            return Ok(json!({"kind": "error", "error": failure}));
        }
    }
    let has_code = reason.contains_key("code");
    let required = if has_code {
        &["kind", "step", "message", "code"][..]
    } else {
        &["kind", "step", "message"][..]
    };
    if !has_only_keys(reason, required, &[])
        || !reason.get("message").is_some_and(Value::is_string)
        || (has_code && !reason.get("code").is_some_and(Value::is_string))
    {
        return malformed(id, event, "turn/end");
    }
    Ok(json!({
        "kind": "error",
        "error": {
            "message": reason["message"].clone(),
            "code": reason.get("code").cloned().unwrap_or_else(|| Value::String("UNKNOWN".to_owned()))
        }
    }))
}

fn migrate_message(
    event: &SessionEvent,
    id: &SessionId,
    message_ids: &HashMap<u64, String>,
) -> SessionEvent {
    let Some(data) = event.data.as_object() else {
        return event.clone();
    };
    match event.event_type.as_str() {
        "user/message"
            if !data.contains_key("id")
                && !data.contains_key("role")
                && !data.contains_key("message")
                && data.contains_key("content")
                && data.contains_key("source") =>
        {
            let mut message = data.clone();
            message.insert(
                "id".to_owned(),
                Value::String(legacy_message_id(id, event.seq)),
            );
            message.insert("role".to_owned(), Value::String("user".to_owned()));
            replace_data(event, Value::Object(message))
        }
        "assistant/message"
            if !data.contains_key("message")
                && data.contains_key("content")
                && data.contains_key("provenance") =>
        {
            let mut event_data = data.clone();
            let content = event_data.remove("content").unwrap_or(Value::Null);
            let provenance = event_data.remove("provenance").unwrap_or(Value::Null);
            let mut source = provenance.as_object().cloned().unwrap_or_default();
            source.insert("kind".to_owned(), Value::String("model".to_owned()));
            event_data.insert(
                "message".to_owned(),
                json!({
                    "id": legacy_message_id(id, event.seq),
                    "role": "assistant",
                    "content": content,
                    "source": source,
                }),
            );
            replace_data(event, Value::Object(event_data))
        }
        "tool/result"
            if !data.contains_key("message")
                && data.contains_key("callId")
                && data.contains_key("content")
                && data.contains_key("isError") =>
        {
            let mut event_data = data.clone();
            let call_id = event_data.remove("callId").unwrap_or(Value::Null);
            let content = event_data.remove("content").unwrap_or(Value::Null);
            let is_error = event_data.remove("isError").unwrap_or(Value::Null);
            let inherited = replacement_start(event).and_then(|seq| message_ids.get(&seq));
            let mut message = Map::new();
            if let Some(message_id) = inherited {
                message.insert("id".to_owned(), Value::String(message_id.clone()));
            } else if replacement_start(event).is_none() {
                message.insert(
                    "id".to_owned(),
                    Value::String(legacy_message_id(id, event.seq)),
                );
            }
            message.insert("role".to_owned(), Value::String("user".to_owned()));
            message.insert(
                "content".to_owned(),
                json!([{
                    "type": "tool-result",
                    "toolCallId": call_id.clone(),
                    "content": content,
                    "isError": is_error,
                }]),
            );
            message.insert(
                "source".to_owned(),
                json!({"kind": "tool", "callId": call_id}),
            );
            event_data.insert("message".to_owned(), Value::Object(message));
            replace_data(event, Value::Object(event_data))
        }
        _ => event.clone(),
    }
}

fn replace_data(event: &SessionEvent, data: Value) -> SessionEvent {
    let mut migrated = event.clone();
    migrated.data = data;
    migrated
}

fn event_message_id(event: &SessionEvent) -> Option<&str> {
    match event.event_type.as_str() {
        "user/message" => event.data.get("id").and_then(Value::as_str),
        "assistant/message" | "tool/result" => event
            .data
            .get("message")
            .and_then(|message| message.get("id"))
            .and_then(Value::as_str),
        _ => None,
    }
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

fn has_only_keys(object: &Map<String, Value>, required: &[&str], optional: &[&str]) -> bool {
    let allowed = required
        .iter()
        .chain(optional)
        .copied()
        .collect::<HashSet<_>>();
    object.keys().all(|key| allowed.contains(key.as_str()))
        && required.iter().all(|key| object.contains_key(*key))
}

fn integer(value: Option<&Value>) -> Option<i64> {
    let number = value?.as_f64()?;
    if !number.is_finite()
        || number.fract() != 0.0
        || !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&number)
    {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)]
    Some(number as i64)
}

fn positive_integer(value: Option<&Value>) -> Option<i64> {
    integer(value).filter(|value| *value >= 1)
}

fn optional_number(object: &Map<String, Value>, key: &str) -> bool {
    object.get(key).is_none_or(Value::is_number)
}

fn optional_string(object: &Map<String, Value>, key: &str) -> bool {
    object.get(key).is_none_or(Value::is_string)
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
            data,
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
        assert_eq!(events[1].data["id"], "legacy-message:legacy-loop:1");
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
        assert_eq!(events[0].data["id"], "legacy-message:legacy-messages:0");
        assert_eq!(
            events[1].data["message"]["id"],
            "legacy-message:legacy-messages:0"
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
