//! Package-owned compaction log-stream invariants.

use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;
use seekdeep_cordis::{Context, DispatchMode, EventArgs, EventOptions, EventReply};
use seekdeep_core::{
    session::{Session, SessionEvent, is_replacement_surface_event},
    session_store::SESSIONS,
};
use seekdeep_invariants::{
    InvariantFailure, InvariantInstaller, InvariantRegistration, InvariantRegistry,
};
use seekdeep_llm::MessageSource;
use serde_json::Value;

use crate::{CompactionId, is_compact_checkpoint_source};

const PACKAGE_NAME: &str = "seekdeep-compaction";

#[derive(Debug)]
struct CompactionTrace {
    compaction_id: CompactionId,
    source_command_id: Option<String>,
    start_seq: u64,
    turn: Option<u64>,
    summarized: bool,
}

#[derive(Debug, Default)]
struct SessionTrace {
    open_turn: Option<u64>,
    compaction: Option<CompactionTrace>,
}

#[derive(Debug)]
enum CompactionTransition {
    Start {
        compaction_id: CompactionId,
        source_command_id: Option<String>,
        start_seq: u64,
        turn: Option<u64>,
    },
    Summary {
        compaction_id: CompactionId,
        source_command_id: Option<String>,
        start_seq: u64,
        turn: Option<u64>,
    },
    End,
    EndSeed,
}

#[derive(Debug, Default)]
struct InvariantState {
    traces: HashMap<usize, SessionTrace>,
    staged: HashMap<usize, (usize, CompactionTransition)>,
}

