//! Package-owned durable workflow-record invariants.

use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;
use seekdeep_cordis::{EventOptions, EventReply};
use seekdeep_core::session::{Session, SessionEvent};
use seekdeep_invariants::{InvariantFailure, InvariantInstaller, InvariantRegistry};

const PACKAGE_NAME: &str = "seekdeep-tool-workflow";

/// Per-run trace accumulated across one workflow-record lifecycle.
#[derive(Debug, Default)]
struct RunTrace {
    ended: bool,
    members: HashMap<u64, bool>,
}

/// Registers the workflow-record invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<seekdeep_invariants::InvariantRegistration> {
    registry.register(
        PACKAGE_NAME,
        InvariantInstaller::new(["sessions"], move |context, fail| {
            Box::pin(async move {
                let traces = Arc::new(Mutex::new(
                    HashMap::<String, HashMap<String, RunTrace>>::new(),
                ));
                // Seed from existing sessions.
                let sessions = context.get(seekdeep_core::session_store::SESSIONS);
                if let Some(store) = sessions {
                    for session in store.list() {
                        seed_session(&session, &traces, &fail)?;
                    }
                }
                let listener_traces = Arc::clone(&traces);
                let listener_fail = fail.clone();
                context.events().on_sync(
                    &context,
                    "session/event",
                    move |_, args| {
                        let Some(session) = args.get::<Arc<Session>>(0) else {
                            return Ok(EventReply::Undefined);
                        };
                        let Some(event) = args.get::<SessionEvent>(1) else {
                            return Ok(EventReply::Undefined);
                        };
                        if !event.event_type.starts_with("tool-workflow/") {
                            return Ok(EventReply::Undefined);
                        }
                        validate_event(&session, event.as_ref(), &listener_traces, &listener_fail)?;
                        Ok(EventReply::Undefined)
                    },
                    global_events(),
                )?;
                Ok(())
            })
        }),
    )
}

fn seed_session(
    session: &Arc<Session>,
    traces: &Arc<Mutex<HashMap<String, HashMap<String, RunTrace>>>>,
    fail: &InvariantFailure,
) -> anyhow::Result<()> {
    let mut map = HashMap::new();
    for event in session.events() {
        if event.event_type.starts_with("tool-workflow/") {
            validate_into(&mut map, &event, fail)?;
        }
    }
    traces.lock().insert(session.id().as_str().to_owned(), map);
    Ok(())
}

fn validate_event(
    session: &Arc<Session>,
    event: &SessionEvent,
    traces: &Arc<Mutex<HashMap<String, HashMap<String, RunTrace>>>>,
    fail: &InvariantFailure,
) -> anyhow::Result<()> {
    let mut all = traces.lock();
    let map = all.entry(session.id().as_str().to_owned()).or_default();
    validate_into(map, event, fail)
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
    let Some(value) = object.get(key).and_then(|v| v.as_str()) else {
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
            if object.get("label").and_then(|v| v.as_str()).is_none() {
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
            let outcome = object.get("outcome").and_then(|v| v.as_str());
            if !matches!(outcome, Some("completed" | "failed" | "cancelled")) {
                return Err(fail
                    .fail(format!(
                        "tool-workflow/agent-end outcome {outcome:?} is invalid"
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
            let reason = object.get("stopReason").and_then(|v| v.as_str());
            if !matches!(reason, Some("completed" | "cancelled" | "error")) {
                return Err(fail
                    .fail(format!(
                        "tool-workflow/run-end stopReason {reason:?} is invalid"
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
