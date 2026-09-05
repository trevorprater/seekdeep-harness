//! Package-owned tool-pipeline invariants.

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

use crate::{ToolExecution, ToolExecutionResult, ToolExecutionToken};

const PACKAGE_NAME: &str = "seekdeep-tools";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolStage {
    Pre,
    Execute,
    Post,
}

#[derive(Debug, Default)]
struct SessionState {
    open_turn: bool,
    dispatch_roots: HashMap<String, String>,
}

#[derive(Debug, Default)]
struct InvariantState {
    stages: HashMap<ToolExecutionToken, ToolStage>,
    sessions: HashMap<usize, SessionState>,
}

/// Registers monotonic pipeline, immutable-result, and nested-dispatch checks.
///
/// Rust makes the JavaScript companion's `Object.isFrozen` checks structural:
/// tool event arguments are owned snapshots published through `Arc<T>`, and
/// their identity/result fields expose no shared mutable access. The live
/// companion therefore enforces the remaining relational contract.
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
        .ok_or_else(|| anyhow::anyhow!("seekdeep-tools invariant requires sessions"))?;
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
    // Presence and type of mode are part of Cordis's internal dispatch envelope.
    args.get::<DispatchMode>(0)
        .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks a dispatch mode"))?;
    let event_name = args
        .get::<String>(1)
        .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks an event name"))?;
    let event_args = args
        .get::<EventArgs>(2)
        .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks event arguments"))?;

    match event_name.as_str() {
        "session/event" => {
            let session = required_session(&event_args, "session/event")?;
            let event = required_event(&event_args)?;
            validate_session_event_before_commit(state, &session, &event, failure)
        }
        "tools/pre-execute" => {
            let execution = required_execution(&event_args, "tools/pre-execute")?;
            let mut state = state.lock();
            if state.stages.contains_key(&execution.token) {
                return Err(failure
                    .fail("tools/pre-execute repeated for one execution")
                    .into());
            }
            state.stages.insert(execution.token, ToolStage::Pre);
            Ok(())
        }
        "tools/execute" => {
            let execution = required_execution(&event_args, "tools/execute")?;
            let mut state = state.lock();
            if state.stages.get(&execution.token) != Some(&ToolStage::Pre) {
                return Err(failure
                    .fail("tools/execute must follow tools/pre-execute")
                    .into());
            }
            state.stages.insert(execution.token, ToolStage::Execute);
            Ok(())
        }
        "tools/post-execute" => {
            let execution = required_execution(&event_args, "tools/post-execute")?;
            let mut state = state.lock();
            if !matches!(
                state.stages.get(&execution.token),
                Some(ToolStage::Pre | ToolStage::Execute)
            ) {
                return Err(failure
                    .fail("tools/post-execute must follow tools/pre-execute or tools/execute")
                    .into());
            }
            state.stages.insert(execution.token, ToolStage::Post);
            Ok(())
        }
        "tools/result" => {
            let execution = required_execution(&event_args, "tools/result")?;
            event_args
                .get::<ToolExecutionResult>(1)
                .ok_or_else(|| anyhow::anyhow!("tools/result lacks its outcome"))?;
            if execution.name.is_empty() || execution.call_id.as_str().is_empty() {
                return Err(failure
                    .fail("tools/result execution must carry non-empty name and callId")
                    .into());
            }
            state.lock().stages.remove(&execution.token);
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate_session_event_before_commit(
    state: &Arc<Mutex<InvariantState>>,
    session: &Arc<Session>,
    event: &SessionEvent,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    let key = session_key(session);
    if !state.lock().sessions.contains_key(&key) {
        seed_session(state, session, failure)?;
    }
    let state = state.lock();
    let session_state = state
        .sessions
        .get(&key)
        .expect("session was seeded before validation");
    validate_dispatch(session_state, event, failure)?;
    if is_code_dispatch(event) && !session_state.open_turn {
        return Err(failure
            .fail(format!(
                "{} appended outside any open turn",
                event.event_type
            ))
            .into());
    }
    Ok(())
}

fn commit_published_event(
    state: &Arc<Mutex<InvariantState>>,
    session: &Arc<Session>,
    event: &SessionEvent,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    let key = session_key(session);
    if !state.lock().sessions.contains_key(&key) {
        seed_session(state, session, failure)?;
    }
    let mut state = state.lock();
    let session_state = state
        .sessions
        .get_mut(&key)
        .expect("session was seeded before publication");
    validate_dispatch(session_state, event, failure)?;
    commit_dispatch(session_state, event);
    match event.event_type.as_str() {
        "turn/start" => session_state.open_turn = true,
        "turn/end" => session_state.open_turn = false,
        _ => {}
    }
    Ok(())
}

fn seed_session(
    state: &Arc<Mutex<InvariantState>>,
    session: &Arc<Session>,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    let mut seeded = SessionState::default();
    for event in session.events() {
        validate_dispatch(&seeded, &event, failure)?;
        commit_dispatch(&mut seeded, &event);
        match event.event_type.as_str() {
            "turn/start" => seeded.open_turn = true,
            "turn/end" => seeded.open_turn = false,
            _ if is_code_dispatch(&event) && !seeded.open_turn => {
                return Err(failure
                    .fail(format!(
                        "{} appended outside any open turn",
                        event.event_type
                    ))
                    .into());
            }
            _ => {}
        }
    }
    state.lock().sessions.insert(session_key(session), seeded);
    Ok(())
}

fn validate_dispatch(
    state: &SessionState,
    event: &SessionEvent,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    if !is_code_dispatch(event) {
        return Ok(());
    }
    let root = js_string_property(&event.data, "rootCallId");
    let parent = js_string_property(&event.data, "parentCallId");
    let child = js_string_property(&event.data, "subCallId");
    if root.is_empty() || parent.is_empty() || child.is_empty() {
        return Err(failure
            .fail(format!(
                "{} must carry non-empty rootCallId, parentCallId, and subCallId",
                event.event_type
            ))
            .into());
    }
    if let Some(known) = state.dispatch_roots.get(&child)
        && known != &root
    {
        return Err(failure
            .fail(format!(
                "{} changed rootCallId for subCallId {child}",
                event.event_type
            ))
            .into());
    }
    if parent != root && state.dispatch_roots.get(&parent) != Some(&root) {
        return Err(failure
            .fail(format!(
                "{} parentCallId {parent} does not belong to rootCallId {root}",
                event.event_type
            ))
            .into());
    }
    Ok(())
}

fn commit_dispatch(state: &mut SessionState, event: &SessionEvent) {
    if is_code_dispatch(event) {
        state.dispatch_roots.insert(
            js_string_property(&event.data, "subCallId"),
            js_string_property(&event.data, "rootCallId"),
        );
    }
}

fn is_code_dispatch(event: &SessionEvent) -> bool {
    matches!(
        event.event_type.as_str(),
        "tool/code-dispatch-start" | "tool/code-dispatch"
    )
}

fn js_string_property(object: &Value, property: &str) -> String {
    object
        .get(property)
        .map_or_else(|| "undefined".to_owned(), js_string)
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

fn required_session(args: &EventArgs, event_name: &str) -> anyhow::Result<Arc<Session>> {
    args.get::<Session>(0)
        .ok_or_else(|| anyhow::anyhow!("{event_name} lacks its session"))
}

fn required_event(args: &EventArgs) -> anyhow::Result<Arc<SessionEvent>> {
    args.get::<SessionEvent>(1)
        .ok_or_else(|| anyhow::anyhow!("session/event lacks its event"))
}

fn required_execution(args: &EventArgs, event_name: &str) -> anyhow::Result<Arc<ToolExecution>> {
    args.get::<ToolExecution>(0)
        .ok_or_else(|| anyhow::anyhow!("{event_name} lacks its execution"))
}

fn session_key(session: &Arc<Session>) -> usize {
    Arc::as_ptr(session) as usize
}

#[cfg(test)]
mod tests {
    use seekdeep_cordis::EventReply;
    use seekdeep_core::{
        session::{AppendOptions, SessionId},
        session_store::{CreateSessionOptions, SessionStore},
    };
    use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
    use seekdeep_llm::{AbortSignal, CallId, ContentBlock};
    use serde_json::{Map, json};

    use super::*;
    use crate::{
        PostToolDecision, PreToolDecision, ScheduledToolPreparation, ToolDefinition,
        ToolExecutionInput, ToolOutputDefinition, ToolRuntime, ToolRuntimeConfig,
        assert_supported_json_schema,
    };

    struct Harness {
        context: Context,
        sessions: Arc<SessionStore>,
        runtime: Arc<ToolRuntime>,
    }

    async fn harness() -> Harness {
        let context = Context::new();
        let sessions = SessionStore::install(&context).expect("sessions");
        let invariants =
            InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
        let registration = register_invariant(&invariants).expect("register tools invariant");
        registration
            .await_ready()
            .await
            .expect("tools invariant ready");
        let runtime =
            ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).expect("tool runtime");
        runtime
            .register(&context, echo_definition())
            .expect("register echo");
        Harness {
            context,
            sessions,
            runtime,
        }
    }

    fn echo_definition() -> ToolDefinition {
        ToolDefinition::new(
            "echo",
            "Echo a value",
            Map::from_iter([("type".to_owned(), Value::String("object".to_owned()))]),
            ToolOutputDefinition::new(
                Arc::new(
                    assert_supported_json_schema(json!({"type": "string"})).expect("output schema"),
                ),
                Arc::new(|_, value| {
                    Ok(vec![ContentBlock::Text {
                        text: value.as_str().unwrap_or_default().to_owned(),
                    }])
                }),
            ),
            Arc::new(|_, _| Box::pin(async { Ok(json!("ok")) })),
        )
    }

    fn input(name: &str, call_id: &str) -> ToolExecutionInput {
        ToolExecutionInput::new(
            CallId::new(call_id),
            name,
            json!({}),
            AbortSignal::default(),
        )
    }

    fn outcome() -> ToolExecutionResult {
        ToolExecutionResult::success(
            json!("ok"),
            vec![ContentBlock::Text {
                text: "ok".to_owned(),
            }],
        )
    }

    async fn prepared_execution(
        runtime: &Arc<ToolRuntime>,
        name: &str,
        call_id: &str,
    ) -> ToolExecution {
        match runtime.prepare_scheduled(input(name, call_id)).await {
            ScheduledToolPreparation::Dispatch { execution }
            | ScheduledToolPreparation::PostResult { execution, .. }
            | ScheduledToolPreparation::FinalResult { execution, .. } => execution,
        }
    }

    fn emit_result(
        context: &Context,
        execution: &ToolExecution,
        result: &ToolExecutionResult,
    ) -> anyhow::Result<()> {
        context.events().emit(
            context,
            "tools/result",
            &EventArgs::from_values(vec![Arc::new(execution.clone()), Arc::new(result.clone())]),
        )
    }

    async fn stage(
        context: &Context,
        event_name: &str,
        execution: &ToolExecution,
    ) -> anyhow::Result<EventReply> {
        let args = if event_name == "tools/post-execute" {
            EventArgs::from_values(vec![Arc::new(execution.clone()), Arc::new(outcome())])
        } else {
            EventArgs::one(execution.clone())
        };
        let reply = match event_name {
            "tools/pre-execute" => EventReply::Value(Arc::new(PreToolDecision::Allow)),
            "tools/execute" => EventReply::Value(Arc::new(outcome())),
            "tools/post-execute" => EventReply::Value(Arc::new(PostToolDecision::default())),
            _ => panic!("unsupported test stage"),
        };
        context
            .events()
            .waterfall(context, event_name, &args, move || {
                Box::pin(async move { Ok(reply) })
            })
            .await
    }

    fn append(
        session: &Session,
        event_type: &str,
        data: Value,
    ) -> Result<SessionEvent, seekdeep_core::session::SessionError> {
        session.append(event_type, data, AppendOptions::default())
    }

    fn dispatch_data(root: &str, parent: &str, child: &str) -> Value {
        json!({
            "rootCallId": root,
            "parentCallId": parent,
            "subCallId": child,
            "name": "echo",
            "arguments": {},
        })
    }

    #[tokio::test]
    async fn accepts_dispatch_and_denial_stage_orders_with_immutable_results() {
        let harness = harness().await;
        let dispatched = harness.runtime.execute(input("echo", "call-1")).await;
        assert!(!dispatched.is_error());

        harness
            .runtime
            .on_pre_execute(
                &harness.context,
                |_, _| async {
                    Ok(PreToolDecision::Deny {
                        reason: "not now".to_owned(),
                    })
                },
                EventOptions::default(),
            )
            .expect("deny policy");
        let denied = harness.runtime.execute(input("echo", "call-2")).await;
        assert!(denied.is_error());
        harness
            .context
            .events()
            .emit(&harness.context, "tools/change", &EventArgs::default())
            .expect("unrelated event");
    }

    #[tokio::test]
    async fn rejects_repeated_and_out_of_order_pipeline_stages() {
        let harness = harness().await;
        let execution = prepared_execution(&harness.runtime, "echo", "call-1").await;
        let repeated = stage(&harness.context, "tools/pre-execute", &execution)
            .await
            .expect_err("repeated pre must fail");
        assert!(repeated.to_string().contains("repeated"));

        emit_result(&harness.context, &execution, &outcome()).expect("clear first stage");
        let execute = stage(&harness.context, "tools/execute", &execution)
            .await
            .expect_err("execute without pre must fail");
        assert!(
            execute
                .to_string()
                .contains("must follow tools/pre-execute")
        );
        let post = stage(&harness.context, "tools/post-execute", &execution)
            .await
            .expect_err("post without pre must fail");
        assert!(
            post.to_string()
                .contains("must follow tools/pre-execute or tools/execute")
        );
    }

    #[tokio::test]
    async fn final_snapshots_are_structurally_immutable_and_require_identity() {
        let harness = harness().await;
        let execution = prepared_execution(&harness.runtime, "echo", "call-1").await;
        let mut local = Arc::new(outcome());
        let published = EventArgs::from_values(vec![Arc::new(execution.clone()), local.clone()]);
        *Arc::make_mut(&mut local) = ToolExecutionResult::failure("changed locally");
        let captured = published
            .get::<ToolExecutionResult>(1)
            .expect("captured result");
        assert!(!captured.is_error());
        harness
            .context
            .events()
            .emit(&harness.context, "tools/result", &published)
            .expect("immutable snapshot");

        let anonymous = prepared_execution(&harness.runtime, "", "call-2").await;
        let error = emit_result(&harness.context, &anonymous, &outcome())
            .expect_err("empty name must fail");
        assert!(error.to_string().contains("non-empty name and callId"));

        let no_call_id = prepared_execution(&harness.runtime, "echo", "").await;
        let error = emit_result(&harness.context, &no_call_id, &outcome())
            .expect_err("empty call id must fail");
        assert!(error.to_string().contains("non-empty name and callId"));
    }

    #[tokio::test]
    async fn requires_code_dispatch_records_to_be_turn_enclosed() {
        let harness = harness().await;
        let session = harness
            .sessions
            .create(
                &harness.context,
                Some(SessionId::new("turn-enclosure")),
                CreateSessionOptions::default(),
            )
            .expect("session");
        let outside = append(
            &session,
            "tool/code-dispatch-start",
            dispatch_data("parent", "parent", "child"),
        )
        .expect_err("outside turn must fail");
        assert!(outside.to_string().contains("outside any open turn"));
        append(&session, "turn/start", json!({"turn": 1})).expect("turn start");
        append(
            &session,
            "tool/code-dispatch-start",
            dispatch_data("parent", "parent", "child"),
        )
        .expect("enclosed dispatch");
        append(
            &session,
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
        )
        .expect("turn end");
    }

    #[tokio::test]
    async fn rejected_dispatch_edge_is_not_committed_to_the_root_index() {
        let harness = harness().await;
        let session = harness
            .sessions
            .create(
                &harness.context,
                Some(SessionId::new("rejected-edge")),
                CreateSessionOptions::default(),
            )
            .expect("session");
        append(
            &session,
            "tool/code-dispatch-start",
            dispatch_data("rejected-root", "rejected-root", "reused-child"),
        )
        .expect_err("outside turn");
        append(&session, "turn/start", json!({"turn": 1})).expect("turn start");
        append(
            &session,
            "tool/code-dispatch-start",
            dispatch_data("accepted-root", "accepted-root", "reused-child"),
        )
        .expect("rejected edge was not committed");
    }

    #[tokio::test]
    async fn rejects_nested_dispatch_that_changes_the_parent_chain_root() {
        let harness = harness().await;
        let session = harness
            .sessions
            .create(
                &harness.context,
                Some(SessionId::new("nested-root")),
                CreateSessionOptions::default(),
            )
            .expect("session");
        append(&session, "turn/start", json!({"turn": 1})).expect("turn start");
        append(
            &session,
            "tool/code-dispatch-start",
            dispatch_data("root", "root", "child"),
        )
        .expect("child");
        append(
            &session,
            "tool/code-dispatch-start",
            dispatch_data("root", "child", "grandchild"),
        )
        .expect("grandchild");
        let error = append(
            &session,
            "tool/code-dispatch-start",
            dispatch_data("another-root", "child", "invalid-grandchild"),
        )
        .expect_err("changed parent root must fail");
        assert!(
            error
                .to_string()
                .contains("parentCallId child does not belong to rootCallId another-root")
        );
        assert!(!session.events().iter().any(|event| {
            event.event_type == "tool/code-dispatch-start"
                && event.data.get("subCallId") == Some(&json!("invalid-grandchild"))
        }));
    }

    #[tokio::test]
    async fn requires_nonempty_dispatch_ids_and_one_root_per_subcall() {
        let harness = harness().await;
        let session = harness
            .sessions
            .create(
                &harness.context,
                Some(SessionId::new("dispatch-identity")),
                CreateSessionOptions::default(),
            )
            .expect("session");
        append(&session, "turn/start", json!({"turn": 1})).expect("turn start");
        let empty = append(
            &session,
            "tool/code-dispatch-start",
            dispatch_data("", "root", "child"),
        )
        .expect_err("empty id must fail");
        assert!(
            empty
                .to_string()
                .contains("must carry non-empty rootCallId")
        );
        append(
            &session,
            "tool/code-dispatch-start",
            dispatch_data("root", "root", "child"),
        )
        .expect("first root");
        let changed = append(
            &session,
            "tool/code-dispatch-start",
            dispatch_data("other-root", "other-root", "child"),
        )
        .expect_err("subcall root changed");
        assert!(
            changed
                .to_string()
                .contains("changed rootCallId for subCallId child")
        );
    }

    #[tokio::test]
    async fn indexes_dispatch_records_emitted_for_a_bare_session() {
        let harness = harness().await;
        let session =
            Session::create(&SessionId::new("bare-dispatch"), None, None).expect("bare session");
        append(&session, "turn/start", json!({"turn": 1})).expect("turn start");
        let event = SessionEvent {
            event_type: "tool/code-dispatch-start".to_owned(),
            seq: 1,
            time: 1,
            data: dispatch_data("root", "root", "child"),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        };
        harness
            .context
            .events()
            .emit(
                &harness.context,
                "session/event",
                &EventArgs::from_values(vec![session, Arc::new(event)]),
            )
            .expect("bare event");
    }

    #[tokio::test]
    async fn replays_enclosed_dispatch_records_on_late_registration() {
        let context = Context::new();
        let sessions = SessionStore::install(&context).expect("sessions");
        let session = sessions
            .create(
                &context,
                Some(SessionId::new("late-enclosed")),
                CreateSessionOptions::default(),
            )
            .expect("session");
        append(&session, "turn/start", json!({"turn": 1})).expect("turn start");
        append(
            &session,
            "tool/code-dispatch",
            json!({
                "rootCallId": "parent",
                "parentCallId": "parent",
                "subCallId": "child",
                "name": "echo",
                "arguments": {},
                "isError": false,
                "content": [{"type": "text", "text": "ok"}],
            }),
        )
        .expect("dispatch");
        append(
            &session,
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
        )
        .expect("turn end");
        let invariants =
            InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
        register_invariant(&invariants)
            .expect("register")
            .await_ready()
            .await
            .expect("late replay accepted");
    }

    #[tokio::test]
    async fn rejects_unenclosed_dispatch_records_on_late_registration() {
        let context = Context::new();
        let sessions = SessionStore::install(&context).expect("sessions");
        let session = sessions
            .create(
                &context,
                Some(SessionId::new("late-unenclosed")),
                CreateSessionOptions::default(),
            )
            .expect("session");
        append(
            &session,
            "tool/code-dispatch-start",
            dispatch_data("parent", "parent", "child"),
        )
        .expect("no invariant installed yet");
        let invariants =
            InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
        let error = register_invariant(&invariants)
            .expect("register")
            .await_ready()
            .await
            .expect_err("late replay must reject");
        assert!(error.to_string().contains("outside any open turn"));
    }
}
