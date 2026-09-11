//! `SubagentRuntime.listChildren` and `listDescendants`, ported from the pinned
//! `list-children.spec.ts` against real JSONL session persistence. Cases that
//! monkey-patch the persistence backend or the projection cache in place are
//! not portable to the typed Rust services and are recorded in the manifest.

mod support;

use std::sync::Arc;

use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{
        AppendOptions, SESSION_FORMAT_VERSION, SessionEvent, SessionHeader, SessionId,
        SessionOrigin, SurfaceOp,
    },
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::{AbortSignal, MessageSource, UserMessage};
use seekdeep_session_persistence::SESSION_PERSISTENCE;
use seekdeep_session_projection::{
    ProjectionDefinition, ProjectionTransition, SESSION_PROJECTIONS, SessionProjectionRegistry,
};
use seekdeep_subagent::{
    SUBAGENT_DESCRIPTOR_VERSION, SubagentActivity, SubagentDescendantListEntry,
    SubagentDiagnosticReason, SubagentListEntry, SubagentListMode, SubagentRuntime,
    SubagentStartRequest,
};
use serde_json::{Value, json};
use support::continuation::*;

/// Boot the continuable stack with real JSONL session persistence and the
/// projection registry (unless a case leaves it unmounted).
async fn setup_listing(script: Vec<Entry>, projections: bool) -> Stack {
    let root = tempfile::tempdir().unwrap();
    let adapter = MockAdapter::new(script);
    let (context, dependencies, subagents) =
        boot_with(Some(root.path()), &adapter, true, projections).await;
    let parent = create_agent(&dependencies.agents, "parent", true).await;
    Stack {
        context,
        dependencies,
        subagents,
        adapter,
        parent,
        root: Some(root),
    }
}

/// Start one continuable child through the real service path and await Activation release.
async fn start_child(stack: &Stack, label: &str) -> SessionId {
    let mut spec = spawn_spec(&stack.parent.agent);
    spec.label = label.to_owned();
    spec.request.prompt = message(&format!("task: {label}"));
    let started = stack.subagents.start_continuable(spec).await.unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    started.child_id
}

struct HeaderOverrides {
    parent_session: Option<SessionId>,
    origin: Option<SessionOrigin>,
    created_at: Option<u64>,
    seed_length: Option<u64>,
}

fn under(parent: &SessionId) -> HeaderOverrides {
    HeaderOverrides {
        parent_session: Some(parent.clone()),
        origin: Some(SessionOrigin::Subagent),
        created_at: None,
        seed_length: None,
    }
}

/// Author one persisted child session directly against the persistence backend.
async fn author_child(
    stack: &Stack,
    id: &str,
    header: HeaderOverrides,
    events: Vec<SessionEvent>,
) -> SessionId {
    let session_id = SessionId::new(id);
    let mut meta = SessionHeader::new(session_id.clone());
    meta.version = SESSION_FORMAT_VERSION;
    meta.created_at = header.created_at.unwrap_or(1);
    meta.parent_session = header.parent_session;
    meta.origin = header.origin;
    meta.seed_length = header.seed_length;
    let persistence = stack
        .context
        .get(SESSION_PERSISTENCE)
        .expect("persistence")
        .persistence();
    persistence.create(&meta).await.unwrap();
    persistence.append(&session_id, &events).await.unwrap();
    session_id
}

fn event(event_type: &str, seq: u64, time: i64, data: Value) -> SessionEvent {
    SessionEvent {
        event_type: event_type.to_owned(),
        seq,
        time,
        data: data.into(),
        source_event_seqs: None,
        surface_op: None,
        ignorable: None,
    }
}

fn turn_start(seq: u64, time: i64, turn: u64) -> SessionEvent {
    event(
        "turn/start",
        seq,
        time,
        json!({ "turn": turn, "trigger": { "kind": "message", "source": { "kind": "user" } } }),
    )
}

fn turn_end(seq: u64, time: i64, turn: u64) -> SessionEvent {
    event(
        "turn/end",
        seq,
        time,
        json!({ "turn": turn, "reason": { "kind": "completed" } }),
    )
}

fn user_message_data(text: &str, source: MessageSource) -> Value {
    serde_json::to_value(UserMessage::new(message(text), source)).unwrap()
}

/// Minimal complete-turn child log with one descriptor payload.
fn child_events(descriptor: Value) -> Vec<SessionEvent> {
    let mut work = event(
        "user/message",
        1,
        2,
        user_message_data("work", MessageSource::user()),
    );
    work.surface_op = Some(SurfaceOp::append());
    vec![
        turn_start(0, 1, 1),
        work,
        event("subagent/descriptor", 2, 3, descriptor),
        turn_end(3, 4, 1),
    ]
}

fn descriptor_payload(label: &str) -> Value {
    descriptor_payload_version(label, SUBAGENT_DESCRIPTOR_VERSION)
}

fn descriptor_payload_version(label: &str, version: u32) -> Value {
    json!({ "version": version, "mode": "continuable", "provider": "spawn", "label": label })
}

fn continuable(
    id: &SessionId,
    label: &str,
    activity: SubagentActivity,
    has_children: bool,
) -> SubagentListEntry {
    SubagentListEntry::Child {
        id: id.clone(),
        activity,
        has_children,
        mode: SubagentListMode::Continuable {
            label: label.to_owned(),
        },
    }
}

