//! Package-owned durable goal-stream invariants.

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

use crate::fold::{GoalFoldState, apply_goal_event, empty_goal_fold_state};

const PACKAGE_NAME: &str = "seekdeep-goal";

#[derive(Debug, Default)]
struct InvariantState {
    states: HashMap<usize, GoalFoldState>,
    staged: HashMap<usize, (usize, GoalFoldState)>,
}

/// Registers the goal-stream invariant companion.
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
        .ok_or_else(|| anyhow::anyhow!("seekdeep-goal invariant requires sessions"))?;
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
            let event_args = args
                .get::<EventArgs>(2)
                .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks event arguments"))?;
            if event_name.as_str() != "session/event" {
                return Ok(EventReply::Undefined);
            }
            let session = required_session(&event_args, "session/event")?;
            let event = required_event(&event_args)?;
            let cloned = clone_state_for(&dispatch_state, &session, &dispatch_failure)?;
            let mut candidate = cloned;
            apply_checked(&mut candidate, &event, &dispatch_failure)?;
            dispatch_state.lock().staged.insert(
                Arc::as_ptr(&event) as usize,
                (session_key(&session), candidate),
            );
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
            let staged = published_state
                .lock()
                .staged
                .remove(&(Arc::as_ptr(&event) as usize));
            let Some((key, candidate)) = staged.filter(|(key, _)| *key == session_key(&session))
            else {
                return Err(published_failure
                    .fail("session/event reached publication without matching goal-fold validation")
                    .into());
            };
            published_state.lock().states.insert(key, candidate);
            Ok(EventReply::Undefined)
        },
        global_events(),
    )?;
    Ok(())
}

fn global_events() -> EventOptions {
    EventOptions {
        global: true,
        ..EventOptions::default()
    }
}

fn seed_session(
    state: &Arc<Mutex<InvariantState>>,
    session: &Arc<Session>,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    let mut folded = empty_goal_fold_state();
    for event in &session.events() {
        apply_checked(&mut folded, event, failure)?;
    }
    state.lock().states.insert(session_key(session), folded);
    Ok(())
}

fn clone_state_for(
    state: &Arc<Mutex<InvariantState>>,
    session: &Arc<Session>,
    failure: &InvariantFailure,
) -> anyhow::Result<GoalFoldState> {
    let key = session_key(session);
    if state.lock().states.contains_key(&key) {
        return Ok(state.lock().states.get(&key).expect("seeded").clone());
    }
    seed_session(state, session, failure)?;
    Ok(state.lock().states.get(&key).expect("seeded").clone())
}

/// Applies one event through the strict goal decoder and attributes failures.
fn apply_checked(
    state: &mut GoalFoldState,
    event: &SessionEvent,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    if let Err(error) = apply_goal_event(state, event) {
        return Err(failure
            .fail(format!(
                "session event {} violates the durable goal stream: {error:#}",
                event.seq
            ))
            .into());
    }
    Ok(())
}

fn required_session(args: &EventArgs, event_name: &str) -> anyhow::Result<Arc<Session>> {
    args.get::<Session>(0)
        .ok_or_else(|| anyhow::anyhow!("{event_name} lacks its session"))
}

fn required_event(args: &EventArgs) -> anyhow::Result<Arc<SessionEvent>> {
    args.get::<SessionEvent>(1)
        .ok_or_else(|| anyhow::anyhow!("session/event lacks its event"))
}

fn session_key(session: &Arc<Session>) -> usize {
    Arc::as_ptr(session) as usize
}
