//! Package-owned durable workflow-record invariants.

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

const PACKAGE_NAME: &str = "seekdeep-tool-workflow";

/// Per-run trace accumulated across one workflow-record lifecycle.
#[derive(Clone, Debug, Default)]
struct RunTrace {
    ended: bool,
    members: HashMap<u64, bool>,
}

/// Shared fold: committed per-session traces plus staged pre-publication candidates.
#[derive(Debug, Default)]
struct InvariantState {
    traces: HashMap<String, HashMap<String, RunTrace>>,
    staged: HashMap<usize, (String, HashMap<String, RunTrace>)>,
}

/// Registers the workflow-record invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        PACKAGE_NAME,
        InvariantInstaller::new(["sessions"], |context, fail| {
            Box::pin(async move {
                install(&context, &fail)?;
                Ok(())
            })
        }),
    )
}

fn install(context: &Context, fail: &InvariantFailure) -> anyhow::Result<()> {
    let sessions = context
        .get(SESSIONS)
        .ok_or_else(|| anyhow::anyhow!("seekdeep-tool-workflow invariant requires sessions"))?;
    let state = Arc::new(Mutex::new(InvariantState::default()));
    for session in sessions.list() {
        seed_session(&state, &session, fail)?;
    }

    let created_state = state.clone();
    let created_fail = fail.clone();
    context.events().on_sync(
        context,
        "session/created",
        move |_, args| {
            let Some(session) = args.get::<Session>(0) else {
                return Ok(EventReply::Undefined);
            };
            seed_session(&created_state, &session, &created_fail)?;
            Ok(EventReply::Undefined)
        },
        global_events(),
    )?;

    let dispatch_state = state.clone();
    let dispatch_fail = fail.clone();
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
            let session = event_args
                .get::<Session>(0)
                .ok_or_else(|| anyhow::anyhow!("session/event lacks its session"))?;
            let event = event_args
                .get::<SessionEvent>(1)
                .ok_or_else(|| anyhow::anyhow!("session/event lacks its event"))?;
            if !event.event_type.starts_with("tool-workflow/") {
                return Ok(EventReply::Undefined);
            }
            let mut candidate = clone_for(&dispatch_state, &session, &dispatch_fail)?;
            validate_into(&mut candidate, event.as_ref(), &dispatch_fail)?;
            dispatch_state.lock().staged.insert(
                Arc::as_ptr(&event) as usize,
                (session.id().as_str().to_owned(), candidate),
            );
            Ok(EventReply::Undefined)
        },
        global_events(),
    )?;

    let commit_state = state;
    let commit_fail = fail.clone();
    context.events().on_sync(
        context,
        "session/event",
        move |_, args| {
            let session = args
                .get::<Session>(0)
                .ok_or_else(|| anyhow::anyhow!("session/event lacks its session"))?;
            let event = args
                .get::<SessionEvent>(1)
                .ok_or_else(|| anyhow::anyhow!("session/event lacks its event"))?;
            if !event.event_type.starts_with("tool-workflow/") {
                return Ok(EventReply::Undefined);
            }
            let key = session.id().as_str().to_owned();
            let staged = commit_state
                .lock()
                .staged
                .remove(&(Arc::as_ptr(&event) as usize));
            let Some((staged_key, candidate)) = staged.filter(|(staged_key, _)| *staged_key == key)
            else {
                return Err(commit_fail
                    .fail(
                        "session/event reached publication without matching workflow-record validation",
                    )
                    .into());
            };
            commit_state.lock().traces.insert(staged_key, candidate);
            Ok(EventReply::Undefined)
        },
        global_events(),
    )?;
    Ok(())
}

fn seed_session(
    state: &Arc<Mutex<InvariantState>>,
    session: &Arc<Session>,
    fail: &InvariantFailure,
) -> anyhow::Result<()> {
    let mut map = HashMap::new();
    for event in session.events() {
        if event.event_type.starts_with("tool-workflow/") {
            validate_into(&mut map, &event, fail)?;
        }
    }
    state
        .lock()
        .traces
        .insert(session.id().as_str().to_owned(), map);
    Ok(())
}

fn clone_for(
    state: &Arc<Mutex<InvariantState>>,
    session: &Arc<Session>,
    fail: &InvariantFailure,
) -> anyhow::Result<HashMap<String, RunTrace>> {
    let key = session.id().as_str().to_owned();
    if let Some(trace) = state.lock().traces.get(&key) {
        return Ok(trace.clone());
    }
    seed_session(state, session, fail)?;
    Ok(state.lock().traces.get(&key).expect("seeded").clone())
}

fn record_of<'a>(
    event: &'a SessionEvent,
    fail: &InvariantFailure,
) -> anyhow::Result<&'a serde_json::Map<String, serde_json::Value>> {
    let Some(object) = event.data.as_object() else {
        return Err(fail
            .fail(format!("{} data must be a JSON object", event.event_type))
            .into());
    };
    Ok(object)
}

fn string_id<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
    label: &str,
    fail: &InvariantFailure,
) -> anyhow::Result<&'a str> {
    let Some(value) = object.get(key).and_then(|value| value.as_str()) else {
        return Err(fail
            .fail(format!("{label} must be a non-empty string"))
            .into());
    };
    if value.is_empty() {
        return Err(fail
            .fail(format!("{label} must be a non-empty string"))
            .into());
    }
    Ok(value)
}