fn one_shot(id: &SessionId, label: Option<&str>) -> SubagentListEntry {
    SubagentListEntry::Child {
        id: id.clone(),
        activity: SubagentActivity::Inactive,
        has_children: false,
        mode: SubagentListMode::OneShot {
            label: label.map(str::to_owned),
        },
    }
}

fn diagnostic(id: &SessionId, reason: SubagentDiagnosticReason) -> SubagentListEntry {
    SubagentListEntry::Diagnostic {
        id: id.clone(),
        reason,
    }
}

fn descendant(
    entry: SubagentListEntry,
    parent: &SessionId,
    depth: u64,
) -> SubagentDescendantListEntry {
    SubagentDescendantListEntry {
        entry,
        parent_id: parent.clone(),
        depth,
    }
}

async fn list(stack: &Stack, parent: &SessionId) -> Vec<SubagentListEntry> {
    stack.subagents.list_children(parent, None).await.unwrap()
}

async fn list_descendants(stack: &Stack, root: &SessionId) -> Vec<SubagentDescendantListEntry> {
    stack.subagents.list_descendants(root, None).await.unwrap()
}

/// Publish one live child session with a descriptor and the parent lineage,
/// without starting an Activation.
fn live_child(
    stack: &Stack,
    parent: &SessionId,
    id: &str,
    created_at: Option<u64>,
    label: &str,
) -> SessionId {
    let session = stack
        .dependencies
        .sessions
        .create(
            &stack.context,
            Some(SessionId::new(id)),
            CreateSessionOptions {
                parent_session: Some(parent.clone()),
                origin: Some(SessionOrigin::Subagent),
                created_at,
                ..CreateSessionOptions::default()
            },
        )
        .unwrap();
    session
        .append("turn/start", json!({ "turn": 1 }), AppendOptions::default())
        .unwrap();
    session
        .append(
            "subagent/descriptor",
            descriptor_payload(label),
            AppendOptions::default(),
        )
        .unwrap();
    session.header().id.clone()
}

/// A foreign registered unit that rejects one specific child's log at view
/// time: `apply` never fails, while the poisoned state detonates only when a
/// listing read folds or serves this child through the registry.
fn hostile_projection_definition() -> ProjectionDefinition {
    ProjectionDefinition::new(
        "subagentListHostileProbe",
        1,
        || Ok(json!({})),
        |_state, event| {
            if event.event_type == "subagent/descriptor" && event.data["label"] == "poison me" {
                Ok(ProjectionTransition::Changed(
                    json!({ "poisoned": true }).into(),
                ))
            } else {
                Ok(ProjectionTransition::Unchanged)
            }
        },
        |state| {
            if state["poisoned"] == true {
                anyhow::bail!("hostile unit rejects the poisoned log");
            }
            Ok(Value::Null)
        },
    )
}

#[tokio::test]
async fn lists_live_children_without_persistence_query_services_or_the_continuation_runtime() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    SessionProjectionRegistry::install(&context).unwrap();
    let subagents = SubagentRuntime::install(&context).unwrap();
    context.registry().await_quiescent().await;
    assert!(context.get(SESSION_PERSISTENCE).is_none());

    let parent_id = SessionId::new("live-only-parent");
    sessions
        .create(
            &context,
            Some(parent_id.clone()),
            CreateSessionOptions::default(),
        )
        .unwrap();
    let child_id = SessionId::new("live-only-child");
    let child = sessions
        .create(
            &context,
            Some(child_id.clone()),
            CreateSessionOptions {
                parent_session: Some(parent_id.clone()),
                origin: Some(SessionOrigin::Subagent),
                ..CreateSessionOptions::default()
            },
        )
        .unwrap();
    child
        .append("turn/start", json!({ "turn": 1 }), AppendOptions::default())
        .unwrap();
    child
        .append(
            "subagent/descriptor",
            descriptor_payload("live-only child"),
            AppendOptions::default(),
        )
        .unwrap();

    assert_eq!(
        subagents.list_children(&parent_id, None).await.unwrap(),
        [continuable(
            &child_id,
            "live-only child",
            SubagentActivity::Running,
            false
        )]
    );
}

#[tokio::test]
async fn fails_loud_when_the_projection_registry_is_not_mounted_even_with_no_children() {
    let stack = setup_listing(vec![], false).await;
    let error = stack
        .subagents
        .list_children(stack.parent.agent.id(), None)
        .await
        .unwrap_err();
    assert_eq!(
        error_code(&error).as_deref(),
        Some("SUBAGENT_CONTROL_PROJECTIONS_UNAVAILABLE")
    );
}

#[tokio::test]
async fn fails_loud_when_the_session_store_is_not_mounted() {
    let context = Context::new();
    SessionProjectionRegistry::install(&context).unwrap();
    let subagents = SubagentRuntime::install(&context).unwrap();
    context.registry().await_quiescent().await;
    let error = subagents
        .list_children(&SessionId::new("no-store-parent"), None)
        .await
        .unwrap_err();
    assert_eq!(
        error_code(&error).as_deref(),
        Some("SUBAGENT_CONTROL_SESSION_STORE_UNAVAILABLE")
    );
}

