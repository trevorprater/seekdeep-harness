//! Relational invariants over session execution events.

use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;
use seekdeep_cordis::{
    Context, DispatchMode, EventArgs, EventOptions, EventReply, fiber::EffectHandle,
};
use seekdeep_llm::CallId;
use thiserror::Error;

use crate::{
    repair::TOOL_NOT_STARTED,
    session::{Session, SessionEvent, SurfaceOp},
    session_store::SessionStore,
};

/// A relational execution-log invariant failed.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{0}")]
pub struct SessionInvariantError(String);

#[derive(Clone, Debug)]
struct SessionTrace {
    last_seq: Option<u64>,
    open_turn: Option<i64>,
    open_step: Option<i64>,
    next_turn: i64,
    next_step: i64,
    pending_calls: std::collections::HashSet<CallId>,
}

/// Relational balance after validating a detached event sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionEventBalance {
    /// Open turn number, when the log ends mid-turn.
    pub open_turn: Option<i64>,
    /// Open step number, when the log ends mid-step.
    pub open_step: Option<i64>,
    /// Number of model-requested tool calls lacking a durable result.
    pub pending_tool_calls: usize,
}

/// Validates a detached contiguous event sequence under the same relational
/// contract enforced for live appends.
///
/// # Errors
///
/// Returns the first sequence, turn, step, or tool-correlation violation.
pub fn validate_session_events(
    events: &[SessionEvent],
) -> Result<SessionEventBalance, SessionInvariantError> {
    validate_session_events_with(events, false)
}

/// Validates persisted structure while admitting model-only assistant surface rewrites.
///
/// Runtime invariant registration remains stricter: a live assistant event
/// must name its open step. Persistence accepts a replacement already
/// validated by [`Session`](crate::session::Session), because model-only
/// projection rewrites may be appended after their originating turn closes.
///
/// # Errors
///
/// Returns the same sequence, turn, step, and tool-call failures as
/// [`validate_session_events`], except for that persisted replacement case.
pub fn validate_persisted_session_events(
    events: &[SessionEvent],
) -> Result<SessionEventBalance, SessionInvariantError> {
    validate_session_events_with(events, true)
}

fn validate_session_events_with(
    events: &[SessionEvent],
    allow_assistant_replacements: bool,
) -> Result<SessionEventBalance, SessionInvariantError> {
    let mut trace = SessionTrace::default();
    for event in events {
        trace = validate_event(&trace, event, allow_assistant_replacements)?;
    }
    Ok(SessionEventBalance {
        open_turn: trace.open_turn,
        open_step: trace.open_step,
        pending_tool_calls: trace.pending_calls.len(),
    })
}

impl Default for SessionTrace {
    fn default() -> Self {
        Self {
            last_seq: None,
            open_turn: None,
            open_step: None,
            next_turn: 1,
            next_step: 1,
            pending_calls: std::collections::HashSet::new(),
        }
    }
}