#[allow(clippy::too_many_lines)]
fn validate_into(
    map: &mut HashMap<String, RunTrace>,
    event: &SessionEvent,
    fail: &InvariantFailure,
) -> anyhow::Result<()> {
    let object = record_of(event, fail)?;
    let run_id = string_id(
        object,
        "runId",
        &format!("{} runId", event.event_type),
        fail,
    )?
    .to_owned();
    match event.event_type.as_str() {
        "tool-workflow/run-start" => {
            let name = string_id(object, "name", "tool-workflow/run-start name", fail)?;
            let _ = name;
            if map.contains_key(&run_id) {
                return Err(fail
                    .fail(format!("tool-workflow/run-start repeats run {run_id}"))
                    .into());
            }
            map.insert(run_id, RunTrace::default());
        }
        "tool-workflow/agent-start" => {
            let run = open_run(map, &run_id, &event.event_type, fail)?;
            let seq = member_seq(object, fail)?;
            if object
                .get("label")
                .and_then(|value| value.as_str())
                .is_none()
            {
                return Err(fail
                    .fail("tool-workflow/agent-start label must be a string")
                    .into());
            }
            if let Some(phase) = object.get("phase")
                && phase.as_str().is_none()
            {
                return Err(fail
                    .fail("tool-workflow/agent-start phase must be a string when present")
                    .into());
            }
            string_id(object, "childId", "tool-workflow/agent-start childId", fail)?;
            if run.members.contains_key(&seq) {
                return Err(fail
                    .fail(format!(
                        "tool-workflow/agent-start repeats member seq {seq} in run {run_id}"
                    ))
                    .into());
            }
            run.members.insert(seq, false);
        }
        "tool-workflow/agent-end" => {
            let run = open_run(map, &run_id, &event.event_type, fail)?;
            let seq = member_seq(object, fail)?;
            let outcome = object.get("outcome");
            if !matches!(
                outcome.and_then(serde_json::Value::as_str),
                Some("completed" | "failed" | "cancelled")
            ) {
                return Err(fail
                    .fail(format!(
                        "tool-workflow/agent-end outcome {} is invalid",
                        outcome.map_or_else(|| "undefined".to_owned(), display_value)
                    ))
                    .into());
            }
            let ended = run.members.get(&seq).copied();
            if ended.is_none() {
                return Err(fail
                    .fail(format!(
                        "tool-workflow/agent-end has no matching member seq {seq} in run {run_id}"
                    ))
                    .into());
            }
            if ended == Some(true) {
                return Err(fail
                    .fail(format!(
                        "tool-workflow/agent-end repeats member seq {seq} in run {run_id}"
                    ))
                    .into());
            }
            run.members.insert(seq, true);
        }
        "tool-workflow/run-end" => {
            let run = open_run(map, &run_id, &event.event_type, fail)?;
            let reason = object.get("stopReason");
            if !matches!(
                reason.and_then(serde_json::Value::as_str),
                Some("completed" | "cancelled" | "error")
            ) {
                return Err(fail
                    .fail(format!(
                        "tool-workflow/run-end stopReason {} is invalid",
                        reason.map_or_else(|| "undefined".to_owned(), display_value)
                    ))
                    .into());
            }
            let open: Vec<u64> = run
                .members
                .iter()
                .filter(|(_, ended)| !**ended)
                .map(|(seq, _)| *seq)
                .collect();
            if !open.is_empty() {
                let list = open
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(fail
                    .fail(format!(
                        "tool-workflow/run-end leaves member seq {list} open in run {run_id}"
                    ))
                    .into());
            }
            run.ended = true;
            run.members.clear();
        }
        other => {
            return Err(fail
                .fail(format!("unknown tool-workflow event type {other}"))
                .into());
        }
    }
    Ok(())
}

fn open_run<'a>(
    map: &'a mut HashMap<String, RunTrace>,
    run_id: &str,
    event_type: &str,
    fail: &InvariantFailure,
) -> anyhow::Result<&'a mut RunTrace> {
    let Some(run) = map.get_mut(run_id) else {
        return Err(fail
            .fail(format!(
                "{event_type} has no matching tool-workflow/run-start for run {run_id}"
            ))
            .into());
    };
    if run.ended {
        return Err(fail
            .fail(format!(
                "{event_type} appears after tool-workflow/run-end for run {run_id}"
            ))
            .into());
    }
    Ok(run)
}

/// Renders a field the way the source's `String(value)` coercion does for scalars: a bare
/// string stays bare, while other JSON scalars use their JSON text.
fn display_value(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

fn member_seq(
    object: &serde_json::Map<String, serde_json::Value>,
    fail: &InvariantFailure,
) -> anyhow::Result<u64> {
    let Some(seq) = object.get("seq").and_then(serde_json::Value::as_u64) else {
        return Err(fail
            .fail("tool-workflow member seq must be a positive safe integer")
            .into());
    };
    if seq < 1 {
        return Err(fail
            .fail("tool-workflow member seq must be a positive safe integer")
            .into());
    }
    Ok(seq)
}

fn global_events() -> EventOptions {
    EventOptions {
        global: true,
        ..EventOptions::default()
    }
}
