//! Package-owned hook invocation/result stream invariants.

use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;
use seekdeep_cordis::{Context, DispatchMode, EventArgs, EventOptions, EventReply};
use seekdeep_core::{
    session::{Session, SessionEvent},
    session_store::SESSIONS,
};
use seekdeep_invariants::{
    InvariantFailure, InvariantInstaller, InvariantRegistration, InvariantRegistry,
};
use serde_json::Value;

const PACKAGE_NAME: &str = "seekdeep-hook-protocol";

/// One committed hook-pair transition.
#[derive(Clone, Debug)]
struct HookTransition {
    key: String,
    delta: i64,
}

/// Per-session pending-invocation state.
#[derive(Clone, Debug, Default)]
struct HookTrace {
    open_turn: Option<u64>,
    pending: HashMap<String, i64>,
}

#[derive(Debug, Default)]
struct InvariantState {
    traces: HashMap<usize, HookTrace>,
    staged: HashMap<usize, (usize, HookTransition)>,
}

/// Correlation key shared by an invoked/result pair.
fn hook_key(turn: u64, point: &str, handler_id: &str) -> String {
    format!("{turn}\0{point}\0{handler_id}")
}

/// Validates one hook event against committed pending invocations.
fn validate_hook_event(
    trace: &HookTrace,
    event: &SessionEvent,
    failure: &InvariantFailure,
) -> anyhow::Result<Option<HookTransition>> {
    if event.event_type != "hook/invoked" && event.event_type != "hook/result" {
        return Ok(None);
    }
    let Some(open_turn) = trace.open_turn else {
        return Err(failure
            .fail(format!(
                "{} appended outside any open turn",
                event.event_type
            ))
            .into());
    };
    let turn = event.data.get("turn").and_then(Value::as_u64);
    if turn != Some(open_turn) {
        let rendered = turn.map_or_else(|| "undefined".to_owned(), |turn| turn.to_string());
        return Err(failure
            .fail(format!(
                "{} names turn {rendered} but open turn is {open_turn}",
                event.event_type
            ))
            .into());
    }
    if event.event_type == "hook/invoked" {
        let point = event.data.get("point").and_then(Value::as_str);
        let handler_id = event.data.get("handlerId").and_then(Value::as_str);
        if point.is_none_or(str::is_empty) || handler_id.is_none_or(str::is_empty) {
            return Err(failure
                .fail("hook/invoked point and handlerId must be non-empty")
                .into());
        }
        let dialect = event.data.get("dialect").and_then(Value::as_str);
        if dialect != Some("claude-code") && dialect != Some("codex") {
            let rendered = dialect.map_or_else(
                || "undefined".to_owned(),
                |dialect| serde_json::to_string(dialect).unwrap_or_default(),
            );
            return Err(failure
                .fail(format!("hook/invoked carries unknown dialect {rendered}"))
                .into());
        }
        return Ok(Some(HookTransition {
            key: hook_key(
                open_turn,
                point.unwrap_or_default(),
                handler_id.unwrap_or_default(),
            ),
            delta: 1,
        }));
    }
    let point = event
        .data
        .get("point")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let handler_id = event
        .data
        .get("handlerId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let key = hook_key(open_turn, point, handler_id);
    if trace.pending.get(&key).copied().unwrap_or(0) == 0 {
        let rendered = serde_json::to_string(handler_id).unwrap_or_else(|_| "null".to_owned());
        return Err(failure
            .fail(format!(
                "hook/result has no matching hook/invoked for {rendered}"
            ))
            .into());
    }
    let duration_ok = event
        .data
        .get("durationMs")
        .and_then(Value::as_f64)
        .is_some_and(|value| value.is_finite() && value >= 0.0);
    if !duration_ok {
        return Err(failure
            .fail("hook/result durationMs must be a non-negative finite number")
            .into());
    }
    Ok(Some(HookTransition { key, delta: -1 }))
}

/// Applies one committed hook-pair transition.
fn apply_hook_transition(pending: &mut HashMap<String, i64>, transition: HookTransition) {
    let next = pending.get(&transition.key).copied().unwrap_or(0) + transition.delta;
    if next == 0 {
        pending.remove(&transition.key);
    } else {
        pending.insert(transition.key, next);
    }
}

/// Rebuilds one trace from a session's committed log.
fn seed_trace(session: &Session, failure: &InvariantFailure) -> anyhow::Result<HookTrace> {
    let mut trace = HookTrace::default();
    for event in &session.events() {
        match event.event_type.as_str() {
            "turn/start" => trace.open_turn = event.data.get("turn").and_then(Value::as_u64),
            "turn/end" => trace.open_turn = None,
            _ => {}
        }
        if let Some(transition) = validate_hook_event(&trace, event, failure)? {
            apply_hook_transition(&mut trace.pending, transition);
        }
    }
    Ok(trace)
}

fn seed_session(
    state: &Arc<Mutex<InvariantState>>,
    session: &Arc<Session>,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    let trace = seed_trace(session, failure)?;
    state.lock().traces.insert(session_key(session), trace);
    Ok(())
}

/// Returns the trace for a session, seeding it from the log when first observed.
fn trace_for(
    state: &Arc<Mutex<InvariantState>>,
    session: &Arc<Session>,
    failure: &InvariantFailure,
) -> anyhow::Result<HookTrace> {
    let key = session_key(session);
    if let Some(trace) = state.lock().traces.get(&key) {
        return Ok(trace.clone());
    }
    let trace = seed_trace(session, failure)?;
    state.lock().traces.insert(key, trace.clone());
    Ok(trace)
}

/// Returns the trace for a session, seeding it in place when first observed.
fn ensure_trace<'a>(
    state: &'a mut InvariantState,
    key: usize,
    session: &Session,
    failure: &InvariantFailure,
) -> anyhow::Result<&'a mut HookTrace> {
    if let std::collections::hash_map::Entry::Vacant(entry) = state.traces.entry(key) {
        entry.insert(seed_trace(session, failure)?);
    }
    Ok(state
        .traces
        .get_mut(&key)
        .expect("trace present after seed"))
}

