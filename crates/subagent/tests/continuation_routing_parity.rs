//! Followup residency routing, continuable child ownership, durability and
//! teardown, and the review regressions of the pinned `continuation.spec.ts`,
//! against the assembled Rust stack.

#![expect(
    clippy::too_many_lines,
    reason = "spec cases ported statement for statement"
)]

mod support;

use std::{sync::Arc, time::Duration};

use seekdeep_agent::AgentStatus;
use seekdeep_cordis::{EventOptions, EventReply};
use seekdeep_core::session::SessionId;
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_llm::{AbortSignal, FinishReason, StreamChunk};
use seekdeep_subagent::{
    SubagentRunEndInfo, SubagentRunInfo, SubagentStartRequest, SubagentStopReason,
};
use serde_json::json;
use support::{continuation::*, providers::ScriptedProvider};

fn record_runs(
    stack: &Stack,
) -> (
    Shared<Vec<SubagentRunInfo>>,
    Shared<Vec<SubagentRunEndInfo>>,
) {
    let starts: Shared<Vec<SubagentRunInfo>> = Arc::default();
    let ends: Shared<Vec<SubagentRunEndInfo>> = Arc::default();
    let observed = Arc::clone(&starts);
    stack
        .context
        .events()
        .on_sync(
            &stack.context,
            "subagent/start",
            move |_, args| {
                if let Some(info) = args.get::<SubagentRunInfo>(0) {
                    observed.lock().unwrap().push((*info).clone());
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    let observed = Arc::clone(&ends);
    stack
        .context
        .events()
        .on_sync(
            &stack.context,
            "subagent/end",
            move |_, args| {
                if let Some(info) = args.get::<SubagentRunEndInfo>(0) {
                    observed.lock().unwrap().push((*info).clone());
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    (starts, ends)
}

fn record_disposals(stack: &Stack) -> Shared<Vec<SessionId>> {
    let disposals: Shared<Vec<SessionId>> = Arc::default();
    let observed = Arc::clone(&disposals);
    stack
        .context
        .events()
        .on_sync(
            &stack.context,
            "agent/disposed",
            move |_, args| {
                if let Some(payload) = args.get::<seekdeep_agent::AgentLifecycleEvent>(0) {
                    observed.lock().unwrap().push(payload.agent.id().clone());
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    disposals
}

async fn wait_requests(stack: &Stack, count: usize) {
    wait_for(Duration::from_secs(5), || {
        (stack.adapter.request_count() >= count).then_some(())
    })
    .await;
}

#[tokio::test]
async fn enqueues_in_the_same_activation_while_it_is_running_preserving_one_inbox_fifo() {
    let (first, release_first) = Entry::gated(text_response("first"));
    let stack = setup(vec![
        first,
        Entry::chunks(text_response("second")),
        Entry::chunks(text_response("third")),
    ])
    .await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_requests(&stack, 1).await;
    let child = stack.dependencies.agents.get(&started.child_id).unwrap();
    assert_eq!(child.status(), AgentStatus::Running);

    let first_message = followup(
        &stack,
        &stack.parent.agent,
        &started.child_id,
        message("first follow-up"),
    )
    .await
    .unwrap();
    let second_message = followup(
        &stack,
        &stack.parent.agent,
        &started.child_id,
        message("second follow-up"),
    )
    .await
    .unwrap();
    assert_ne!(first_message, second_message);
    assert!(Arc::ptr_eq(
        &stack.dependencies.agents.get(&started.child_id).unwrap(),
        &child
    ));

    release_first.open_sender();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    let loaded = load(&stack.context, &started.child_id).await;
    assert_eq!(
        user_texts(&loaded.events),
        ["child task", "first follow-up", "second follow-up"]
    );
}

#[tokio::test]
async fn cold_resumes_a_settled_child_into_a_new_activation() {
    let stack = setup(vec![
        Entry::chunks(text_response("first")),
        Entry::chunks(text_response("after resume")),
    ])
    .await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;

    followup(
        &stack,
        &stack.parent.agent,
        &started.child_id,
        message("continue please"),
    )
    .await
    .unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;

    let loaded = load(&stack.context, &started.child_id).await;
    assert_eq!(
        user_texts(&loaded.events),
        ["child task", "continue please"]
    );
    assert_eq!(events_of(&loaded.events, "subagent/descriptor").len(), 1);
}

#[tokio::test]
async fn cold_resumes_after_the_initial_provider_unregisters() {
    let stack = setup(vec![
        Entry::chunks(text_response("first")),
        Entry::chunks(text_response("after resume")),
    ])
    .await;
    let invariants = InvariantRegistry::install(
        &stack.context,
        &InvariantConfig {
            enabled: true,
            package_allowlist: Vec::new(),
            package_blocklist: Vec::new(),
        },
    )
    .unwrap();
    let _invariant = seekdeep_subagent::invariant::register_invariant(&invariants).unwrap();
    stack.context.registry().await_quiescent().await;
    let retired: Arc<dyn seekdeep_subagent::SubagentProvider> =
        ScriptedProvider::continuable("retired");
    let registration = stack.subagents.register_provider(retired).unwrap();
    let (starts, ends) = record_runs(&stack);

    let started = stack
        .subagents
        .start_continuable(start_spec(
            &stack.parent.agent,
            "retired",
            AbortSignal::default(),
        ))
        .await
        .unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    registration.dispose().await.unwrap();
    assert!(stack.subagents.get_provider("retired").is_none());

    followup(
        &stack,
        &stack.parent.agent,
        &started.child_id,
        message("continue without provider"),
    )
    .await
    .unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    wait_for(Duration::from_secs(5), || {
        (ends.lock().unwrap().len() == 2).then_some(())
    })
    .await;

    let starts = starts.lock().unwrap().clone();
    let ends = ends.lock().unwrap().clone();
    assert_eq!(
        starts
            .iter()
            .map(|info| info.provider.as_str())
            .collect::<Vec<_>>(),
        ["retired", "retired"]
    );
    assert_eq!(
        ends.iter()
            .map(|info| info.run_id.clone())
            .collect::<Vec<_>>(),
        starts
            .iter()
            .map(|info| info.run_id.clone())
            .collect::<Vec<_>>()
    );
    let loaded = load(&stack.context, &started.child_id).await;
    assert_eq!(
        user_texts(&loaded.events),
        ["child task", "continue without provider"]
    );
}

#[tokio::test]
async fn wakes_a_waiting_activation_instead_of_cold_resuming_it() {
    let (grandchild_entry, release_grandchild) = Entry::gated(text_response("grandchild"));
    let stack = setup(vec![
        Entry::chunks(text_response("child done")),
        grandchild_entry,
        Entry::chunks(text_response("woken")),
    ])
    .await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    let child = wait_activation(&stack.dependencies.agents, &started.child_id).await;
    let grandchild = stack
        .subagents
        .start_continuable(spawn_spec(&child))
        .await
        .unwrap();
    wait_requests(&stack, 2).await;
    wait_for(Duration::from_secs(5), || {
        (child.status() == AgentStatus::Idle
            && stack
                .dependencies
                .agents
                .get(&started.child_id)
                .is_some_and(|live| Arc::ptr_eq(&live, &child)))
        .then_some(())
    })
    .await;

    followup(
        &stack,
        &stack.parent.agent,
        &started.child_id,
        message("while waiting"),
    )
    .await
    .unwrap();
    assert!(Arc::ptr_eq(
        &stack.dependencies.agents.get(&started.child_id).unwrap(),
        &child
    ));

    release_grandchild.open_sender();
    wait_no_activation(&stack.dependencies.agents, &grandchild.child_id).await;
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    let loaded = load(&stack.context, &started.child_id).await;
    let texts = user_texts(&loaded.events);
    assert_eq!(&texts[..2], ["child task", "while waiting"]);
    assert!(
        texts[2..]
            .join("\n")
            .contains("finished and will do no further work")
    );
}

#[tokio::test]
async fn rejects_a_parent_that_is_not_the_durable_direct_parent() {
    let stack = setup(vec![Entry::chunks(text_response("first"))]).await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    let stranger = create_agent(&stack.dependencies.agents, "stranger", true).await;
    let error = followup(
        &stack,
        &stranger.agent,
        &started.child_id,
        message("mine now"),
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("belongs to another parent session"),
        "{error}"
    );
}

#[tokio::test]
async fn reports_an_unresumable_child_whose_persisted_log_has_no_supported_descriptor() {
    let stack = setup(vec![Entry::chunks(text_response("one shot"))]).await;
    let run = stack
        .subagents
        .start(
            "spawn",
            SubagentStartRequest {
                label: Some("one-shot work".to_owned()),
                prompt: message("one-shot work"),
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

    let error = followup(
        &stack,
        &stack.parent.agent,
        &one_shot_id,
        message("continue"),
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("no supported continuation state"),
        "{error}"
    );
}

#[tokio::test]
async fn reports_an_unknown_child_id_as_unavailable() {
    let stack = setup(vec![]).await;
    let error = followup(
        &stack,
        &stack.parent.agent,
        &SessionId::new("missing"),
        message("hello"),
    )
    .await
    .unwrap_err();
    assert_eq!(error_code(&error).as_deref(), Some("NOT_RESUMABLE"));
}

#[tokio::test]
async fn cold_resumes_a_delivery_that_lost_the_race_with_final_disposal() {
    let stack = setup(vec![
        Entry::chunks(text_response("first")),
        Entry::chunks(text_response("after the race")),
    ])
    .await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    let child = wait_activation(&stack.dependencies.agents, &started.child_id).await;
    let idle = child.when_idle().unwrap();
    idle.await.unwrap();
    followup(
        &stack,
        &stack.parent.agent,
        &started.child_id,
        message("raced"),
    )
    .await
    .unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    let loaded = load(&stack.context, &started.child_id).await;
    assert!(has_user_text(&loaded.events, "raced"));
}

#[tokio::test]
async fn keeps_a_parent_activation_waiting_until_its_child_completes_disposal() {
    let (grandchild_entry, release_grandchild) = Entry::gated(text_response("grandchild"));
    let stack = setup(vec![
        Entry::chunks(text_response("child done")),
        grandchild_entry,
    ])
    .await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    let child = wait_activation(&stack.dependencies.agents, &started.child_id).await;
    let grandchild = stack
        .subagents
        .start_continuable(spawn_spec(&child))
        .await
        .unwrap();
    wait_for(Duration::from_secs(5), || {
        (child.status() == AgentStatus::Idle
            && stack
                .dependencies
                .agents
                .get(&started.child_id)
                .is_some_and(|live| Arc::ptr_eq(&live, &child)))
        .then_some(())
    })
    .await;
    assert!(Arc::ptr_eq(
        &stack.dependencies.agents.get(&started.child_id).unwrap(),
        &child
    ));
    assert!(
        stack
            .dependencies
            .agents
            .get(&grandchild.child_id)
            .is_some()
    );

    release_grandchild.open_sender();
    wait_no_activation(&stack.dependencies.agents, &grandchild.child_id).await;
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
}

#[tokio::test]
async fn does_not_add_a_top_level_parent_to_the_waiting_graph() {
    let stack = setup(vec![Entry::chunks(text_response("done"))]).await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    let live = stack
        .dependencies
        .agents
        .get(stack.parent.agent.id())
        .unwrap();
    assert!(Arc::ptr_eq(&live, &stack.parent.agent));
}

#[tokio::test]
async fn disposes_every_live_activation_forest_child_first_on_manager_teardown() {
    let (grandchild_entry, hold) = Entry::gated(text_response("grandchild"));
    let stack = setup(vec![
        Entry::chunks(text_response("child done")),
        grandchild_entry,
    ])
    .await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    let child = wait_activation(&stack.dependencies.agents, &started.child_id).await;
    let grandchild = stack
        .subagents
        .start_continuable(spawn_spec(&child))
        .await
        .unwrap();
    wait_activation(&stack.dependencies.agents, &grandchild.child_id).await;

    let disposals = record_disposals(&stack);
    let subagents = Arc::clone(&stack.subagents);
    let drained = tokio::spawn(async move { drain_manager(&subagents).await });
    hold.open_sender();
    drained.await.unwrap().unwrap();

    let disposals = disposals.lock().unwrap().clone();
    let grandchild_index = disposals
        .iter()
        .position(|id| *id == grandchild.child_id)
        .unwrap();
    let child_index = disposals
        .iter()
        .position(|id| *id == started.child_id)
        .unwrap();
    assert!(grandchild_index < child_index);
    let loaded = load(&stack.context, &started.child_id).await;
    assert_eq!(loaded.meta.id, started.child_id);
}

#[tokio::test]
async fn drains_one_parent_forest_without_disabling_a_sibling_parent_forest() {
    let (target_entry, release_target) = Entry::gated(text_response("target child"));
    let (sibling_entry, release_sibling) = Entry::gated(text_response("sibling child"));
    let (grandchild_entry, release_grandchild) = Entry::gated(text_response("target grandchild"));
    let stack = setup(vec![
        target_entry,
        sibling_entry,
        grandchild_entry,
        Entry::chunks(text_response("sibling follow-up")),
    ])
    .await;
    let sibling_parent = create_agent(&stack.dependencies.agents, "sibling-parent", true).await;
    let target = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    let sibling = stack
        .subagents
        .start_continuable(spawn_spec(&sibling_parent.agent))
        .await
        .unwrap();
    wait_requests(&stack, 2).await;
    let target_child = stack.dependencies.agents.get(&target.child_id).unwrap();
    let sibling_child = stack.dependencies.agents.get(&sibling.child_id).unwrap();
    let grandchild = stack
        .subagents
        .start_continuable(spawn_spec(&target_child))
        .await
        .unwrap();
    wait_requests(&stack, 3).await;

    let subagents = Arc::clone(&stack.subagents);
    let parent = Arc::clone(&stack.parent.agent);
    let drained = tokio::spawn(async move {
        subagents
            .drain_continuable_descendants(std::slice::from_ref(&parent))
            .await
    });
    let subagents = Arc::clone(&stack.subagents);
    let parent = Arc::clone(&stack.parent.agent);
    let converged = tokio::spawn(async move {
        subagents
            .drain_continuable_descendants(std::slice::from_ref(&parent))
            .await
    });
    // The scoped cutoff cancels only the selected forest (the target child's
    // and grandchild's model calls), in tree order.
    wait_for(Duration::from_secs(5), || {
        (stack.adapter.cancelled().len() == 2).then_some(())
    })
    .await;
    assert_eq!(stack.adapter.cancelled(), [0, 2]);
    assert!(Arc::ptr_eq(
        &stack.dependencies.agents.get(&target.child_id).unwrap(),
        &target_child
    ));
    assert!(
        stack
            .dependencies
            .agents
            .get(&grandchild.child_id)
            .is_some()
    );
    assert!(Arc::ptr_eq(
        &stack.dependencies.agents.get(&sibling.child_id).unwrap(),
        &sibling_child
    ));
    followup(
        &stack,
        &sibling_parent.agent,
        &sibling.child_id,
        message("still live"),
    )
    .await
    .unwrap();
    let error = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap_err();
    assert_eq!(error_code(&error).as_deref(), Some("DRAINING"));
    let error = followup(
        &stack,
        &stack.parent.agent,
        &target.child_id,
        message("too late"),
    )
    .await
    .unwrap_err();
    assert_eq!(error_code(&error).as_deref(), Some("DRAINING"));

    release_target.open_sender();
    release_grandchild.open_sender();
    drained.await.unwrap().unwrap();
    converged.await.unwrap().unwrap();
    assert!(stack.dependencies.agents.get(&target.child_id).is_none());
    assert!(
        stack
            .dependencies
            .agents
            .get(&grandchild.child_id)
            .is_none()
    );
    assert!(Arc::ptr_eq(
        &stack.dependencies.agents.get(&sibling.child_id).unwrap(),
        &sibling_child
    ));
    let error = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap_err();
    assert_eq!(error_code(&error).as_deref(), Some("DRAINING"));

    release_sibling.open_sender();
    wait_no_activation(&stack.dependencies.agents, &sibling.child_id).await;
}

#[tokio::test]
async fn retains_a_continuable_root_while_draining_only_its_descendants() {
    let (child_entry, release_child) = Entry::gated(text_response("child"));
    let (grandchild_entry, release_grandchild) = Entry::gated(text_response("grandchild"));
    let stack = setup(vec![child_entry, grandchild_entry]).await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_requests(&stack, 1).await;
    let child = stack.dependencies.agents.get(&started.child_id).unwrap();
    let grandchild = stack
        .subagents
        .start_continuable(spawn_spec(&child))
        .await
        .unwrap();
    wait_requests(&stack, 2).await;

    let subagents = Arc::clone(&stack.subagents);
    let root = Arc::clone(&child);
    let drained = tokio::spawn(async move {
        subagents
            .drain_continuable_descendants(std::slice::from_ref(&root))
            .await
    });
    wait_for(Duration::from_secs(5), || {
        (stack.adapter.cancelled().len() == 1).then_some(())
    })
    .await;
    assert_eq!(stack.adapter.cancelled(), [1]);
    assert!(Arc::ptr_eq(
        &stack.dependencies.agents.get(&started.child_id).unwrap(),
        &child
    ));
    release_grandchild.open_sender();
    drained.await.unwrap().unwrap();
    assert!(
        stack
            .dependencies
            .agents
            .get(&grandchild.child_id)
            .is_none()
    );
    assert!(Arc::ptr_eq(
        &stack.dependencies.agents.get(&started.child_id).unwrap(),
        &child
    ));
    let error = stack
        .subagents
        .start_continuable(spawn_spec(&child))
        .await
        .unwrap_err();
    assert_eq!(error_code(&error).as_deref(), Some("DRAINING"));

    release_child.open_sender();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
}

#[tokio::test]
async fn finds_scoped_descendants_after_an_intermediate_one_shot_agent_leaves_the_registry() {
    let (intermediate_entry, release_intermediate) = Entry::gated(text_response("one-shot"));
    let (descendant_entry, release_descendant) =
        Entry::gated(text_response("continuable descendant"));
    let stack = setup(vec![intermediate_entry, descendant_entry]).await;
    let run = stack
        .subagents
        .start(
            "spawn",
            SubagentStartRequest {
                label: Some("one-shot task".to_owned()),
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
    let intermediate = Arc::clone(run.local_agent().expect("spawn must publish a local Agent"));
    let descendant = stack
        .subagents
        .start_continuable(spawn_spec(&intermediate))
        .await
        .unwrap();
    wait_requests(&stack, 2).await;

    let intermediate_id = intermediate.id().clone();
    let disposing = run.dispose();
    release_intermediate.open_sender();
    disposing.await.unwrap();
    assert!(stack.dependencies.agents.get(&intermediate_id).is_none());
    assert!(
        stack
            .dependencies
            .agents
            .get(&descendant.child_id)
            .is_some()
    );

    let subagents = Arc::clone(&stack.subagents);
    let parent = Arc::clone(&stack.parent.agent);
    let drained = tokio::spawn(async move {
        subagents
            .drain_continuable_descendants(std::slice::from_ref(&parent))
            .await
    });
    wait_for(Duration::from_secs(5), || {
        (stack.adapter.cancelled().len() >= 2).then_some(())
    })
    .await;
    assert!(stack.adapter.cancelled().contains(&1));
    release_descendant.open_sender();
    drained.await.unwrap().unwrap();
    assert!(
        stack
            .dependencies
            .agents
            .get(&descendant.child_id)
            .is_none()
    );
}

#[tokio::test]
async fn rejects_new_materialization_and_delivery_once_draining_begins() {
    let stack = setup(vec![Entry::chunks(text_response("done"))]).await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;

    drain_manager(&stack.subagents).await.unwrap();

    let error = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap_err();
    assert_eq!(error_code(&error).as_deref(), Some("DRAINING"));
    let error = followup(
        &stack,
        &stack.parent.agent,
        &started.child_id,
        message("too late"),
    )
    .await
    .unwrap_err();
    assert_eq!(error_code(&error).as_deref(), Some("DRAINING"));
}

#[tokio::test]
async fn rejects_an_initial_prompt_when_drain_starts_after_materialization() {
    let stack = setup(vec![]).await;
    let drains: Shared<Vec<tokio::task::JoinHandle<anyhow::Result<()>>>> = Arc::default();
    let accepted: Shared<Vec<String>> = Arc::default();
    let subagents = Arc::clone(&stack.subagents);
    let observed_drains = Arc::clone(&drains);
    stack
        .context
        .events()
        .on_sync(
            &stack.context,
            "subagent/start",
            move |_, _| {
                let subagents = Arc::clone(&subagents);
                observed_drains
                    .lock()
                    .unwrap()
                    .push(tokio::spawn(drain_manager(&subagents)));
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    let observed = Arc::clone(&accepted);
    stack
        .context
        .events()
        .on_sync(
            &stack.context,
            "agent/inbox/inserted",
            move |_, args| {
                if let Some(payload) = args
                    .get::<seekdeep_agent::AgentEvent<seekdeep_agent_loop::AgentInboxMessage>>(0)
                {
                    observed
                        .lock()
                        .unwrap()
                        .push(payload.payload.message.id().to_string());
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();

    let error = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap_err();
    assert_eq!(error_code(&error).as_deref(), Some("DRAINING"), "{error}");
    let drains = std::mem::take(&mut *drains.lock().unwrap());
    for drain in drains {
        drain.await.unwrap().unwrap();
    }
    assert!(accepted.lock().unwrap().is_empty());
    assert_eq!(agent_ids(&stack.dependencies.agents), ["parent"]);
}

#[tokio::test]
async fn waits_for_a_published_materialization_to_finish_rollback_before_drain_resolves() {
    let stack = setup(vec![]).await;
    let order: Shared<Vec<&'static str>> = Arc::default();
    let drains: Shared<Vec<tokio::task::JoinHandle<()>>> = Arc::default();
    let parent_id = stack.parent.agent.id().clone();
    let subagents = Arc::clone(&stack.subagents);
    let observed_order = Arc::clone(&order);
    let observed_drains = Arc::clone(&drains);
    stack
        .context
        .events()
        .on_sync(
            &stack.context,
            "agent/created",
            move |_, args| {
                if let Some(payload) = args.get::<seekdeep_agent::AgentLifecycleEvent>(0)
                    && *payload.agent.id() != parent_id
                {
                    let subagents = Arc::clone(&subagents);
                    let order = Arc::clone(&observed_order);
                    // The source drain sets its cutoff synchronously before its
                    // first await; the boxed continuation carries the rest.
                    let drain = drain_manager(&subagents);
                    observed_drains
                        .lock()
                        .unwrap()
                        .push(tokio::spawn(async move {
                            drain.await.unwrap();
                            order.lock().unwrap().push("drain");
                        }));
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    let parent_id = stack.parent.agent.id().clone();
    let observed_order = Arc::clone(&order);
    stack
        .context
        .events()
        .on_sync(
            &stack.context,
            "agent/disposed",
            move |_, args| {
                if let Some(payload) = args.get::<seekdeep_agent::AgentLifecycleEvent>(0)
                    && *payload.agent.id() != parent_id
                {
                    observed_order.lock().unwrap().push("disposed");
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();

    let error = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap_err();
    assert_eq!(error_code(&error).as_deref(), Some("DRAINING"), "{error}");
    let drains = std::mem::take(&mut *drains.lock().unwrap());
    for drain in drains {
        drain.await.unwrap();
    }
    assert_eq!(*order.lock().unwrap(), ["disposed", "drain"]);
    assert_eq!(agent_ids(&stack.dependencies.agents), ["parent"]);
}

#[tokio::test]
async fn admits_a_live_follow_up_before_a_later_drain_can_begin_disposal() {
    let (entry, hold) = Entry::gated(text_response("working"));
    let stack = setup(vec![entry]).await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_requests(&stack, 1).await;
    let order: Shared<Vec<&'static str>> = Arc::default();
    let observed = Arc::clone(&order);
    stack
        .context
        .events()
        .on_sync(
            &stack.context,
            "agent/inbox/inserted",
            move |_, args| {
                if let Some(payload) =
                    args.get::<seekdeep_agent::AgentEvent<seekdeep_agent_loop::AgentInboxMessage>>(0)
                    && payload.payload.message.content().iter().any(|block| {
                        matches!(block, seekdeep_llm::ContentBlock::Text { text } if text == "before drain")
                    })
                {
                    observed.lock().unwrap().push("enqueue");
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();

    let delivery = followup(
        &stack,
        &stack.parent.agent,
        &started.child_id,
        message("before drain"),
    );
    let subagents = Arc::clone(&stack.subagents);
    let drained = tokio::spawn(async move { drain_manager(&subagents).await });
    hold.open_sender();

    delivery.await.unwrap();
    drained.await.unwrap().unwrap();
    wait_for(Duration::from_secs(5), || {
        (stack.adapter.cancelled().len() == 1).then_some(())
    })
    .await;
    assert_eq!(*order.lock().unwrap(), ["enqueue"]);
}

#[tokio::test]
async fn has_no_automatic_replay_for_an_accepted_but_unlogged_message() {
    let (entry, hold) = Entry::gated(text_response("first"));
    let stack = setup(vec![entry]).await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_requests(&stack, 1).await;
    followup(
        &stack,
        &stack.parent.agent,
        &started.child_id,
        message("never logged"),
    )
    .await
    .unwrap();

    let subagents = Arc::clone(&stack.subagents);
    let drained = tokio::spawn(async move { drain_manager(&subagents).await });
    hold.open_sender();
    drained.await.unwrap().unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;

    let loaded = load(&stack.context, &started.child_id).await;
    assert!(!has_user_text(&loaded.events, "never logged"));
}

#[tokio::test]
async fn reports_the_childs_own_terminal_reason_not_teardown_success() {
    let stack = setup(vec![Entry::chunks(vec![
        StreamChunk::BlockStart {
            index: 0,
            block_type: "text".to_owned(),
        },
        StreamChunk::TextDelta {
            index: 0,
            text: "partial".to_owned(),
        },
        StreamChunk::BlockEnd {
            index: 0,
            block: seekdeep_llm::ContentBlock::Text {
                text: "partial".into(),
            },
        },
        StreamChunk::Finish {
            reason: FinishReason::MaxTokens,
            replay_state: None,
        },
    ])])
    .await;
    let (_starts, ends) = record_runs(&stack);
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    wait_for(Duration::from_secs(5), || {
        (ends.lock().unwrap().len() == 1).then_some(())
    })
    .await;
    assert_eq!(
        ends.lock().unwrap()[0].stop_reason,
        SubagentStopReason::MaxTokens
    );
}

#[tokio::test]
async fn rejects_a_live_delivery_whose_caller_signal_aborted_before_admission() {
    let (entry, release_first) = Entry::gated(text_response("working"));
    let stack = setup(vec![entry]).await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_requests(&stack, 1).await;
    let child = stack.dependencies.agents.get(&started.child_id).unwrap();
    let before = child.session().events().len();

    let signal = AbortSignal::default();
    signal.abort_with_reason(json!("caller gave up"));
    assert!(
        followup_with(
            &stack.subagents,
            &stack.parent.agent,
            &started.child_id,
            message("cancelled"),
            signal
        )
        .await
        .is_err()
    );

    release_first.open_sender();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    let loaded = load(&stack.context, &started.child_id).await;
    assert!(!has_user_text(&loaded.events, "cancelled"));
    assert!(before > 0);
}

#[tokio::test]
async fn reports_this_epochs_own_output_captured_while_the_child_was_still_live() {
    let stack = setup(vec![
        Entry::chunks(text_response("first answer")),
        Entry::chunks(text_response("second answer")),
    ])
    .await;
    park_parent(&stack.context, &stack.parent.agent);
    let (_starts, ends) = record_runs(&stack);

    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    wait_for(Duration::from_secs(5), || {
        (ends.lock().unwrap().len() == 1).then_some(())
    })
    .await;
    assert_eq!(
        ends.lock().unwrap()[0].last_assistant_message,
        Some(message("first answer"))
    );

    followup(
        &stack,
        &stack.parent.agent,
        &started.child_id,
        message("again"),
    )
    .await
    .unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    wait_for(Duration::from_secs(5), || {
        (ends.lock().unwrap().len() == 2).then_some(())
    })
    .await;
    assert_eq!(
        ends.lock().unwrap()[1].last_assistant_message,
        Some(message("second answer"))
    );
}

#[tokio::test]
async fn keeps_the_epochs_earlier_text_past_a_final_empty_usage_only_message() {
    let stack = setup(vec![
        Entry::chunks(tool_call_response(
            "t1",
            "noop",
            &json!({}),
            Some("partial one"),
        )),
        Entry::chunks(vec![
            StreamChunk::BlockStart {
                index: 0,
                block_type: "tool-call".to_owned(),
            },
            StreamChunk::ToolCallDelta {
                index: 0,
                id: seekdeep_llm::CallId::new("t2"),
                name: Some("noop".to_owned()),
                arguments_delta: "{}".to_owned(),
            },
            StreamChunk::BlockEnd {
                index: 0,
                block: seekdeep_llm::ContentBlock::ToolCall {
                    id: seekdeep_llm::CallId::new("t2"),
                    name: "noop".to_owned(),
                    arguments: "{}".to_owned(),
                },
            },
            StreamChunk::Usage {
                usage: seekdeep_llm::TokenUsage {
                    input_tokens: 20,
                    output_tokens: 5,
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                },
            },
            StreamChunk::Finish {
                reason: FinishReason::MaxTokens,
                replay_state: None,
            },
        ]),
    ])
    .await;
    register_noop_tool(&stack);
    let (_starts, ends) = record_runs(&stack);

    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    wait_for(Duration::from_secs(5), || {
        (ends.lock().unwrap().len() == 1).then_some(())
    })
    .await;
    let end = ends.lock().unwrap()[0].clone();
    assert_eq!(end.stop_reason, SubagentStopReason::MaxTokens);
    assert_eq!(
        end.last_assistant_message,
        Some(vec![
            seekdeep_llm::ContentBlock::Text {
                text: "partial one".into(),
            },
            seekdeep_llm::ContentBlock::ToolCall {
                id: seekdeep_llm::CallId::new("t1"),
                name: "noop".to_owned(),
                arguments: "{}".to_owned(),
            },
        ])
    );
}

#[tokio::test]
async fn reports_a_resumed_epoch_that_opened_no_turn_without_the_previous_answer() {
    let stack = setup(vec![Entry::chunks(text_response("first answer"))]).await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;

    let (_starts, ends) = record_runs(&stack);
    // Block the resumed prompt so this epoch produces nothing of its own.
    let parent = Arc::clone(&stack.parent.agent);
    stack
        .context
        .events()
        .on_waterfall(
            &stack.context,
            "agent/pre-step",
            move |_, args, next| {
                let parent = Arc::clone(&parent);
                Box::pin(async move {
                    let event = args
                        .get::<seekdeep_agent::AgentEvent<seekdeep_agent_loop::AgentPreStepEvent>>(
                            0,
                        )
                        .ok_or_else(|| anyhow::anyhow!("missing pre-step payload"))?;
                    if Arc::ptr_eq(&event.agent, &parent) {
                        next.run().await
                    } else {
                        Ok(EventReply::Value(Arc::new(
                            seekdeep_agent::PreStepDecision::Reject,
                        )))
                    }
                })
            },
            EventOptions::default(),
        )
        .unwrap();
    followup(
        &stack,
        &stack.parent.agent,
        &started.child_id,
        message("again"),
    )
    .await
    .unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    wait_for(Duration::from_secs(5), || {
        (ends.lock().unwrap().len() == 1).then_some(())
    })
    .await;
    let end = ends.lock().unwrap()[0].clone();
    assert_eq!(end.last_assistant_message, None);
    assert_eq!(end.stop_reason, SubagentStopReason::Refusal);
}
