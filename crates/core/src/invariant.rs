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
    let mut trace = SessionTrace::default();
    for event in events {
        trace = validate_event(&trace, event)?;
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
            let next = validate_event(&trace, &event)?;
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
        trace = validate_event(&trace, &event)?;
    }
    Ok(trace)
}

fn validate_event(
    current: &SessionTrace,
    event: &SessionEvent,
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
        "assistant/chunk" | "assistant/message" => {
            require_open_step(&next, &event.event_type, event)?;
        }
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
    use serde_json::json;

    use super::*;
    use crate::{
        session::AppendOptions,
        session_store::{CreateSessionOptions, SessionStore},
    };

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