fn session_key(session: &Arc<Session>) -> usize {
    Arc::as_ptr(session) as usize
}

fn required_session(args: &EventArgs, event_name: &str) -> anyhow::Result<Arc<Session>> {
    args.get::<Session>(0)
        .ok_or_else(|| anyhow::anyhow!("{event_name} lacks its session"))
}

fn required_event(args: &EventArgs) -> anyhow::Result<Arc<SessionEvent>> {
    args.get::<SessionEvent>(1)
        .ok_or_else(|| anyhow::anyhow!("session/event lacks its event"))
}

fn global_events() -> EventOptions {
    EventOptions {
        global: true,
        ..EventOptions::default()
    }
}

fn install(context: &Context, failure: &InvariantFailure) -> anyhow::Result<()> {
    let sessions = context
        .get(SESSIONS)
        .ok_or_else(|| anyhow::anyhow!("seekdeep-hook-protocol invariant requires sessions"))?;
    let state = Arc::new(Mutex::new(InvariantState::default()));

    for session in sessions.list() {
        seed_session(&state, &session, failure)?;
    }

    let created_state = state.clone();
    let created_failure = failure.clone();
    context.events().on_sync(
        context,
        "session/created",
        move |_, args| {
            let session = required_session(&args, "session/created")?;
            seed_session(&created_state, &session, &created_failure)?;
            Ok(EventReply::Undefined)
        },
        global_events(),
    )?;

    let dispatch_state = state.clone();
    let dispatch_failure = failure.clone();
    context.events().on_sync(
        context,
        "internal/dispatch",
        move |_, args| {
            args.get::<DispatchMode>(0)
                .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks a dispatch mode"))?;
            let event_name = args
                .get::<String>(1)
                .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks an event name"))?;
            let carried = args
                .get::<EventArgs>(2)
                .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks event arguments"))?;
            if event_name.as_str() != "session/event" {
                return Ok(EventReply::Undefined);
            }
            let session = required_session(&carried, "session/event")?;
            let event = required_event(&carried)?;
            let trace = trace_for(&dispatch_state, &session, &dispatch_failure)?;
            if let Some(transition) = validate_hook_event(&trace, &event, &dispatch_failure)? {
                dispatch_state.lock().staged.insert(
                    Arc::as_ptr(&event) as usize,
                    (session_key(&session), transition),
                );
            }
            Ok(EventReply::Undefined)
        },
        global_events(),
    )?;

    let published_state = state;
    let published_failure = failure.clone();
    context.events().on_sync(
        context,
        "session/event",
        move |_, args| {
            let session = required_session(&args, "session/event")?;
            let event = required_event(&args)?;
            let key = session_key(&session);

            if event.event_type == "turn/start" || event.event_type == "turn/end" {
                let mut guard = published_state.lock();
                let trace = ensure_trace(&mut guard, key, &session, &published_failure)?;
                trace.open_turn = if event.event_type == "turn/start" {
                    event.data.get("turn").and_then(Value::as_u64)
                } else {
                    None
                };
                return Ok(EventReply::Undefined);
            }
            if event.event_type != "hook/invoked" && event.event_type != "hook/result" {
                return Ok(EventReply::Undefined);
            }
            let mut guard = published_state.lock();
            let staged = guard.staged.remove(&(Arc::as_ptr(&event) as usize));
            let Some((_, transition)) = staged.filter(|(staged_key, _)| *staged_key == key) else {
                return Err(published_failure
                    .fail("hook event published without pre-commit validation")
                    .into());
            };
            let trace = ensure_trace(&mut guard, key, &session, &published_failure)?;
            apply_hook_transition(&mut trace.pending, transition);
            Ok(EventReply::Undefined)
        },
        global_events(),
    )?;
    Ok(())
}

/// Registers the hook-protocol invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration or installer failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        PACKAGE_NAME,
        InvariantInstaller::new(["sessions"], |context, failure| async move {
            install(&context, &failure)
        }),
    )
}
