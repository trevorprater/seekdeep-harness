//! Package-owned approval audit-stream invariants.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

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

const PACKAGE_NAME: &str = "seekdeep-user-approval";

#[derive(Clone, Debug, PartialEq, Eq)]
enum Transition {
    Asked(String),
    Decided(String),
}

#[derive(Debug, Default)]
struct Trace {
    open_turn: bool,
    pending: HashSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct EventKey {
    session: usize,
    seq: u64,
    event_type: String,
}

#[derive(Debug, Default)]
struct State {
    traces: HashMap<usize, Trace>,
    staged: HashMap<EventKey, Transition>,
}

/// Registers turn enclosure, audit pairing, and vocabulary checks.
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
            install(&context, &failure)?;
            Ok(())
        }),
    )
}

fn install(context: &Context, failure: &InvariantFailure) -> anyhow::Result<()> {
    let sessions = context
        .get(SESSIONS)
        .ok_or_else(|| anyhow::anyhow!("seekdeep-user-approval invariant requires sessions"))?;
    let state = Arc::new(Mutex::new(State::default()));
    for session in sessions.list() {
        seed(&state, &session, failure)?;
    }

    let created_state = state.clone();
    let created_failure = failure.clone();
    context.events().on_sync(
        context,
        "session/created",
        move |_, args| {
            let session = required_session(&args, "session/created")?;
            seed(&created_state, &session, &created_failure)?;
            Ok(EventReply::Undefined)
        },
        global(),
    )?;

    let published_state = state.clone();
    let published_failure = failure.clone();
    context.events().on_sync(
        context,
        "session/event",
        move |_, args| {
            let session = required_session(&args, "session/event")?;
            let event = required_event(&args)?;
            publish(&published_state, &session, &event, &published_failure)?;
            Ok(EventReply::Undefined)
        },
        global(),
    )?;

    let staged_state = state;
    let staged_failure = failure.clone();
    context.events().on_sync(
        context,
        "internal/dispatch",
        move |_, args| {
            stage_internal(&staged_state, &args, &staged_failure)?;
            Ok(EventReply::Undefined)
        },
        global(),
    )?;
    Ok(())
}

fn global() -> EventOptions {
    EventOptions {
        global: true,
        ..EventOptions::default()
    }
}

fn stage_internal(
    state: &Arc<Mutex<State>>,
    args: &EventArgs,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    args.get::<DispatchMode>(0)
        .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks a dispatch mode"))?;
    let name = args
        .get::<String>(1)
        .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks an event name"))?;
    if name.as_str() != "session/event" {
        return Ok(());
    }
    let carried = args
        .get::<EventArgs>(2)
        .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks event arguments"))?;
    let session = required_session(&carried, "session/event")?;
    let event = required_event(&carried)?;
    ensure_seeded(state, &session, failure)?;
    let transition = {
        let state = state.lock();
        validate(
            state
                .traces
                .get(&session_key(&session))
                .expect("seeded trace"),
            &event,
            failure,
        )?
    };
    if let Some(transition) = transition {
        state
            .lock()
            .staged
            .insert(key(&session, &event), transition);
    }
    Ok(())
}

fn publish(
    state: &Arc<Mutex<State>>,
    session: &Arc<Session>,
    event: &SessionEvent,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    ensure_seeded(state, session, failure)?;
    let mut state = state.lock();
    let transition = if matches!(
        event.event_type.as_str(),
        "approval/asked" | "approval/decided"
    ) {
        Some(state.staged.remove(&key(session, event)).ok_or_else(|| {
            failure.fail("approval audit event published without pre-commit validation")
        })?)
    } else {
        None
    };
    let trace = state
        .traces
        .get_mut(&session_key(session))
        .expect("seeded trace");
    match event.event_type.as_str() {
        "turn/start" => trace.open_turn = true,
        "turn/end" => trace.open_turn = false,
        _ => {}
    }
    if let Some(transition) = transition {
        apply(trace, transition);
    }
    Ok(())
}

fn ensure_seeded(
    state: &Arc<Mutex<State>>,
    session: &Arc<Session>,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    if !state.lock().traces.contains_key(&session_key(session)) {
        seed(state, session, failure)?;
    }
    Ok(())
}

fn seed(
    state: &Arc<Mutex<State>>,
    session: &Arc<Session>,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    let mut trace = Trace::default();
    for event in session.events() {
        match event.event_type.as_str() {
            "turn/start" => trace.open_turn = true,
            "turn/end" => trace.open_turn = false,
            _ => {}
        }
        if let Some(transition) = validate(&trace, &event, failure)? {
            apply(&mut trace, transition);
        }
    }
    state.lock().traces.insert(session_key(session), trace);
    Ok(())
}

fn validate(
    trace: &Trace,
    event: &SessionEvent,
    failure: &InvariantFailure,
) -> anyhow::Result<Option<Transition>> {
    match event.event_type.as_str() {
        "approval/asked" => {
            if !trace.open_turn {
                return Err(failure
                    .fail("approval/asked appended outside any open turn")
                    .into());
            }
            let tool_name = string(&event.data, "toolName");
            if tool_name.is_empty() {
                return Err(failure
                    .fail("approval/asked toolName must be non-empty")
                    .into());
            }
            let id = string(&event.data, "id");
            if trace.pending.contains(&id) {
                return Err(failure
                    .fail(format!("approval/asked repeated open id {id:?}"))
                    .into());
            }
            Ok(Some(Transition::Asked(id)))
        }
        "approval/decided" => {
            if !trace.open_turn {
                return Err(failure
                    .fail("approval/decided appended outside any open turn")
                    .into());
            }
            let id = string(&event.data, "id");
            if !trace.pending.contains(&id) {
                return Err(failure
                    .fail(format!(
                        "approval/decided has no matching approval/asked for id {id:?}"
                    ))
                    .into());
            }
            let outcome = string(&event.data, "outcome");
            if !matches!(
                outcome.as_str(),
                "allowed-once" | "rejected" | "cancelled" | "unavailable"
            ) {
                return Err(failure
                    .fail(format!(
                        "approval/decided carries unknown outcome {outcome:?}"
                    ))
                    .into());
            }
            Ok(Some(Transition::Decided(id)))
        }
        "approval/policy" => {
            let policy = string(&event.data, "policy");
            if !matches!(policy.as_str(), "ask" | "never") {
                return Err(failure
                    .fail(format!("approval/policy carries unknown policy {policy:?}"))
                    .into());
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn apply(trace: &mut Trace, transition: Transition) {
    match transition {
        Transition::Asked(id) => {
            trace.pending.insert(id);
        }
        Transition::Decided(id) => {
            trace.pending.remove(&id);
        }
    }
}

fn string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn required_session(args: &EventArgs, event: &str) -> anyhow::Result<Arc<Session>> {
    args.get::<Session>(0)
        .ok_or_else(|| anyhow::anyhow!("{event} lacks its session"))
}

fn required_event(args: &EventArgs) -> anyhow::Result<Arc<SessionEvent>> {
    args.get::<SessionEvent>(1)
        .ok_or_else(|| anyhow::anyhow!("session/event lacks its event"))
}

fn session_key(session: &Arc<Session>) -> usize {
    Arc::as_ptr(session) as usize
}

fn key(session: &Arc<Session>, event: &SessionEvent) -> EventKey {
    EventKey {
        session: session_key(session),
        seq: event.seq,
        event_type: event.event_type.clone(),
    }
}