#[tokio::test]
async fn lists_a_persisted_continuable_child_as_inactive_with_its_durable_label() {
    let stack = setup_listing(vec![Entry::chunks(text_response("done"))], true).await;
    let child_id = start_child(&stack, "summarize the doc").await;
    assert_eq!(
        list(&stack, stack.parent.agent.id()).await,
        [continuable(
            &child_id,
            "summarize the doc",
            SubagentActivity::Inactive,
            false
        )]
    );
}

#[tokio::test]
async fn lists_one_shot_and_continuable_children_under_the_same_parent() {
    let stack = setup_listing(
        vec![
            Entry::chunks(text_response("once")),
            Entry::chunks(text_response("again")),
        ],
        true,
    )
    .await;
    let run = stack
        .subagents
        .start(
            "spawn",
            SubagentStartRequest {
                label: None,
                prompt: message("finish once"),
                parent: Arc::clone(&stack.parent.agent),
                signal: AbortSignal::default(),
                agent_options: None,
                output_schema: None,
                max_depth: None,
                tool_filter: None,
                persona: None,
            },
        )
        .await
        .unwrap();
    let one_shot_id = run.id().clone();
    run.result().await.unwrap();
    run.dispose().await.unwrap();
    let continuable_id = start_child(&stack, "continuable child").await;

    let entries = list(&stack, stack.parent.agent.id()).await;
    assert_eq!(entries.len(), 2);
    assert!(entries.contains(&one_shot(&one_shot_id, None)));
    assert!(entries.contains(&continuable(
        &continuable_id,
        "continuable child",
        SubagentActivity::Inactive,
        false
    )));
}

#[tokio::test]
async fn accepts_a_persisted_non_live_parent_target_after_restart() {
    let stack = setup_listing(vec![], true).await;
    let cold_parent = SessionId::new("00000000-0000-4000-8000-00000000cccc");
    let persistence = stack
        .context
        .get(SESSION_PERSISTENCE)
        .unwrap()
        .persistence();
    let mut meta = SessionHeader::new(cold_parent.clone());
    meta.created_at = 1;
    persistence.create(&meta).await.unwrap();
    persistence
        .append(&cold_parent, &[turn_start(0, 1, 1), turn_end(1, 2, 1)])
        .await
        .unwrap();
    let child_id = author_child(
        &stack,
        "00000000-0000-4000-8000-00000000cdcd",
        under(&cold_parent),
        child_events(descriptor_payload("persisted parent case")),
    )
    .await;
    assert_eq!(
        list(&stack, &cold_parent).await,
        [continuable(
            &child_id,
            "persisted parent case",
            SubagentActivity::Inactive,
            false
        )]
    );
}