/// Installs pre-commit validation and post-commit trace advancement.
///
/// # Errors
///
/// Returns when an existing live session violates the relational contract or
/// event-listener registration is rejected by an inactive context.
pub fn install_session_invariants(
    context: &Context,
    sessions: &SessionStore,
) -> anyhow::Result<Vec<EffectHandle>> {
    let traces = Arc::new(Mutex::new(HashMap::<usize, SessionTrace>::new()));
    let staged = Arc::new(Mutex::new(HashMap::<(usize, u64), SessionTrace>::new()));
    for session in sessions.list() {
        traces
            .lock()
            .insert(session_key(&session), seed_trace(&session)?);
    }

    let mut effects = Vec::new();
    let created_traces = traces.clone();
    effects.push(context.events().on_sync(
        context,
        "session/created",
        move |_, args| {
            let session = required_session(&args)?;
            created_traces
                .lock()
                .insert(session_key(&session), seed_trace(&session)?);
            Ok(EventReply::Undefined)
        },
        EventOptions {
            global: true,
            ..EventOptions::default()
        },
    )?);

    let validation_traces = traces.clone();
    let validation_staged = staged.clone();
    effects.push(context.events().on_sync(
        context,
        "internal/dispatch",
        move |_, args| {
            let mode = args.get::<DispatchMode>(0);
            let name = args.get::<String>(1);
            if mode.as_deref() != Some(&DispatchMode::Emit)
                || name.as_deref().map(String::as_str) != Some("session/event")
            {
                return Ok(EventReply::Undefined);
            }
            let event_args = args
                .get::<EventArgs>(2)
                .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks event arguments"))?;
            let session = required_session(&event_args)?;
            let event = event_args
                .get::<SessionEvent>(1)
                .ok_or_else(|| anyhow::anyhow!("session/event lacks an event"))?;
            let key = session_key(&session);
            let trace = validation_traces.lock().get(&key).cloned();
            let trace = match trace {
                Some(trace) => trace,
                None => seed_trace(&session)?,
            };
            let next = validate_event(&trace, &event, false)?;
            validation_staged.lock().insert((key, event.seq), next);
            Ok(EventReply::Undefined)
        },
        EventOptions {
            global: true,
            ..EventOptions::default()
        },
    )?);

    let commit_traces = traces;
    let commit_staged = staged;
    effects.push(context.events().on_sync(
        context,
        "session/event",
        move |_, args| {
            let session = required_session(&args)?;
            let event = args
                .get::<SessionEvent>(1)
                .ok_or_else(|| anyhow::anyhow!("session/event lacks an event"))?;
            let key = session_key(&session);
            let next = commit_staged
                .lock()
                .remove(&(key, event.seq))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "session/event reached publication without matching pre-commit validation"
                    )
                })?;
            commit_traces.lock().insert(key, next);
            Ok(EventReply::Undefined)
        },
        EventOptions {
            global: true,
            ..EventOptions::default()
        },
    )?);
    Ok(effects)
}

fn required_session(args: &EventArgs) -> anyhow::Result<Arc<Session>> {
    args.get::<Session>(0)
        .ok_or_else(|| anyhow::anyhow!("session event lacks a session"))
}

fn session_key(session: &Arc<Session>) -> usize {
    Arc::as_ptr(session) as usize
}

fn seed_trace(session: &Arc<Session>) -> anyhow::Result<SessionTrace> {
    let mut trace = SessionTrace::default();
    for event in session.events() {
        trace = validate_event(&trace, &event, false)?;
    }
    Ok(trace)
}

fn validate_event(
    current: &SessionTrace,
    event: &SessionEvent,
    allow_assistant_replacements: bool,
) -> Result<SessionTrace, SessionInvariantError> {
    if current.last_seq.is_some_and(|last| event.seq <= last) {
        return fail(format!(
            "seq must strictly increase: saw {} after {}",
            event.seq,
            current.last_seq.unwrap_or_default()
        ));
    }
    let mut next = current.clone();
    next.last_seq = Some(event.seq);
    match event.event_type.as_str() {
        "turn/start" => validate_turn_start(&mut next, event)?,
        "turn/end" => validate_turn_end(&mut next, event)?,
        "step/start" => validate_step_start(&mut next, event)?,
        "step/end" => validate_step_end(&mut next, event)?,
        "assistant/chunk" => {
            require_open_step(&next, &event.event_type, event)?;
        }
        "assistant/message"
            if allow_assistant_replacements
                && matches!(event.surface_op, Some(SurfaceOp::Replace(_))) => {}
        "assistant/message" => require_open_step(&next, &event.event_type, event)?,
        "tool/call" => validate_tool_call(&mut next, event)?,
        "tool/result" => validate_tool_result(&mut next, event)?,
        "todo/write" | "request/header" | "request/context" => {
            if next.open_turn.is_none() {
                return fail(format!(
                    "{} appended outside any open turn (core execution events must be turn-enclosed)",
                    event.event_type
                ));
            }
        }
        _ => {}
    }
    Ok(next)
}

fn validate_turn_start(
    trace: &mut SessionTrace,
    event: &SessionEvent,
) -> Result<(), SessionInvariantError> {
    let turn = field(event, "turn")?;
    if let Some(open) = trace.open_turn {
        return fail(format!("turn/start {turn} while turn {open} is still open"));
    }
    if turn != trace.next_turn {
        return fail(format!(
            "turn/start expected turn {}, got {turn}",
            trace.next_turn
        ));
    }
    trace.open_turn = Some(turn);
    trace.next_step = 1;
    Ok(())
}