/// Registers compaction start/summary/end checks.
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
        .ok_or_else(|| anyhow::anyhow!("seekdeep-compaction invariant requires sessions"))?;
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

    let published_state = state.clone();
    let published_failure = failure.clone();
    context.events().on_sync(
        context,
        "session/event",
        move |_, args| {
            let session = required_session(&args, "session/event")?;
            let event = required_event(&args)?;
            commit_published_event(&published_state, &session, &event, &published_failure)?;
            Ok(EventReply::Undefined)
        },
        global_events(),
    )?;

    let dispatch_state = state;
    let dispatch_failure = failure.clone();
    context.events().on_sync(
        context,
        "internal/dispatch",
        move |_, args| {
            validate_internal_dispatch(&dispatch_state, &args, &dispatch_failure)?;
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

fn validate_internal_dispatch(
    state: &Arc<Mutex<InvariantState>>,
    args: &EventArgs,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    args.get::<DispatchMode>(0)
        .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks a dispatch mode"))?;
    let event_name = args
        .get::<String>(1)
        .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks an event name"))?;
    let event_args = args
        .get::<EventArgs>(2)
        .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks event arguments"))?;

    if event_name.as_str() != "session/event" {
        return Ok(());
    }
    let session = required_session(&event_args, "session/event")?;
    let event = required_event(&event_args)?;
    let key = session_key(&session);
    ensure_seeded(state, &session, failure)?;
    let transition = {
        let state = state.lock();
        let trace = state.traces.get(&key).expect("session was seeded");
        validate_turn_boundary(trace, &event, failure)?;
        validate_compaction_event(trace, &event, failure)?
    };
    if let Some(transition) = transition {
        state
            .lock()
            .staged
            .insert(Arc::as_ptr(&event) as usize, (key, transition));
    }
    Ok(())
}

fn commit_published_event(
    state: &Arc<Mutex<InvariantState>>,
    session: &Arc<Session>,
    event: &Arc<SessionEvent>,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    let key = session_key(session);
    ensure_seeded(state, session, failure)?;
    let mut state = state.lock();
    let InvariantState { traces, staged } = &mut *state;
    let trace = traces.get_mut(&key).expect("session was seeded");
    validate_turn_boundary(trace, event, failure)?;
    if apply_turn_boundary(trace, event) {
        return Ok(());
    }
    if !matches!(
        event.event_type.as_str(),
        "session/end-seed" | "compaction/start" | "compaction/summary" | "compaction/end"
    ) {
        return Ok(());
    }
    let Some((staged_key, transition)) = staged
        .remove(&(Arc::as_ptr(event) as usize))
        .filter(|(key, _)| *key == session_key(session))
    else {
        return Err(failure
            .fail("compaction event published without pre-commit validation")
            .into());
    };
    let _ = staged_key;
    trace.compaction = apply_compaction_transition(transition);
    Ok(())
}

fn ensure_seeded(
    state: &Arc<Mutex<InvariantState>>,
    session: &Arc<Session>,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    let key = session_key(session);
    if state.lock().traces.contains_key(&key) {
        return Ok(());
    }
    seed_session(state, session, failure)
}

fn seed_session(
    state: &Arc<Mutex<InvariantState>>,
    session: &Arc<Session>,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    let mut trace = SessionTrace::default();
    let events = session.events();
    let orphan_starts = inherited_orphan_start_seqs(&events);
    for event in &events {
        let skip_boundary = trace
            .compaction
            .as_ref()
            .is_some_and(|compaction| orphan_starts.contains(&compaction.start_seq));
        if !skip_boundary {
            validate_turn_boundary(&trace, event, failure)?;
        }
        let transition = validate_compaction_event(&trace, event, failure)?;
        if let Some(transition) = transition {
            trace.compaction = apply_compaction_transition(transition);
        }
        apply_turn_boundary(&mut trace, event);
    }
    state.lock().traces.insert(session_key(session), trace);
    Ok(())
}

/// Compaction starts still unmatched when a later seed boundary made them stale.
fn inherited_orphan_start_seqs(events: &[SessionEvent]) -> std::collections::HashSet<u64> {
    let mut stale = std::collections::HashSet::new();
    let mut open_start_seq: Option<u64> = None;
    for event in events {
        match event.event_type.as_str() {
            "compaction/start" => open_start_seq = Some(event.seq),
            "compaction/end" => open_start_seq = None,
            "session/end-seed" => {
                if let Some(start_seq) = open_start_seq {
                    stale.insert(start_seq);
                }
                open_start_seq = None;
            }
            _ => {}
        }
    }
    stale
}

fn validate_turn_boundary(
    trace: &SessionTrace,
    event: &SessionEvent,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    if !matches!(event.event_type.as_str(), "turn/start" | "turn/end") {
        return Ok(());
    }
    let Some(compaction) = &trace.compaction else {
        return Ok(());
    };
    let owner = compaction.turn.map_or_else(
        || "standalone compaction".to_owned(),
        |turn| format!("compaction for turn {turn}"),
    );
    Err(failure
        .fail(format!("{} cannot cross an open {owner}", event.event_type))
        .into())
}

fn apply_turn_boundary(trace: &mut SessionTrace, event: &SessionEvent) -> bool {
    match event.event_type.as_str() {
        "turn/start" => {
            trace.open_turn = event.data.get("turn").and_then(Value::as_u64);
            true
        }
        "turn/end" => {
            trace.open_turn = None;
            true
        }
        _ => false,
    }
}

fn validate_owner(
    owner: Option<u64>,
    open_turn: Option<u64>,
    event_type: &str,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    match owner {
        None => {
            if let Some(open_turn) = open_turn {
                return Err(failure
                    .fail(format!(
                        "{event_type} is standalone but turn {open_turn} is open"
                    ))
                    .into());
            }
            Ok(())
        }
        Some(owner) => match open_turn {
            None => Err(failure
                .fail(format!(
                    "{event_type} for turn {owner} appended outside any open turn"
                ))
                .into()),
            Some(open_turn) if owner != open_turn => Err(failure
                .fail(format!(
                    "{event_type} names turn {owner} but open turn is {open_turn}"
                ))
                .into()),
            Some(_) => Ok(()),
        },
    }
}

#[allow(clippy::too_many_lines)]
fn validate_compaction_event(
    trace: &SessionTrace,
    event: &SessionEvent,
    failure: &InvariantFailure,
) -> anyhow::Result<Option<CompactionTransition>> {
    match event.event_type.as_str() {
        "session/end-seed" => return Ok(Some(CompactionTransition::EndSeed)),
        "user/message"
            if is_replacement_surface_event(event)
                && checkpoint_source(&event.data)
                    .is_some_and(|source| is_compact_checkpoint_source(&source)) =>
        {
            validate_checkpoint(trace, event, failure)?;
            return Ok(None);
        }
        "compaction/start" | "compaction/summary" | "compaction/end" => {}
        _ => return Ok(None),
    }

    let data = &event.data;
    let open = trace.compaction.as_ref();
    match event.event_type.as_str() {
        "compaction/start" => {
            validate_id(
                data.get("compactionId"),
                "compaction/start compactionId",
                failure,
            )?;
            if data.get("sourceCommandId").is_some() {
                validate_id(
                    data.get("sourceCommandId"),
                    "compaction/start sourceCommandId",
                    failure,
                )?;
            }
            if let Some(open) = open {
                let owner = open.turn.map_or_else(
                    || "standalone compaction".to_owned(),
                    |turn| format!("turn {turn}"),
                );
                return Err(failure
                    .fail(format!(
                        "compaction/start while {owner} is still compacting"
                    ))
                    .into());
            }
            let turn = data.get("turn").and_then(Value::as_u64);
            validate_owner(turn, trace.open_turn, "compaction/start", failure)?;
            Ok(Some(CompactionTransition::Start {
                compaction_id: CompactionId::new(required_string(data.get("compactionId"))?),
                source_command_id: data
                    .get("sourceCommandId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                start_seq: event.seq,
                turn,
            }))
        }
        "compaction/summary" => {
            validate_id(
                data.get("compactionId"),
                "compaction/summary compactionId",
                failure,
            )?;
            if data.get("sourceCommandId").is_some() {
                validate_id(
                    data.get("sourceCommandId"),
                    "compaction/summary sourceCommandId",
                    failure,
                )?;
            }
            let Some(open) = open else {
                return Err(failure
                    .fail("compaction/summary has no matching compaction/start")
                    .into());
            };
            if data.get("compactionId").and_then(Value::as_str) != Some(open.compaction_id.as_str())
            {
                return Err(failure
                    .fail(format!(
                        "compaction/summary id {} does not match compaction/start id {}",
                        js_string_opt(data.get("compactionId")),
                        open.compaction_id.as_str()
                    ))
                    .into());
            }
            validate_source_command_id(
                "compaction/summary",
                data.get("sourceCommandId"),
                open.source_command_id.as_deref(),
                failure,
            )?;
            validate_owner(open.turn, trace.open_turn, "compaction/summary", failure)?;
            if open.summarized {
                return Err(failure
                    .fail("compaction/summary repeated within one compaction")
                    .into());
            }
            let seqs = data.get("shadowedSeqs").and_then(Value::as_array);
            let Some(seqs) = seqs.filter(|seqs| !seqs.is_empty()) else {
                return Err(failure
                    .fail("compaction/summary shadowedSeqs must be non-empty")
                    .into());
            };
            let range = data.get("shadowedRange");
            let start = range.and_then(|r| r.get("start")).and_then(Value::as_u64);
            let end = range.and_then(|r| r.get("end")).and_then(Value::as_u64);
            if seqs.first().and_then(Value::as_u64) != start
                || seqs.last().and_then(Value::as_u64) != end
            {
                return Err(failure
                    .fail(
                        "compaction/summary shadowedRange must match the first and last shadowedSeqs",
                    )
                    .into());
            }
            if !data.get("shadowedTokenCount").is_some_and(Value::is_u64) {
                return Err(failure
                    .fail(
                        "compaction/summary shadowedTokenCount must be a non-negative safe integer",
                    )
                    .into());
            }
            Ok(Some(CompactionTransition::Summary {
                compaction_id: open.compaction_id.clone(),
                source_command_id: open.source_command_id.clone(),
                start_seq: open.start_seq,
                turn: open.turn,
            }))
        }
        "compaction/end" => {
            validate_id(
                data.get("compactionId"),
                "compaction/end compactionId",
                failure,
            )?;
            if data.get("sourceCommandId").is_some() {
                validate_id(
                    data.get("sourceCommandId"),
                    "compaction/end sourceCommandId",
                    failure,
                )?;
            }
            let Some(open) = open else {
                return Err(failure
                    .fail("compaction/end has no matching compaction/start")
                    .into());
            };
            if data.get("compactionId").and_then(Value::as_str) != Some(open.compaction_id.as_str())
            {
                return Err(failure
                    .fail(format!(
                        "compaction/end id {} does not match compaction/start id {}",
                        js_string_opt(data.get("compactionId")),
                        open.compaction_id.as_str()
                    ))
                    .into());
            }
            validate_source_command_id(
                "compaction/end",
                data.get("sourceCommandId"),
                open.source_command_id.as_deref(),
                failure,
            )?;
            let turn = data.get("turn").and_then(Value::as_u64);
            if turn != open.turn {
                return Err(failure
                    .fail(format!(
                        "compaction/end owner {} does not match compaction/start owner {}",
                        data.get("turn")
                            .map_or_else(|| "undefined".to_owned(), js_string),
                        open.turn
                            .map_or_else(|| "null".to_owned(), |turn| turn.to_string())
                    ))
                    .into());
            }
            validate_owner(open.turn, trace.open_turn, "compaction/end", failure)?;
            if data.get("error").is_none() && !open.summarized {
                return Err(failure
                    .fail("successful compaction/end requires one compaction/summary")
                    .into());
            }
            Ok(Some(CompactionTransition::End))
        }
        _ => Ok(None),
    }
}

fn validate_checkpoint(
    trace: &SessionTrace,
    event: &SessionEvent,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    let source = event.data.get("source").cloned().unwrap_or(Value::Null);
    validate_id(
        source.get("compactionId"),
        "compaction checkpoint compactionId",
        failure,
    )?;
    if source.get("sourceCommandId").is_some() {
        validate_id(
            source.get("sourceCommandId"),
            "compaction checkpoint sourceCommandId",
            failure,
        )?;
    }
    let Some(open) = &trace.compaction else {
        return Err(failure
            .fail("compaction checkpoint has no matching compaction/start")
            .into());
    };
    if source.get("compactionId").and_then(Value::as_str) != Some(open.compaction_id.as_str()) {
        return Err(failure
            .fail(format!(
                "compaction checkpoint id {} does not match compaction/start id {}",
                js_string_opt(source.get("compactionId")),
                open.compaction_id.as_str()
            ))
            .into());
    }
    validate_source_command_id(
        "compaction checkpoint",
        source.get("sourceCommandId"),
        open.source_command_id.as_deref(),
        failure,
    )
}

fn apply_compaction_transition(transition: CompactionTransition) -> Option<CompactionTrace> {
    match transition {
        CompactionTransition::Start {
            compaction_id,
            source_command_id,
            start_seq,
            turn,
        } => Some(CompactionTrace {
            compaction_id,
            source_command_id,
            start_seq,
            turn,
            summarized: false,
        }),
        CompactionTransition::Summary {
            compaction_id,
            source_command_id,
            start_seq,
            turn,
        } => Some(CompactionTrace {
            compaction_id,
            source_command_id,
            start_seq,
            turn,
            summarized: true,
        }),
        CompactionTransition::End | CompactionTransition::EndSeed => None,
    }
}

fn validate_id(
    value: Option<&Value>,
    label: &str,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    if value.and_then(Value::as_str).is_none_or(str::is_empty) {
        return Err(failure
            .fail(format!("{label} must be a non-empty string"))
            .into());
    }
    Ok(())
}

fn validate_source_command_id(
    event_type: &str,
    value: Option<&Value>,
    expected: Option<&str>,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    if value.is_some() {
        validate_id(value, &format!("{event_type} sourceCommandId"), failure)?;
    }
    if value.and_then(Value::as_str) != expected {
        return Err(failure
            .fail(format!(
                "{event_type} sourceCommandId {} does not match compaction/start sourceCommandId {}",
                value.map_or_else(|| "undefined".to_owned(), js_string),
                expected.map_or("undefined".to_owned(), str::to_owned)
            ))
            .into());
    }
    Ok(())
}

fn checkpoint_source(data: &Value) -> Option<MessageSource> {
    let source = data.get("source")?;
    serde_json::from_value(source.clone()).ok()
}

fn required_string(value: Option<&Value>) -> anyhow::Result<String> {
    value
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("compaction id must be a string"))
}

fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(values) => values
            .iter()
            .map(|value| match value {
                Value::Null => String::new(),
                other => js_string(other),
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

fn js_string_opt(value: Option<&Value>) -> String {
    value.map_or_else(|| "undefined".to_owned(), js_string)
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