#[tokio::test]
async fn orders_children_by_created_at_then_id_without_listing_ordinary_forks() {
    let stack = setup_listing(vec![], true).await;
    let parent_id = stack.parent.agent.id().clone();
    let late = live_child(
        &stack,
        &parent_id,
        "00000000-0000-4000-8000-000000000009",
        Some(9),
        "late child",
    );
    let tie_b = live_child(
        &stack,
        &parent_id,
        "00000000-0000-4000-8000-000000000002",
        Some(5),
        "tie b",
    );
    let tie_a = live_child(
        &stack,
        &parent_id,
        "00000000-0000-4000-8000-000000000001",
        Some(5),
        "tie a",
    );
    let fork = stack
        .dependencies
        .sessions
        .fork(
            &stack.context,
            stack.parent.agent.session(),
            None,
            Some(SessionId::new("plain-fork")),
        )
        .unwrap();
    stack.dependencies.sessions.flush(&fork).await.unwrap();
    let entries = list(&stack, &parent_id).await;
    let ids = entries
        .iter()
        .map(|entry| match entry {
            SubagentListEntry::Child { id, .. } | SubagentListEntry::Diagnostic { id, .. } => {
                id.clone()
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(ids, [tie_a, tie_b, late]);
    assert!(
        entries
            .iter()
            .all(|entry| matches!(entry, SubagentListEntry::Child { .. }))
    );
}

#[tokio::test]
async fn omits_a_live_child_that_has_not_appended_its_descriptor_yet() {
    let stack = setup_listing(vec![], true).await;
    let pending = stack
        .dependencies
        .sessions
        .create(
            &stack.context,
            Some(SessionId::new("creation-window-child")),
            CreateSessionOptions {
                parent_session: Some(stack.parent.agent.id().clone()),
                origin: Some(SessionOrigin::Subagent),
                ..CreateSessionOptions::default()
            },
        )
        .unwrap();
    pending
        .append("turn/start", json!({ "turn": 1 }), AppendOptions::default())
        .unwrap();
    assert!(list(&stack, stack.parent.agent.id()).await.is_empty());
}

#[tokio::test]
async fn lists_a_one_shot_child_with_its_durable_creation_label() {
    let stack = setup_listing(vec![], true).await;
    let labeled = author_child(
        &stack,
        "00000000-0000-4000-8000-00000000ab02",
        under(stack.parent.agent.id()),
        child_events(json!({
            "version": SUBAGENT_DESCRIPTOR_VERSION,
            "mode": "one-shot",
            "provider": "spawn",
            "label": "labeled one-shot",
        })),
    )
    .await;
    assert_eq!(
        list(&stack, stack.parent.agent.id()).await,
        [one_shot(&labeled, Some("labeled one-shot"))]
    );
}

#[tokio::test]
async fn reports_a_live_child_as_running_while_keeping_settled_siblings_complete() {
    let stack = setup_listing(vec![Entry::chunks(text_response("done"))], true).await;
    let settled = start_child(&stack, "settled child").await;
    let live_id = live_child(
        &stack,
        stack.parent.agent.id(),
        "live-child",
        None,
        "live child",
    );
    let entries = list(&stack, stack.parent.agent.id()).await;
    assert!(entries.contains(&continuable(
        &settled,
        "settled child",
        SubagentActivity::Inactive,
        false
    )));
    assert!(entries.contains(&continuable(
        &live_id,
        "live child",
        SubagentActivity::Running,
        false
    )));
}

#[tokio::test]
async fn lists_the_last_descriptor_when_a_log_carries_more_than_one() {
    let stack = setup_listing(vec![Entry::chunks(text_response("done"))], true).await;
    let healthy = start_child(&stack, "healthy sibling").await;
    let mut events = child_events(descriptor_payload("twice"));
    events.insert(
        3,
        event(
            "subagent/descriptor",
            3,
            3,
            descriptor_payload("twice again"),
        ),
    );
    events[4].seq = 4;
    let doubled = author_child(
        &stack,
        "00000000-0000-4000-8000-00000000dupe",
        under(stack.parent.agent.id()),
        events,
    )
    .await;
    let entries = list(&stack, stack.parent.agent.id()).await;
    assert!(entries.contains(&continuable(
        &doubled,
        "twice again",
        SubagentActivity::Inactive,
        false
    )));
    assert!(entries.contains(&continuable(
        &healthy,
        "healthy sibling",
        SubagentActivity::Inactive,
        false
    )));
}

#[tokio::test]
async fn serves_the_serializable_null_sentinel_when_a_later_descriptor_invalidates_the_identity() {
    let stack = setup_listing(vec![], true).await;
    let projections = stack.context.get(SESSION_PROJECTIONS).unwrap();
    let live_id = SessionId::new("invalidated-live-child");
    let live = stack
        .dependencies
        .sessions
        .create(
            &stack.context,
            Some(live_id.clone()),
            CreateSessionOptions {
                parent_session: Some(stack.parent.agent.id().clone()),
                origin: Some(SessionOrigin::Subagent),
                ..CreateSessionOptions::default()
            },
        )
        .unwrap();
    live.append("turn/start", json!({ "turn": 1 }), AppendOptions::default())
        .unwrap();
    live.append(
        "subagent/descriptor",
        descriptor_payload("was valid"),
        AppendOptions::default(),
    )
    .unwrap();
    assert_eq!(
        projections.snapshot(&live).unwrap().values["subagent"],
        json!({ "mode": "continuable", "label": "was valid", "seq": 1 })
    );
    live.append(
        "subagent/descriptor",
        json!({ "version": SUBAGENT_DESCRIPTOR_VERSION, "mode": "continuable", "provider": 7 }),
        AppendOptions::default(),
    )
    .unwrap();
    let values = projections.snapshot(&live).unwrap().values;
    assert_eq!(values["subagent"], Value::Null);
    let wired: Value = serde_json::from_str(&serde_json::to_string(&values).unwrap()).unwrap();
    assert!(wired.as_object().unwrap().contains_key("subagent"));
    assert_eq!(wired["subagent"], Value::Null);
    assert!(list(&stack, stack.parent.agent.id()).await.is_empty());
}

#[tokio::test]
async fn diagnoses_a_settled_child_whose_later_descriptor_invalidated_the_identity_as_corrupt() {
    let stack = setup_listing(vec![], true).await;
    let mut events = child_events(descriptor_payload("was valid"));
    events.insert(
        3,
        event(
            "subagent/descriptor",
            3,
            3,
            json!({ "version": SUBAGENT_DESCRIPTOR_VERSION, "mode": "continuable", "provider": 7 }),
        ),
    );
    events[4].seq = 4;
    let invalidated = author_child(
        &stack,
        "00000000-0000-4000-8000-00000000ad01",
        under(stack.parent.agent.id()),
        events,
    )
    .await;
    assert_eq!(
        list(&stack, stack.parent.agent.id()).await,
        [diagnostic(&invalidated, SubagentDiagnosticReason::Corrupt)]
    );
}

#[tokio::test]
async fn maps_a_child_rejected_by_persistence_inspection_to_unavailable() {
    let stack = setup_listing(vec![], true).await;
    let invalid = author_child(
        &stack,
        "00000000-0000-4000-8000-0000000000ee",
        under(stack.parent.agent.id()),
        vec![
            turn_start(0, 1, 1),
            event(
                "user/message",
                1,
                2,
                user_message_data("work", MessageSource::user()),
            ),
            event(
                "subagent/descriptor",
                2,
                3,
                descriptor_payload("broken surface"),
            ),
        ],
    )
    .await;
    assert_eq!(
        list(&stack, stack.parent.agent.id()).await,
        [diagnostic(&invalid, SubagentDiagnosticReason::Unavailable)]
    );
}

#[tokio::test]
async fn diagnoses_a_malformed_descriptor_payload_as_corrupt() {
    let stack = setup_listing(vec![], true).await;
    let malformed = author_child(
        &stack,
        "00000000-0000-4000-8000-0000000000ff",
        under(stack.parent.agent.id()),
        child_events(
            json!({ "version": SUBAGENT_DESCRIPTOR_VERSION, "mode": "continuable", "provider": 7 }),
        ),
    )
    .await;
    assert_eq!(
        list(&stack, stack.parent.agent.id()).await,
        [diagnostic(&malformed, SubagentDiagnosticReason::Corrupt)]
    );
}

#[tokio::test]
async fn diagnoses_an_unknown_descriptor_version_as_corrupt() {
    let stack = setup_listing(vec![], true).await;
    let future = author_child(
        &stack,
        "00000000-0000-4000-8000-0000000000aa",
        under(stack.parent.agent.id()),
        child_events(descriptor_payload_version(
            "from the future",
            SUBAGENT_DESCRIPTOR_VERSION + 1,
        )),
    )
    .await;
    assert_eq!(
        list(&stack, stack.parent.agent.id()).await,
        [diagnostic(&future, SubagentDiagnosticReason::Corrupt)]
    );
}

#[tokio::test]
async fn lists_a_fork_whose_seed_replays_an_ancestor_descriptor_under_that_identity() {
    let stack = setup_listing(vec![], true).await;
    let seed = child_events(descriptor_payload("ancestor label"));
    let mut header = under(stack.parent.agent.id());
    header.seed_length = Some(seed.len() as u64);
    let fork_child =
        author_child(&stack, "00000000-0000-4000-8000-0000000000f0", header, seed).await;
    assert_eq!(
        list(&stack, stack.parent.agent.id()).await,
        [continuable(
            &fork_child,
            "ancestor label",
            SubagentActivity::Inactive,
            false
        )]
    );
}

#[tokio::test]
async fn does_not_filter_by_provider_availability() {
    let stack = setup_listing(vec![], true).await;
    let foreign = author_child(
        &stack,
        "00000000-0000-4000-8000-0000000000bb",
        under(stack.parent.agent.id()),
        child_events(json!({
            "version": SUBAGENT_DESCRIPTOR_VERSION,
            "mode": "continuable",
            "provider": "not-mounted",
            "label": "orphan provider",
        })),
    )
    .await;
    assert_eq!(
        list(&stack, stack.parent.agent.id()).await,
        [continuable(
            &foreign,
            "orphan provider",
            SubagentActivity::Inactive,
            false
        )]
    );
}

#[tokio::test]
async fn contains_a_foreign_unit_failure_during_a_cold_fold_to_that_child_as_corrupt() {
    let stack = setup_listing(vec![Entry::chunks(text_response("done"))], true).await;
    let _hostile = stack
        .context
        .get(SESSION_PROJECTIONS)
        .unwrap()
        .register(&stack.context, hostile_projection_definition())
        .unwrap();
    let healthy = start_child(&stack, "healthy sibling").await;
    let poisoned = author_child(
        &stack,
        "00000000-0000-4000-8000-00000000d00d",
        under(stack.parent.agent.id()),
        child_events(descriptor_payload("poison me")),
    )
    .await;
    let entries = list(&stack, stack.parent.agent.id()).await;
    assert!(entries.contains(&diagnostic(&poisoned, SubagentDiagnosticReason::Corrupt)));
    assert!(entries.contains(&continuable(
        &healthy,
        "healthy sibling",
        SubagentActivity::Inactive,
        false
    )));
}

#[tokio::test]
async fn contains_a_foreign_unit_failure_during_a_live_snapshot_to_that_child_as_corrupt() {
    let stack = setup_listing(vec![], true).await;
    let _hostile = stack
        .context
        .get(SESSION_PROJECTIONS)
        .unwrap()
        .register(&stack.context, hostile_projection_definition())
        .unwrap();
    let poisoned_id = live_child(
        &stack,
        stack.parent.agent.id(),
        "live-poisoned-child",
        None,
        "poison me",
    );
    let healthy_id = live_child(
        &stack,
        stack.parent.agent.id(),
        "live-healthy-child",
        None,
        "live healthy",
    );
    let entries = list(&stack, stack.parent.agent.id()).await;
    assert!(entries.contains(&diagnostic(&poisoned_id, SubagentDiagnosticReason::Corrupt)));
    assert!(entries.contains(&continuable(
        &healthy_id,
        "live healthy",
        SubagentActivity::Running,
        false
    )));
}

#[tokio::test]
async fn lists_compacted_and_uncompacted_children_identically() {
    let stack = setup_listing(vec![], true).await;
    let mut plain_header = under(stack.parent.agent.id());
    plain_header.created_at = Some(1);
    let plain = author_child(
        &stack,
        "00000000-0000-4000-8000-00000000c0de",
        plain_header,
        child_events(descriptor_payload("twin child")),
    )
    .await;
    let mut compacted_events = child_events(descriptor_payload("twin child"));
    let mut summary = event(
        "user/message",
        4,
        5,
        user_message_data("summary of everything", MessageSource::plugin("compact")),
    );
    summary.surface_op = Some(SurfaceOp::replace(1, 1));
    summary.source_event_seqs = Some(vec![1]);
    compacted_events.push(summary);
    let mut compacted_header = under(stack.parent.agent.id());
    compacted_header.created_at = Some(2);
    let compacted = author_child(
        &stack,
        "00000000-0000-4000-8000-00000000c1de",
        compacted_header,
        compacted_events,
    )
    .await;
    assert_eq!(
        list(&stack, stack.parent.agent.id()).await,
        [
            continuable(&plain, "twin child", SubagentActivity::Inactive, false),
            continuable(&compacted, "twin child", SubagentActivity::Inactive, false),
        ]
    );
}

#[tokio::test]
async fn reports_an_origin_classified_grandchild_without_inspecting_it() {
    let stack = setup_listing(vec![Entry::chunks(text_response("done"))], true).await;
    let child_id = start_child(&stack, "direct child").await;
    author_child(
        &stack,
        "00000000-0000-4000-8000-0000000000cc",
        under(&child_id),
        child_events(descriptor_payload("grandchild")),
    )
    .await;
    assert_eq!(
        list(&stack, stack.parent.agent.id()).await,
        [continuable(
            &child_id,
            "direct child",
            SubagentActivity::Inactive,
            true
        )]
    );
}

#[tokio::test]
async fn does_not_count_an_ordinary_grandchild_without_subagent_origin() {
    let stack = setup_listing(vec![Entry::chunks(text_response("done"))], true).await;
    let child_id = start_child(&stack, "direct child").await;
    author_child(
        &stack,
        "00000000-0000-4000-8000-0000000000f1",
        HeaderOverrides {
            parent_session: Some(child_id.clone()),
            origin: None,
            created_at: None,
            seed_length: None,
        },
        vec![turn_start(0, 1, 1), turn_end(1, 2, 1)],
    )
    .await;
    assert_eq!(
        list(&stack, stack.parent.agent.id()).await,
        [continuable(
            &child_id,
            "direct child",
            SubagentActivity::Inactive,
            false
        )]
    );
}

#[tokio::test]
async fn counts_an_origin_classified_diagnostic_grandchild() {
    let stack = setup_listing(vec![Entry::chunks(text_response("done"))], true).await;
    let child_id = start_child(&stack, "direct child").await;
    let diagnostic_id = author_child(
        &stack,
        "00000000-0000-4000-8000-0000000000f2",
        under(&child_id),
        child_events(
            json!({ "version": SUBAGENT_DESCRIPTOR_VERSION, "mode": "continuable", "provider": 7 }),
        ),
    )
    .await;
    assert_eq!(
        list(&stack, &child_id).await,
        [diagnostic(
            &diagnostic_id,
            SubagentDiagnosticReason::Corrupt
        )]
    );
    assert_eq!(
        list(&stack, stack.parent.agent.id()).await,
        [continuable(
            &child_id,
            "direct child",
            SubagentActivity::Inactive,
            true
        )]
    );
}

#[tokio::test]
async fn a_pre_aborted_signal_stops_before_any_persistence_read() {
    let stack = setup_listing(vec![], true).await;
    let signal = AbortSignal::default();
    signal.abort();
    let error = stack
        .subagents
        .list_children(stack.parent.agent.id(), Some(signal))
        .await
        .unwrap_err();
    assert_eq!(error_code(&error).as_deref(), Some("CANCELLED"));
}

#[tokio::test]
async fn returns_an_empty_array_for_a_parent_with_no_children() {
    let stack = setup_listing(vec![], true).await;
    stack
        .dependencies
        .sessions
        .flush(stack.parent.agent.session())
        .await
        .unwrap();
    assert!(list(&stack, stack.parent.agent.id()).await.is_empty());
}

#[tokio::test]
async fn subagent_error_from_list_children_is_typed_with_its_stable_code() {
    let stack = setup_listing(vec![], false).await;
    let error = stack
        .subagents
        .list_children(stack.parent.agent.id(), None)
        .await
        .unwrap_err();
    let typed = error
        .downcast_ref::<seekdeep_subagent::SubagentError>()
        .expect("typed SubagentError");
    assert_eq!(typed.code, "SUBAGENT_CONTROL_PROJECTIONS_UNAVAILABLE");
}

#[tokio::test]
async fn flattens_the_complete_tree_in_stable_pre_order_with_verified_parent_and_depth() {
    let stack = setup_listing(vec![], true).await;
    let parent_id = stack.parent.agent.id().clone();
    let mut header = under(&parent_id);
    header.created_at = Some(1);
    let child_a = author_child(
        &stack,
        "00000000-0000-4000-8000-00000000aaa1",
        header,
        child_events(descriptor_payload("branch a")),
    )
    .await;
    let mut header = under(&child_a);
    header.created_at = Some(2);
    let grandchild = author_child(
        &stack,
        "00000000-0000-4000-8000-00000000aaa2",
        header,
        child_events(descriptor_payload("under a")),
    )
    .await;
    let mut header = under(&parent_id);
    header.created_at = Some(3);
    let child_b = author_child(
        &stack,
        "00000000-0000-4000-8000-00000000aaa3",
        header,
        child_events(descriptor_payload("branch b")),
    )
    .await;

    assert_eq!(
        list_descendants(&stack, &parent_id).await,
        [
            descendant(
                continuable(&child_a, "branch a", SubagentActivity::Inactive, true),
                &parent_id,
                1
            ),
            descendant(
                continuable(&grandchild, "under a", SubagentActivity::Inactive, false),
                &child_a,
                2
            ),
            descendant(
                continuable(&child_b, "branch b", SubagentActivity::Inactive, false),
                &parent_id,
                1
            ),
        ]
    );
}

#[tokio::test]
async fn returns_an_empty_result_when_the_root_has_no_descendants() {
    let stack = setup_listing(vec![], true).await;
    stack
        .dependencies
        .sessions
        .flush(stack.parent.agent.session())
        .await
        .unwrap();
    assert!(
        list_descendants(&stack, stack.parent.agent.id())
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn omits_a_live_creation_window_candidate_while_continuing_through_its_subtree() {
    let stack = setup_listing(vec![], true).await;
    let bare_id = SessionId::new("live-creation-window");
    let bare = stack
        .dependencies
        .sessions
        .create(
            &stack.context,
            Some(bare_id.clone()),
            CreateSessionOptions {
                created_at: Some(1),
                parent_session: Some(stack.parent.agent.id().clone()),
                origin: Some(SessionOrigin::Subagent),
                ..CreateSessionOptions::default()
            },
        )
        .unwrap();
    bare.append("turn/start", json!({ "turn": 1 }), AppendOptions::default())
        .unwrap();
    let mut header = under(&bare_id);
    header.created_at = Some(2);
    let below = author_child(
        &stack,
        "00000000-0000-4000-8000-00000000aaaf",
        header,
        child_events(descriptor_payload("below the creation window")),
    )
    .await;
    assert_eq!(
        list_descendants(&stack, stack.parent.agent.id()).await,
        [descendant(
            continuable(
                &below,
                "below the creation window",
                SubagentActivity::Inactive,
                false
            ),
            &bare_id,
            2
        )]
    );
}

#[tokio::test]
async fn contains_a_corrupt_parent_cycle_without_revisiting_the_requested_root() {
    let stack = setup_listing(vec![], true).await;
    let root_id = SessionId::new("cycle-root");
    let node_id = SessionId::new("cycle-node");
    author_child(
        &stack,
        "cycle-root",
        HeaderOverrides {
            parent_session: Some(node_id.clone()),
            origin: None,
            created_at: Some(2),
            seed_length: None,
        },
        child_events(descriptor_payload("ordinary cycle root")),
    )
    .await;
    let mut header = under(&root_id);
    header.created_at = Some(1);
    author_child(
        &stack,
        "cycle-node",
        header,
        child_events(descriptor_payload("cycle child")),
    )
    .await;
    assert_eq!(
        list_descendants(&stack, &root_id).await,
        [descendant(
            continuable(&node_id, "cycle child", SubagentActivity::Inactive, false),
            &root_id,
            1
        )]
    );
}

#[tokio::test]
async fn walks_a_deeply_nested_ordinary_session_chain_without_consuming_the_call_stack() {
    let stack = setup_listing(vec![], true).await;
    let depth = 10_000_u64;
    let mut parent_id = stack.parent.agent.id().clone();
    for level in 1..depth {
        let session = stack
            .dependencies
            .sessions
            .create(
                &stack.context,
                Some(SessionId::new(format!("deep-ordinary-{level}"))),
                CreateSessionOptions {
                    created_at: Some(level),
                    parent_session: Some(parent_id.clone()),
                    ..CreateSessionOptions::default()
                },
            )
            .unwrap();
        parent_id = session.header().id.clone();
    }
    let leaf_id = SessionId::new("deep-subagent-leaf");
    let leaf = stack
        .dependencies
        .sessions
        .create(
            &stack.context,
            Some(leaf_id.clone()),
            CreateSessionOptions {
                created_at: Some(depth),
                parent_session: Some(parent_id.clone()),
                origin: Some(SessionOrigin::Subagent),
                ..CreateSessionOptions::default()
            },
        )
        .unwrap();
    leaf.append("turn/start", json!({ "turn": 1 }), AppendOptions::default())
        .unwrap();
    leaf.append(
        "subagent/descriptor",
        descriptor_payload("deep leaf"),
        AppendOptions::default(),
    )
    .unwrap();
    assert_eq!(
        list_descendants(&stack, stack.parent.agent.id()).await,
        [descendant(
            continuable(&leaf_id, "deep leaf", SubagentActivity::Running, false),
            &parent_id,
            depth
        )]
    );
}

#[tokio::test]
async fn discovers_continuable_descendants_below_ordinary_and_one_shot_intermediates() {
    let stack = setup_listing(vec![Entry::chunks(text_response("one shot"))], true).await;
    let fork = stack
        .dependencies
        .sessions
        .fork(
            &stack.context,
            stack.parent.agent.session(),
            None,
            Some(SessionId::new("plain-fork")),
        )
        .unwrap();
    stack.dependencies.sessions.flush(&fork).await.unwrap();
    let fork_id = fork.header().id.clone();
    let mut header = under(&fork_id);
    header.created_at = Some(2);
    let under_fork = author_child(
        &stack,
        "00000000-0000-4000-8000-00000000bbb1",
        header,
        child_events(descriptor_payload("under the fork")),
    )
    .await;
    let run = stack
        .subagents
        .start(
            "spawn",
            SubagentStartRequest {
                label: Some("one-shot intermediate".to_owned()),
                prompt: message("one-shot task"),
                parent: Arc::clone(&stack.parent.agent),
                signal: AbortSignal::default(),
                agent_options: None,
                output_schema: None,
                max_depth: None,
                tool_filter: None,
                persona: None,
            },
        )
        .await
        .unwrap();
    run.result().await.unwrap();
    stack
        .dependencies
        .sessions
        .flush(run.local_agent().unwrap().session())
        .await
        .unwrap();
    let one_shot_id = run.id().clone();
    run.dispose().await.unwrap();
    let mut header = under(&one_shot_id);
    header.created_at = Some(9_999_999_999_999);
    let under_one_shot = author_child(
        &stack,
        "00000000-0000-4000-8000-00000000bbb2",
        header,
        child_events(descriptor_payload("under the one-shot")),
    )
    .await;

    let entries = list_descendants(&stack, stack.parent.agent.id()).await;
    let ids = entries
        .iter()
        .map(|entry| match &entry.entry {
            SubagentListEntry::Child { id, .. } | SubagentListEntry::Diagnostic { id, .. } => {
                id.clone()
            }
        })
        .collect::<Vec<_>>();
    assert!(!ids.contains(&fork_id));
    assert!(entries.contains(&descendant(
        continuable(
            &under_fork,
            "under the fork",
            SubagentActivity::Inactive,
            false
        ),
        &fork_id,
        2
    )));
    assert!(entries.iter().any(|entry| matches!(
        &entry.entry,
        SubagentListEntry::Child { id, mode: SubagentListMode::OneShot { .. }, .. }
            if id == &one_shot_id && entry.parent_id == *stack.parent.agent.id() && entry.depth == 1
    )));
    assert!(entries.contains(&descendant(
        continuable(
            &under_one_shot,
            "under the one-shot",
            SubagentActivity::Inactive,
            false
        ),
        &one_shot_id,
        2
    )));
    let position = |target: &SessionId| ids.iter().position(|id| id == target).unwrap();
    assert!(position(&under_one_shot) > position(&one_shot_id));
}

#[tokio::test]
async fn diagnoses_a_settled_descriptor_less_node_while_walking_its_subtree() {
    let stack = setup_listing(vec![], true).await;
    let mut header = under(stack.parent.agent.id());
    header.created_at = Some(1);
    let bare = author_child(
        &stack,
        "00000000-0000-4000-8000-00000000eee1",
        header,
        vec![turn_start(0, 1, 1), turn_end(1, 2, 1)],
    )
    .await;
    let mut header = under(&bare);
    header.created_at = Some(2);
    let below = author_child(
        &stack,
        "00000000-0000-4000-8000-00000000eee2",
        header,
        child_events(descriptor_payload("below the bare node")),
    )
    .await;
    let parent_id = stack.parent.agent.id().clone();
    assert_eq!(
        list_descendants(&stack, &parent_id).await,
        [
            descendant(
                diagnostic(&bare, SubagentDiagnosticReason::Corrupt),
                &parent_id,
                1
            ),
            descendant(
                continuable(
                    &below,
                    "below the bare node",
                    SubagentActivity::Inactive,
                    false
                ),
                &bare,
                2
            ),
        ]
    );
}

#[tokio::test]
async fn keeps_traversing_below_a_corrupt_intermediate_and_positions_its_diagnostic() {
    let stack = setup_listing(vec![], true).await;
    let mut header = under(stack.parent.agent.id());
    header.created_at = Some(1);
    let corrupt = author_child(
        &stack,
        "00000000-0000-4000-8000-00000000ccc1",
        header,
        child_events(descriptor_payload_version("unsupported descriptor", 999)),
    )
    .await;
    let mut header = under(&corrupt);
    header.created_at = Some(2);
    let below = author_child(
        &stack,
        "00000000-0000-4000-8000-00000000ccc2",
        header,
        child_events(descriptor_payload("below the corrupt node")),
    )
    .await;
    let parent_id = stack.parent.agent.id().clone();
    assert_eq!(
        list_descendants(&stack, &parent_id).await,
        [
            descendant(
                diagnostic(&corrupt, SubagentDiagnosticReason::Corrupt),
                &parent_id,
                1
            ),
            descendant(
                continuable(
                    &below,
                    "below the corrupt node",
                    SubagentActivity::Inactive,
                    false
                ),
                &corrupt,
                2
            ),
        ]
    );
}

#[tokio::test]
async fn a_pre_aborted_signal_stops_the_descendant_scan_before_persistence_reads() {
    let stack = setup_listing(vec![Entry::chunks(text_response("done"))], true).await;
    start_child(&stack, "never read").await;
    let signal = AbortSignal::default();
    signal.abort();
    let error = stack
        .subagents
        .list_descendants(stack.parent.agent.id(), Some(signal))
        .await
        .unwrap_err();
    assert_eq!(error_code(&error).as_deref(), Some("CANCELLED"));
}

#[tokio::test]
async fn list_descendants_fails_loud_when_the_projection_registry_is_not_mounted() {
    let stack = setup_listing(vec![], false).await;
    let error = stack
        .subagents
        .list_descendants(stack.parent.agent.id(), None)
        .await
        .unwrap_err();
    assert_eq!(
        error_code(&error).as_deref(),
        Some("SUBAGENT_CONTROL_PROJECTIONS_UNAVAILABLE")
    );
}