fn validate_turn_end(
    trace: &mut SessionTrace,
    event: &SessionEvent,
) -> Result<(), SessionInvariantError> {
    let turn = field(event, "turn")?;
    if trace.open_turn != Some(turn) {
        return fail(format!(
            "turn/end {turn} does not match open turn {}",
            optional_number(trace.open_turn)
        ));
    }
    if let Some(step) = trace.open_step {
        return fail(format!("turn/end {turn} while step {step} is still open"));
    }
    trace.open_turn = None;
    trace.next_turn += 1;
    Ok(())
}

fn validate_step_start(
    trace: &mut SessionTrace,
    event: &SessionEvent,
) -> Result<(), SessionInvariantError> {
    let turn = field(event, "turn")?;
    let step = field(event, "step")?;
    if trace.open_turn != Some(turn) {
        return fail(format!(
            "step/start in turn {turn} but open turn is {}",
            optional_number(trace.open_turn)
        ));
    }
    if let Some(open) = trace.open_step {
        return fail(format!("step/start {step} while step {open} is still open"));
    }
    if step != trace.next_step {
        return fail(format!(
            "step/start expected step {} in turn {turn}, got {step}",
            trace.next_step
        ));
    }
    trace.open_step = Some(step);
    Ok(())
}

fn validate_step_end(
    trace: &mut SessionTrace,
    event: &SessionEvent,
) -> Result<(), SessionInvariantError> {
    require_open_step(trace, "step/end", event)?;
    trace.pending_calls.clear();
    trace.open_step = None;
    trace.next_step += 1;
    Ok(())
}

fn validate_tool_call(
    trace: &mut SessionTrace,
    event: &SessionEvent,
) -> Result<(), SessionInvariantError> {
    require_open_step(trace, "tool/call", event)?;
    let call_id = event
        .data
        .get("callId")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| SessionInvariantError("tool/call lacks callId".to_owned()))?;
    trace.pending_calls.insert(CallId::new(call_id));
    Ok(())
}

fn validate_tool_result(
    trace: &mut SessionTrace,
    event: &SessionEvent,
) -> Result<(), SessionInvariantError> {
    if !matches!(event.surface_op, Some(SurfaceOp::Marker(ref marker)) if marker == "append") {
        if trace.open_turn.is_none() {
            return fail("tool/result surface replacement appended outside any open turn");
        }
        return Ok(());
    }
    require_open_step(trace, "tool/result", event)?;
    let call_id = event
        .data
        .pointer("/message/source/callId")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| SessionInvariantError("tool/result lacks source callId".to_owned()))?;
    let call_id = CallId::new(call_id);
    let synthetic_not_started = event
        .data
        .pointer("/message/content/0/isError")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
        && event
            .data
            .pointer("/error/code")
            .and_then(serde_json::Value::as_str)
            == Some(TOOL_NOT_STARTED);
    if !trace.pending_calls.contains(&call_id) && !synthetic_not_started {
        return fail(format!(
            "tool/result for {call_id} with no prior tool/call in this step"
        ));
    }
    trace.pending_calls.remove(&call_id);
    Ok(())
}

fn require_open_step(
    trace: &SessionTrace,
    kind: &str,
    event: &SessionEvent,
) -> Result<(), SessionInvariantError> {
    let turn = field(event, "turn")?;
    let step = field(event, "step")?;
    if trace.open_turn != Some(turn) || trace.open_step != Some(step) {
        return fail(format!(
            "{kind} names turn {turn}/step {step} but open is turn {}/step {}",
            optional_number(trace.open_turn),
            optional_number(trace.open_step)
        ));
    }
    Ok(())
}

fn field(event: &SessionEvent, name: &str) -> Result<i64, SessionInvariantError> {
    event
        .data
        .get(name)
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| SessionInvariantError(format!("{} lacks numeric {name}", event.event_type)))
}

