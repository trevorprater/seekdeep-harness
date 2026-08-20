//! Behavioral mirror of `packages/workflow/tool-workflow/tests/invariant.spec.ts`.

use std::sync::Arc;

use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{AppendOptions, Session, SessionError, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_tool_workflow::invariant::register_invariant;
use serde_json::{Value, json};

async fn setup() -> (Context, Arc<Session>) {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let session = sessions
        .create(&context, None, CreateSessionOptions::default())
        .expect("session");
    let invariants =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
    let registration = register_invariant(&invariants).expect("register");
    registration.await_ready().await.expect("ready");
    (context, session)
}

fn run_start(session: &Session, id: &str, name: &str) {
    session
        .append(
            "tool-workflow/run-start",
            json!({"runId": id, "name": name}),
            AppendOptions::default(),
        )
        .expect("run-start");
}

fn agent_start(session: &Session, id: &str, seq: u64, label: &str) {
    session
        .append(
            "tool-workflow/agent-start",
            json!({"runId": id, "seq": seq, "label": label, "childId": format!("child-{seq}")}),
            AppendOptions::default(),
        )
        .expect("agent-start");
}

#[tokio::test]
async fn accepts_interleaved_complete_runs_and_an_unfinished_continuous_prefix() {
    let (_, session) = setup().await;
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .unwrap();
    session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .unwrap();

    run_start(&session, "first", "first");
    run_start(&session, "second", "second");
    agent_start(&session, "second", 1, "");
    session
        .append(
            "tool-workflow/run-end",
            json!({"runId": "first", "stopReason": "completed"}),
            AppendOptions::default(),
        )
        .unwrap();
    session
        .append(
            "tool-workflow/agent-end",
            json!({"runId": "second", "seq": 1, "outcome": "cancelled"}),
            AppendOptions::default(),
        )
        .unwrap();
    session
        .append(
            "tool-workflow/run-end",
            json!({"runId": "second", "stopReason": "cancelled"}),
            AppendOptions::default(),
        )
        .unwrap();

    run_start(&session, "third", "third");
    agent_start(&session, "third", 1, "failed");
    session
        .append(
            "tool-workflow/agent-end",
            json!({"runId": "third", "seq": 1, "outcome": "failed"}),
            AppendOptions::default(),
        )
        .unwrap();
    session
        .append(
            "tool-workflow/run-end",
            json!({"runId": "third", "stopReason": "error"}),
            AppendOptions::default(),
        )
        .unwrap();

    run_start(&session, "prefix", "prefix");
    agent_start(&session, "prefix", 1, "open");
}

#[tokio::test]
async fn rejects_a_malformed_candidate_before_commit_and_keeps_the_fold_reusable() {
    let (_, session) = setup().await;
    run_start(&session, "run", "run");
    let error = session
        .append(
            "tool-workflow/agent-end",
            json!({"runId": "run", "seq": 1, "outcome": "completed"}),
            AppendOptions::default(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("seekdeep-tool-workflow"));
    assert!(error.to_string().contains("no matching member seq"));
    // The fold stays usable: a valid run-end still commits.
    session
        .append(
            "tool-workflow/run-end",
            json!({"runId": "run", "stopReason": "completed"}),
            AppendOptions::default(),
        )
        .unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn rejects_invalid_workflow_candidates() {
    type Case = (
        &'static str,
        fn(&Session) -> Result<(), SessionError>,
        &'static str,
    );
    let cases: Vec<Case> = vec![
        (
            "null data",
            |s| {
                s.append(
                    "tool-workflow/run-start",
                    Value::Null,
                    AppendOptions::default(),
                )
                .map(|_| ())
            },
            "data must be a JSON object",
        ),
        (
            "primitive data",
            |s| {
                s.append(
                    "tool-workflow/run-start",
                    json!(1),
                    AppendOptions::default(),
                )
                .map(|_| ())
            },
            "data must be a JSON object",
        ),
        (
            "array data",
            |s| {
                s.append(
                    "tool-workflow/run-start",
                    json!([]),
                    AppendOptions::default(),
                )
                .map(|_| ())
            },
            "data must be a JSON object",
        ),
        (
            "numeric run id",
            |s| {
                s.append(
                    "tool-workflow/agent-start",
                    json!({"runId": 1, "seq": 1, "label": "bad", "childId": "child"}),
                    AppendOptions::default(),
                )
                .map(|_| ())
            },
            "runId must be a non-empty string",
        ),
        (
            "empty run id",
            |s| {
                s.append(
                    "tool-workflow/agent-start",
                    json!({"runId": "", "seq": 1, "label": "bad", "childId": "child"}),
                    AppendOptions::default(),
                )
                .map(|_| ())
            },
            "runId must be a non-empty string",
        ),
        (
            "empty run name",
            |s| {
                s.append(
                    "tool-workflow/run-start",
                    json!({"runId": "empty-name", "name": ""}),
                    AppendOptions::default(),
                )
                .map(|_| ())
            },
            "name must be a non-empty string",
        ),
        (
            "non-string run name",
            |s| {
                s.append(
                    "tool-workflow/run-start",
                    json!({"runId": "bad-name", "name": 1}),
                    AppendOptions::default(),
                )
                .map(|_| ())
            },
            "name must be a non-empty string",
        ),
        (
            "duplicate run",
            |s| {
                s.append(
                    "tool-workflow/run-start",
                    json!({"runId": "run", "name": "again"}),
                    AppendOptions::default(),
                )
                .map(|_| ())
            },
            "repeats run",
        ),
        (
            "missing run",
            |s| {
                s.append(
                    "tool-workflow/agent-start",
                    json!({"runId": "missing", "seq": 1, "label": "bad", "childId": "child"}),
                    AppendOptions::default(),
                )
                .map(|_| ())
            },
            "no matching tool-workflow/run-start",
        ),
        (
            "non-positive member seq",
            |s| {
                s.append(
                    "tool-workflow/agent-start",
                    json!({"runId": "run", "seq": 0, "label": "bad", "childId": "child"}),
                    AppendOptions::default(),
                )
                .map(|_| ())
            },
            "positive safe integer",
        ),
        (
            "non-integer member seq",
            |s| {
                s.append(
                    "tool-workflow/agent-start",
                    json!({"runId": "run", "seq": 1.5, "label": "bad", "childId": "child"}),
                    AppendOptions::default(),
                )
                .map(|_| ())
            },
            "positive safe integer",
        ),
        (
            "non-string member label",
            |s| {
                s.append(
                    "tool-workflow/agent-start",
                    json!({"runId": "run", "seq": 1, "label": 1, "childId": "child"}),
                    AppendOptions::default(),
                )
                .map(|_| ())
            },
            "label must be a string",
        ),
        (
            "non-string member phase",
            |s| {
                s.append(
                    "tool-workflow/agent-start",
                    json!({"runId": "run", "seq": 1, "label": "bad", "phase": 1, "childId": "child"}),
                    AppendOptions::default(),
                )
                .map(|_| ())
            },
            "phase must be a string",
        ),
        (
            "empty child id",
            |s| {
                s.append(
                    "tool-workflow/agent-start",
                    json!({"runId": "run", "seq": 1, "label": "bad", "childId": ""}),
                    AppendOptions::default(),
                )
                .map(|_| ())
            },
            "childId must be a non-empty string",
        ),
        (
            "duplicate member start",
            |s| {
                s.append(
                    "tool-workflow/agent-start",
                    json!({"runId": "run", "seq": 1, "label": "one", "childId": "child"}),
                    AppendOptions::default(),
                )?;
                s.append(
                    "tool-workflow/agent-start",
                    json!({"runId": "run", "seq": 1, "label": "two", "childId": "child-2"}),
                    AppendOptions::default(),
                )
                .map(|_| ())
            },
            "repeats member seq",
        ),
        (
            "invalid member outcome",
            |s| {
                s.append(
                    "tool-workflow/agent-start",
                    json!({"runId": "run", "seq": 1, "label": "one", "childId": "child"}),
                    AppendOptions::default(),
                )?;
                s.append(
                    "tool-workflow/agent-end",
                    json!({"runId": "run", "seq": 1, "outcome": "unknown"}),
                    AppendOptions::default(),
                )
                .map(|_| ())
            },
            "outcome unknown is invalid",
        ),
        (
            "duplicate member end",
            |s| {
                s.append(
                    "tool-workflow/agent-start",
                    json!({"runId": "run", "seq": 1, "label": "one", "childId": "child"}),
                    AppendOptions::default(),
                )?;
                s.append(
                    "tool-workflow/agent-end",
                    json!({"runId": "run", "seq": 1, "outcome": "completed"}),
                    AppendOptions::default(),
                )?;
                s.append(
                    "tool-workflow/agent-end",
                    json!({"runId": "run", "seq": 1, "outcome": "completed"}),
                    AppendOptions::default(),
                )
                .map(|_| ())
            },
            "repeats member seq",
        ),
        (
            "run end with an open member",
            |s| {
                s.append(
                    "tool-workflow/agent-start",
                    json!({"runId": "run", "seq": 1, "label": "open", "childId": "child"}),
                    AppendOptions::default(),
                )?;
                s.append(
                    "tool-workflow/run-end",
                    json!({"runId": "run", "stopReason": "completed"}),
                    AppendOptions::default(),
                )
                .map(|_| ())
            },
            "leaves member seq 1 open",
        ),
        (
            "invalid run stop reason",
            |s| {
                s.append(
                    "tool-workflow/run-end",
                    json!({"runId": "run", "stopReason": "unknown"}),
                    AppendOptions::default(),
                )
                .map(|_| ())
            },
            "stopReason unknown is invalid",
        ),
        (
            "event after run end",
            |s| {
                s.append(
                    "tool-workflow/run-end",
                    json!({"runId": "run", "stopReason": "completed"}),
                    AppendOptions::default(),
                )?;
                s.append(
                    "tool-workflow/agent-start",
                    json!({"runId": "run", "seq": 1, "label": "late", "childId": "child"}),
                    AppendOptions::default(),
                )
                .map(|_| ())
            },
            "appears after",
        ),
        (
            "unknown workflow event",
            |s| {
                s.append(
                    "tool-workflow/unknown",
                    json!({"runId": "run"}),
                    AppendOptions::default(),
                )
                .map(|_| ())
            },
            "unknown tool-workflow event type",
        ),
    ];

    for (name, mutate, pattern) in cases {
        let (_, session) = setup().await;
        run_start(&session, "run", "run");
        let error = mutate(&session).unwrap_err();
        assert!(
            error.to_string().contains(pattern),
            "{name}: expected {pattern:?} in {error}"
        );
    }
}

#[tokio::test]
async fn validates_existing_cold_history_while_allowing_an_unfinished_prefix() {
    // Valid cold history: an unfinished continuous prefix is accepted.
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let valid = sessions
        .create(
            &context,
            Some(SessionId::new("workflow-record-cold-valid")),
            CreateSessionOptions::default(),
        )
        .expect("session");
    run_start(&valid, "valid", "valid");
    agent_start(&valid, "valid", 1, "open");
    let invariants =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
    let registration = register_invariant(&invariants).expect("register");
    registration
        .await_ready()
        .await
        .expect("valid cold history");

    // Broken cold history: an event after run-end rejects at registration.
    let broken_context = Context::new();
    let broken_sessions = SessionStore::install(&broken_context).expect("sessions");
    let broken = broken_sessions
        .create(
            &broken_context,
            Some(SessionId::new("workflow-record-cold-invalid")),
            CreateSessionOptions::default(),
        )
        .expect("session");
    run_start(&broken, "broken", "broken");
    broken
        .append(
            "tool-workflow/run-end",
            json!({"runId": "broken", "stopReason": "completed"}),
            AppendOptions::default(),
        )
        .unwrap();
    broken
        .append(
            "tool-workflow/agent-start",
            json!({"runId": "broken", "seq": 1, "label": "late", "childId": "late"}),
            AppendOptions::default(),
        )
        .unwrap();
    let broken_invariants =
        InvariantRegistry::install(&broken_context, &InvariantConfig::default())
            .expect("invariants");
    let broken_registration = register_invariant(&broken_invariants).expect("register");
    let error = broken_registration
        .await_ready()
        .await
        .expect_err("broken cold history");
    assert!(error.to_string().contains("appears after"));
}
