//! Package-owned command lifecycle pairing and source-reference invariants.

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

const PACKAGE_NAME: &str = "seekdeep-commands";

#[derive(Debug, Default)]
struct State {
    runs: HashMap<usize, HashSet<String>>,
}

/// Registers lifecycle pairing and authoritative-source validation.
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

fn install(context: &Context, failure: &InvariantFailure) -> anyhow::Result<()> {
    let sessions = context
        .get(SESSIONS)
        .ok_or_else(|| anyhow::anyhow!("seekdeep-commands invariant requires sessions"))?;
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
    let staged_state = state;
    let staged_failure = failure.clone();
    context.events().on_sync(
        context,
        "internal/dispatch",
        move |_, args| {
            validate_internal(&staged_state, &args, &staged_failure)?;
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

fn validate_internal(
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
    let event = carried
        .get::<SessionEvent>(1)
        .ok_or_else(|| anyhow::anyhow!("session/event lacks its event"))?;
    ensure_seeded(state, &session, failure)?;
    validate_and_apply(state, &session, &event, failure)
}

fn ensure_seeded(
    state: &Arc<Mutex<State>>,
    session: &Arc<Session>,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    if !state.lock().runs.contains_key(&session_key(session)) {
        seed(state, session, failure)?;
    }
    Ok(())
}

fn seed(
    state: &Arc<Mutex<State>>,
    session: &Arc<Session>,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    state.lock().runs.entry(session_key(session)).or_default();
    for event in session.events() {
        validate_and_apply(state, session, &event, failure)?;
    }
    Ok(())
}

fn validate_and_apply(
    state: &Arc<Mutex<State>>,
    session: &Arc<Session>,
    event: &SessionEvent,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    let command_id = string(&event.data, "commandId");
    match event.event_type.as_str() {
        "command/run" => {
            let mut state = state.lock();
            let runs = state.runs.entry(session_key(session)).or_default();
            if runs.contains(&command_id) {
                return Err(failure
                    .fail(format!("command/run repeats commandId {command_id:?}"))
                    .into());
            }
            runs.insert(command_id);
        }
        "command/done" => {
            if !state
                .lock()
                .runs
                .get(&session_key(session))
                .is_some_and(|runs| runs.contains(&command_id))
            {
                return Err(failure
                    .fail(format!(
                        "command/done {command_id:?} pairs no prior command/run in this log"
                    ))
                    .into());
            }
            validate_source(session, event, &command_id, failure)?;
        }
        _ => {}
    }
    Ok(())
}

fn validate_source(
    session: &Session,
    event: &SessionEvent,
    command_id: &str,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    let Some(source) = event.data.get("sourceEventSeq") else {
        return Ok(());
    };
    let sequence = source.as_u64();
    let valid = sequence.is_some_and(|sequence| {
        event.data.get("kind").and_then(Value::as_str) == Some("success")
            && sequence < event.seq
            && usize::try_from(sequence)
                .ok()
                .and_then(|index| session.events().get(index).cloned())
                .is_some_and(|source_event| {
                    source_event.seq == sequence
                        && !matches!(
                            source_event.event_type.as_str(),
                            "command/run" | "command/done"
                        )
                })
    });
    if !valid {
        return Err(failure
            .fail(format!(
                "command/done {command_id:?} has invalid sourceEventSeq {}",
                display_value(source)
            ))
            .into());
    }
    Ok(())
}

fn string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn display_value(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

fn required_session(args: &EventArgs, event: &str) -> anyhow::Result<Arc<Session>> {
    args.get::<Session>(0)
        .ok_or_else(|| anyhow::anyhow!("{event} lacks its session"))
}

fn session_key(session: &Arc<Session>) -> usize {
    Arc::as_ptr(session) as usize
}