fn optional_number(value: Option<i64>) -> String {
    value.map_or_else(|| "null".to_owned(), |value| value.to_string())
}

fn fail<T>(message: impl Into<String>) -> Result<T, SessionInvariantError> {
    Err(SessionInvariantError(message.into()))
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::{
        session::{AppendOptions, SessionId},
        session_store::{CreateSessionOptions, SessionStore},
    };

    fn event(event_type: &str, seq: u64, data: Value) -> SessionEvent {
        SessionEvent {
            event_type: event_type.to_owned(),
            seq,
            time: i64::try_from(seq).expect("small seq"),
            data,
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        }
    }

    fn surface_event(event_type: &str, seq: u64, data: Value) -> SessionEvent {
        let mut event = event(event_type, seq, data);
        event.surface_op = Some(SurfaceOp::append());
        event
    }

    fn tool_result(call_id: &str, is_error: bool) -> Value {
        json!({
            "turn": 1,
            "step": 1,
            "message": {
                "source": {"kind": "tool", "callId": call_id},
                "content": [{"type": "tool-result", "toolCallId": call_id, "isError": is_error, "content": []}],
            },
        })
    }

    fn not_started_result(call_id: &str) -> Value {
        json!({
            "turn": 1,
            "step": 1,
            "message": {
                "source": {"kind": "tool", "callId": call_id},
                "content": [{"type": "tool-result", "toolCallId": call_id, "isError": true, "content": []}],
            },
            "error": {"name": "ToolNotStartedError", "code": TOOL_NOT_STARTED},
        })
    }

    #[test]
    fn accepts_a_well_formed_turn_step_and_tool_sequence() {
        let events = vec![
            event("turn/start", 0, json!({"turn": 1})),
            surface_event(
                "user/message",
                1,
                json!({"id": "u", "role": "user", "source": {"kind": "user"}, "content": [{"type": "text", "text": "hi"}]}),
            ),
            event("step/start", 2, json!({"turn": 1, "step": 1})),
            event(
                "assistant/chunk",
                3,
                json!({"turn": 1, "step": 1, "chunk": {"type": "text-delta", "index": 0, "text": "h"}}),
            ),
            surface_event(
                "assistant/message",
                4,
                json!({"turn": 1, "step": 1, "message": {"id": "m", "role": "assistant", "source": {"kind": "model", "provider": "mock", "model": "mock"}, "content": [{"type": "tool-call", "id": "c1", "name": "echo", "arguments": "{}"}]}}),
            ),
            event(
                "tool/call",
                5,
                json!({"turn": 1, "step": 1, "callId": "c1", "name": "echo", "arguments": "{}"}),
            ),
            surface_event("tool/result", 6, tool_result("c1", false)),
            event("step/end", 7, json!({"turn": 1, "step": 1})),
            event(
                "turn/end",
                8,
                json!({"turn": 1, "reason": {"kind": "completed"}}),
            ),
        ];
        let balance = validate_session_events(&events).expect("valid trace");
        assert_eq!(
            balance,
            SessionEventBalance {
                open_turn: None,
                open_step: None,
                pending_tool_calls: 0,
            }
        );
    }

    #[test]
    fn rejects_non_monotonic_event_sequence_numbers() {
        let events = vec![
            event("turn/start", 0, json!({"turn": 1})),
            event(
                "turn/end",
                0,
                json!({"turn": 1, "reason": {"kind": "completed"}}),
            ),
        ];
        let error = validate_session_events(&events).expect_err("non-monotonic seq");
        assert!(error.to_string().contains("seq must strictly increase"));
    }

    #[test]
    fn enforces_turn_numbering() {
        let nested = validate_session_events(&[
            event("turn/start", 0, json!({"turn": 1})),
            event("turn/start", 1, json!({"turn": 2})),
        ])
        .expect_err("nested turn");
        assert!(nested.to_string().contains("still open"));

        let mismatched = validate_session_events(&[
            event("turn/start", 0, json!({"turn": 1})),
            event(
                "turn/end",
                1,
                json!({"turn": 2, "reason": {"kind": "completed"}}),
            ),
        ])
        .expect_err("mismatched turn end");
        assert!(
            mismatched
                .to_string()
                .contains("does not match open turn 1")
        );

        let skipped = validate_session_events(&[
            event("turn/start", 0, json!({"turn": 1})),
            event(
                "turn/end",
                1,
                json!({"turn": 1, "reason": {"kind": "completed"}}),
            ),
            event("turn/start", 2, json!({"turn": 3})),
        ])
        .expect_err("skipped turn");
        assert!(skipped.to_string().contains("expected turn 2"));
    }

    #[test]
    fn requires_core_execution_enclosure() {
        let outside = validate_session_events(&[event(
            "request/context",
            0,
            json!({"provider": "mock", "model": "m"}),
        )])
        .expect_err("request/context outside turn");
        assert!(outside.to_string().contains("outside any open turn"));

        validate_session_events(&[
            event("turn/start", 0, json!({"turn": 1})),
            event("step/start", 1, json!({"turn": 1, "step": 1})),
            event("todo/write", 2, json!({"todos": []})),
            event(
                "request/header",
                3,
                json!({"header": {"config": {"provider": "mock", "model": "mock"}}, "reason": "initial"}),
            ),
            event("request/context", 4, json!({"provider": "mock", "model": "mock"})),
        ])
        .expect("turn-enclosed execution events");
    }

    #[test]
    fn enforces_open_step_identity_and_numbering() {
        let wrong_turn = validate_session_events(&[
            event("turn/start", 0, json!({"turn": 1})),
            event("step/start", 1, json!({"turn": 2, "step": 1})),
        ])
        .expect_err("step in wrong turn");
        assert!(wrong_turn.to_string().contains("open turn is 1"));

        let nested = validate_session_events(&[
            event("turn/start", 0, json!({"turn": 1})),
            event("step/start", 1, json!({"turn": 1, "step": 1})),
            event("step/start", 2, json!({"turn": 1, "step": 2})),
        ])
        .expect_err("nested step");
        assert!(nested.to_string().contains("still open"));

        let skipped = validate_session_events(&[
            event("turn/start", 0, json!({"turn": 1})),
            event("step/start", 1, json!({"turn": 1, "step": 1})),
            event("step/end", 2, json!({"turn": 1, "step": 1})),
            event("step/start", 3, json!({"turn": 1, "step": 3})),
        ])
        .expect_err("skipped step");
        assert!(skipped.to_string().contains("expected step 2"));
    }

    #[test]
    fn requires_step_scoped_stream_and_tool_events_to_name_the_open_step() {
        let chunk = validate_session_events(&[
            event("turn/start", 0, json!({"turn": 1})),
            event(
                "assistant/chunk",
                1,
                json!({"turn": 1, "step": 1, "chunk": {"type": "text-delta", "index": 0, "text": "x"}}),
            ),
        ])
        .expect_err("chunk without step");
        assert!(chunk.to_string().contains("open is turn 1/step null"));

        let ghost = validate_session_events(&[
            event("turn/start", 0, json!({"turn": 1})),
            event("step/start", 1, json!({"turn": 1, "step": 1})),
            surface_event("tool/result", 2, tool_result("ghost", false)),
        ])
        .expect_err("ghost tool result");
        assert!(ghost.to_string().contains("no prior tool/call"));
    }

    #[test]
    fn keeps_fresh_tool_result_appends_open_step_checked() {
        let error = validate_session_events(&[
            event("turn/start", 0, json!({"turn": 1})),
            surface_event("tool/result", 1, tool_result("closed", false)),
        ])
        .expect_err("tool result without step");
        assert!(error.to_string().contains("open is turn 1/step null"));
    }

    #[test]
    fn persistence_admits_a_validated_assistant_surface_rewrite_after_turn_close() {
        let original = surface_event(
            "assistant/message",
            2,
            json!({
                "turn": 1,
                "step": 1,
                "message": {
                    "id": "original",
                    "role": "assistant",
                    "source": {"kind": "model", "provider": "mock", "model": "mock"},
                    "content": [{"type": "text", "text": "original"}]
                }
            }),
        );
        let mut replacement = original.clone();
        replacement.seq = 5;
        replacement.surface_op = Some(SurfaceOp::replace(2, 2));
        replacement.source_event_seqs = Some(vec![2]);
        let events = [
            event("turn/start", 0, json!({"turn": 1})),
            event("step/start", 1, json!({"turn": 1, "step": 1})),
            original,
            event("step/end", 3, json!({"turn": 1, "step": 1})),
            event(
                "turn/end",
                4,
                json!({"turn": 1, "reason": {"kind": "completed"}}),
            ),
            replacement,
        ];
        assert!(validate_session_events(&events).is_err());
        validate_persisted_session_events(&events).expect("persisted surface rewrite");
    }

    #[test]
    fn treats_a_validated_tool_result_replacement_as_a_turn_enclosed_rewrite() {
        let mut outside = event("tool/result", 0, tool_result("rewrite", false));
        outside.surface_op = Some(SurfaceOp::replace(0, 0));
        outside.source_event_seqs = Some(vec![0]);
        let error = validate_session_events(&[outside]).expect_err("replacement outside turn");
        assert!(error.to_string().contains("outside any open turn"));

        let mut inside = event("tool/result", 1, tool_result("rewrite", false));
        inside.surface_op = Some(SurfaceOp::replace(0, 0));
        inside.source_event_seqs = Some(vec![0]);
        validate_session_events(&[event("turn/start", 0, json!({"turn": 1})), inside])
            .expect("turn-enclosed rewrite");
    }

    #[test]
    fn allows_not_started_repair_results_and_unresolved_calls_at_step_end() {
        validate_session_events(&[
            event("turn/start", 0, json!({"turn": 1})),
            event("step/start", 1, json!({"turn": 1, "step": 1})),
            surface_event("tool/result", 2, not_started_result("crashed")),
            event("step/end", 3, json!({"turn": 1, "step": 1})),
            event(
                "turn/end",
                4,
                json!({"turn": 1, "reason": {"kind": "interrupted"}}),
            ),
        ])
        .expect("not-started repair result");

        validate_session_events(&[
            event("turn/start", 0, json!({"turn": 1})),
            event("step/start", 1, json!({"turn": 1, "step": 1})),
            event("tool/call", 2, json!({"turn": 1, "step": 1, "callId": "c1", "name": "echo", "arguments": "{}"})),
            event("step/end", 3, json!({"turn": 1, "step": 1})),
            event("turn/end", 4, json!({"turn": 1, "reason": {"kind": "error", "error": {"message": "boom", "code": "UNKNOWN"}}})),
        ])
        .expect("unresolved call at step end");
    }

    #[test]
    fn does_not_let_a_later_step_result_satisfy_an_earlier_call() {
        let error = validate_session_events(&[
            event("turn/start", 0, json!({"turn": 1})),
            event("step/start", 1, json!({"turn": 1, "step": 1})),
            event("tool/call", 2, json!({"turn": 1, "step": 1, "callId": "c1", "name": "echo", "arguments": "{}"})),
            event("step/end", 3, json!({"turn": 1, "step": 1})),
            event("step/start", 4, json!({"turn": 1, "step": 2})),
            surface_event(
                "tool/result",
                5,
                json!({
                    "turn": 1,
                    "step": 2,
                    "message": {
                        "source": {"kind": "tool", "callId": "c1"},
                        "content": [{"type": "tool-result", "toolCallId": "c1", "isError": false, "content": []}],
                    },
                }),
            ),
        ])
        .expect_err("cross-step tool result");
        assert!(
            error
                .to_string()
                .contains("no prior tool/call in this step")
        );
    }

    #[test]
    fn accepts_end_seed_whether_or_not_a_turn_is_open() {
        validate_session_events(&[
            event("turn/start", 0, json!({"turn": 1})),
            event(
                "turn/end",
                1,
                json!({"turn": 1, "reason": {"kind": "completed"}}),
            ),
            event("session/end-seed", 2, json!({})),
        ])
        .expect("end-seed between turns");

        validate_session_events(&[
            event("turn/start", 0, json!({"turn": 1})),
            event("session/end-seed", 1, json!({})),
        ])
        .expect("end-seed inside an open turn");
    }

    #[test]
    fn replays_seeded_sessions_and_tracks_each_session_independently() {
        let context = Context::new();
        let store = SessionStore::install(&context).expect("store");
        install_session_invariants(&context, &store).expect("invariants");

        let bad_seed = vec![
            event("turn/start", 0, json!({"turn": 1})),
            event("turn/start", 1, json!({"turn": 2})),
        ];
        let error = store
            .create(
                &context,
                Some(SessionId::new("bad")),
                CreateSessionOptions {
                    seed: Some(bad_seed),
                    ..CreateSessionOptions::default()
                },
            )
            .expect_err("nested-turn seed");
        assert!(error.to_string().contains("still open"));

        let a = store
            .create(
                &context,
                Some(SessionId::new("a")),
                CreateSessionOptions::default(),
            )
            .expect("a");
        let b = store
            .create(
                &context,
                Some(SessionId::new("b")),
                CreateSessionOptions::default(),
            )
            .expect("b");
        a.append("turn/start", json!({"turn": 1}), AppendOptions::default())
            .expect("a turn");
        b.append("turn/start", json!({"turn": 1}), AppendOptions::default())
            .expect("b turn");
    }

    #[tokio::test]
    async fn rebuilds_trace_state_for_existing_sessions_on_reload() {
        let context = Context::new();
        let store = SessionStore::install(&context).expect("store");
        let effects = install_session_invariants(&context, &store).expect("invariants");
        let session = store
            .create(&context, None, CreateSessionOptions::default())
            .expect("session");
        session
            .append("turn/start", json!({"turn": 1}), AppendOptions::default())
            .expect("turn");
        session
            .append(
                "step/start",
                json!({"turn": 1, "step": 1}),
                AppendOptions::default(),
            )
            .expect("step");
        for effect in effects {
            effect.dispose().await.expect("dispose effect");
        }
        install_session_invariants(&context, &store).expect("re-install");

        session
            .append(
                "assistant/chunk",
                json!({"turn": 1, "step": 1, "chunk": {"type": "text-delta", "index": 0, "text": "h"}}),
                AppendOptions::default(),
            )
            .expect("step-scoped chunk after reload");
        let error = session
            .append("turn/start", json!({"turn": 2}), AppendOptions::default())
            .expect_err("still-open turn after reload");
        assert!(error.to_string().contains("still open"));
    }

    #[tokio::test]
    async fn removes_all_listeners_when_disposed() {
        let context = Context::new();
        let store = SessionStore::install(&context).expect("store");
        let effects = install_session_invariants(&context, &store).expect("invariants");
        let session = store
            .create(&context, None, CreateSessionOptions::default())
            .expect("session");
        session
            .append("turn/start", json!({"turn": 1}), AppendOptions::default())
            .expect("turn");
        for effect in effects {
            effect.dispose().await.expect("dispose effect");
        }
        session
            .append("turn/start", json!({"turn": 2}), AppendOptions::default())
            .expect("listeners removed");
    }

    #[test]
    fn invalid_transition_is_rejected_before_commit() {
        let context = Context::new();
        let store = SessionStore::install(&context).expect("store");
        install_session_invariants(&context, &store).expect("invariants");
        let session = store
            .create(&context, None, CreateSessionOptions::default())
            .expect("session");
        let error = session
            .append("turn/start", json!({"turn": 2}), AppendOptions::default())
            .expect_err("wrong first turn");
        assert!(error.to_string().contains("expected turn 1"));
        assert!(session.events().is_empty());
    }

    #[test]
    fn valid_trace_advances_after_each_commit() {
        let context = Context::new();
        let store = SessionStore::install(&context).expect("store");
        install_session_invariants(&context, &store).expect("invariants");
        let session = store
            .create(&context, None, CreateSessionOptions::default())
            .expect("session");
        for (kind, data) in [
            ("turn/start", json!({"turn": 1})),
            ("step/start", json!({"turn": 1, "step": 1})),
            ("step/end", json!({"turn": 1, "step": 1})),
            (
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "completed"}}),
            ),
        ] {
            session
                .append(kind, data, AppendOptions::default())
                .expect("valid transition");
        }
        assert_eq!(session.seq(), 4);
    }
}
