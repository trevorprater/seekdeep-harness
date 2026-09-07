//! The remaining review regressions and the settlement-delivery contract of
//! the pinned `continuation.spec.ts`, against the assembled Rust stack.

mod support;

use std::{sync::Arc, time::Duration};

use seekdeep_agent::{AgentCancelCause, AgentStatus, CancelOptions, PreStepDecision};
use seekdeep_cordis::{EventOptions, EventReply};
use seekdeep_core::session::SessionId;
use seekdeep_llm::{AbortSignal, MessageSource, UserMessage};
use seekdeep_subagent::{
    SubagentInterruptAuthority, SubagentReportDelivery, SubagentReportOptions, SubagentRunEndInfo,
    SubagentStopReason,
};
use support::continuation::*;

fn record_ends(stack: &Stack) -> Shared<Vec<SubagentRunEndInfo>> {
    let ends: Shared<Vec<SubagentRunEndInfo>> = Arc::default();
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
    ends
}

async fn wait_requests(stack: &Stack, count: usize) {
    wait_for(Duration::from_secs(5), || {
        (stack.adapter.request_count() >= count).then_some(())
    })
    .await;
}

async fn wait_notices(agent: &Arc<seekdeep_agent::Agent>, count: usize) -> Vec<Notice> {
    wait_for(Duration::from_secs(5), || {
        let notices = settlement_notices(agent);
        (notices.len() == count).then_some(notices)
    })
    .await
}

#[tokio::test]
async fn cancels_a_running_turn_before_the_best_effort_final_flush() {
    let (entry, hold) = Entry::gated(text_response("slow"));
    let stack = setup(vec![entry]).await;
    let flushes: Shared<Vec<bool>> = Arc::default();
    let observed = Arc::clone(&flushes);
    let adapter = Arc::clone(&stack.adapter);
    stack
        .context
        .events()
        .on_sync(
            &stack.context,
            "session/flush",
            move |_, args| {
                if let Some(session) = args.get::<seekdeep_core::session::Session>(0)
                    && session.header().parent_session.is_some()
                {
                    // Flushing a still-running turn cannot cover the events
                    // cancellation adds, so the child's model call must already
                    // be cancelled when its final flush runs.
                    let cancelled = adapter
                        .requests
                        .lock()
                        .first()
                        .and_then(|request| request.signal.as_ref())
                        .is_some_and(AbortSignal::is_aborted);
                    observed.lock().unwrap().push(cancelled);
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();

    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_activation(&stack.dependencies.agents, &started.child_id).await;
    wait_requests(&stack, 1).await;

    let drained = drain_manager(&stack.subagents);
    hold.open_sender();
    drained.await.unwrap();

    let flushes = flushes.lock().unwrap().clone();
    assert!(!flushes.is_empty());
    assert_eq!(flushes.last(), Some(&true));
}

#[tokio::test]
async fn releases_an_accepted_message_that_is_discarded_instead_of_run() {
    let (entry, hold) = Entry::gated(text_response("working"));
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
        message("discarded"),
    )
    .await
    .unwrap();

    let drained = drain_manager(&stack.subagents);
    hold.open_sender();
    drained.await.unwrap();

    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    let loaded = load(&stack.context, &started.child_id).await;
    assert!(!has_user_text(&loaded.events, "discarded"));
}

#[tokio::test]
async fn settles_after_a_delivery_discarded_inside_its_own_admission_window() {
    let (entry, release_first) = Entry::gated(text_response("working"));
    let stack = setup(vec![entry]).await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_requests(&stack, 1).await;
    let child = stack.dependencies.agents.get(&started.child_id).unwrap();

    // Cancel from the synchronous enqueue observer: the discard fires after the
    // id is recorded but before `followup()` returns.
    let cancelling = Arc::clone(&child);
    let off = child
        .context()
        .events()
        .on_sync(
            child.context(),
            "agent/inbox/inserted",
            move |_, args| {
                if let Some(payload) =
                    args.get::<seekdeep_agent::AgentEvent<seekdeep_agent_loop::AgentInboxMessage>>(0)
                    && payload.payload.message.content().iter().any(|block| {
                        matches!(block, seekdeep_llm::ContentBlock::Text { text } if text == "doomed")
                    })
                {
                    cancelling
                        .cancel(AgentCancelCause::User, CancelOptions::default())
                        .unwrap();
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    followup(
        &stack,
        &stack.parent.agent,
        &started.child_id,
        message("doomed"),
    )
    .await
    .unwrap();
    off.dispose().await.unwrap();

    release_first.open_sender();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    let loaded = load(&stack.context, &started.child_id).await;
    assert!(!has_user_text(&loaded.events, "doomed"));
}

#[tokio::test]
async fn releases_older_ids_discarded_during_a_later_admission_window() {
    let (entry, release_first) = Entry::gated(text_response("working"));
    let stack = setup(vec![entry]).await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_requests(&stack, 1).await;
    let child = stack.dependencies.agents.get(&started.child_id).unwrap();

    followup(
        &stack,
        &stack.parent.agent,
        &started.child_id,
        message("queued"),
    )
    .await
    .unwrap();
    let cancelling = Arc::clone(&child);
    let off = child
        .context()
        .events()
        .on_sync(
            child.context(),
            "agent/inbox/inserted",
            move |_, args| {
                if let Some(payload) =
                    args.get::<seekdeep_agent::AgentEvent<seekdeep_agent_loop::AgentInboxMessage>>(0)
                    && payload.payload.message.content().iter().any(|block| {
                        matches!(block, seekdeep_llm::ContentBlock::Text { text } if text == "doomed")
                    })
                {
                    cancelling
                        .cancel(AgentCancelCause::User, CancelOptions::default())
                        .unwrap();
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    followup(
        &stack,
        &stack.parent.agent,
        &started.child_id,
        message("doomed"),
    )
    .await
    .unwrap();
    off.dispose().await.unwrap();

    release_first.open_sender();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
}

#[tokio::test]
async fn reports_a_prompt_a_pre_step_rejection_discarded_as_refusal() {
    let stack = setup(vec![]).await;
    park_parent(&stack.context, &stack.parent.agent);
    let ends = record_ends(&stack);
    child_pre_step(&stack.context, |_, _| async {
        Ok(Some(PreStepDecision::Reject))
    });

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
        SubagentStopReason::Refusal
    );
}

#[tokio::test]
async fn retains_the_activation_while_an_accepted_message_is_still_in_the_inbox() {
    let (first, release_first) = Entry::gated(text_response("first"));
    let stack = setup(vec![first, Entry::chunks(text_response("second"))]).await;
    let registered_at_enqueue: Shared<Vec<bool>> = Arc::default();
    let observed = Arc::clone(&registered_at_enqueue);
    let agents = Arc::clone(&stack.dependencies.agents);
    stack
        .context
        .events()
        .on_sync(
            &stack.context,
            "agent/inbox/inserted",
            move |_, args| {
                if let Some(payload) = args
                    .get::<seekdeep_agent::AgentEvent<seekdeep_agent_loop::AgentInboxMessage>>(0)
                    && payload.agent.session().header().parent_session.is_some()
                {
                    observed.lock().unwrap().push(
                        agents
                            .get(payload.agent.id())
                            .is_some_and(|live| Arc::ptr_eq(&live, &payload.agent)),
                    );
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();

    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_requests(&stack, 1).await;
    let child = stack.dependencies.agents.get(&started.child_id).unwrap();
    followup(
        &stack,
        &stack.parent.agent,
        &started.child_id,
        message("queued"),
    )
    .await
    .unwrap();

    let registered = registered_at_enqueue.lock().unwrap().clone();
    assert!(!registered.is_empty());
    assert!(!registered.contains(&false));
    assert!(Arc::ptr_eq(
        &stack.dependencies.agents.get(&started.child_id).unwrap(),
        &child
    ));
    release_first.open_sender();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    let child_requests = stack
        .adapter
        .requests
        .lock()
        .iter()
        .filter(|request| request.session_id.as_ref() == Some(&started.child_id))
        .count();
    assert_eq!(child_requests, 2);
    let loaded = load(&stack.context, &started.child_id).await;
    assert!(has_user_text(&loaded.events, "queued"));
}

#[tokio::test]
async fn tells_the_parent_what_the_child_finished_with_without_being_asked() {
    let stack = setup(vec![
        Entry::chunks(text_response("the answer")),
        Entry::chunks(text_response("parent ack")),
    ])
    .await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;

    let notices = wait_notices(&stack.parent.agent, 1).await;
    assert_eq!(notices[0].sender, started.child_id.to_string());
    assert_eq!(
        notices[0].text,
        format!(
            "Background subagent {} finished and will do no further work unless you send it more.\nIts closing message:\nthe answer",
            started.child_id
        )
    );
    assert_eq!(
        notices[0].summary,
        format!(
            "Background subagent {} finished and will do no further work unless you send it more.",
            started.child_id
        )
    );
}

#[tokio::test]
async fn delivers_even_when_the_child_already_reported_for_itself() {
    let stack = setup(vec![
        Entry::chunks(text_response("the answer")),
        Entry::chunks(text_response("parent ack")),
    ])
    .await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    let child = wait_activation(&stack.dependencies.agents, &started.child_id).await;
    stack
        .subagents
        .report_from(
            &child,
            message("an explicit report"),
            SubagentReportOptions {
                delivery: SubagentReportDelivery::Quiet,
                signal: AbortSignal::default(),
            },
        )
        .unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    wait_notices(&stack.parent.agent, 1).await;
}

#[tokio::test]
async fn delivers_the_terminal_reason_when_the_child_never_had_a_chance_to_report() {
    let stack = setup(vec![
        Entry::chunks(max_tokens_response("half an ans")),
        Entry::chunks(text_response("parent ack")),
    ])
    .await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    let notices = wait_notices(&stack.parent.agent, 1).await;
    assert_eq!(
        notices[0].text,
        format!(
            "Background subagent {} ran out of room before it finished.\nIts closing message:\nhalf an ans",
            started.child_id
        )
    );
}

#[tokio::test]
async fn tells_the_parent_a_policy_rejected_delivery_was_declined_not_finished() {
    let stack = setup(vec![Entry::chunks(text_response("parent ack"))]).await;
    child_pre_step(&stack.context, |_, _| async {
        Ok(Some(PreStepDecision::Reject))
    });
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    let notices = wait_notices(&stack.parent.agent, 1).await;
    assert_eq!(
        notices[0].text,
        format!(
            "Background subagent {} declined the task.\nIt left no closing message.",
            started.child_id
        )
    );
}

#[tokio::test]
async fn reports_a_turn_that_failed_before_reaching_its_first_step() {
    let (first, release_first) = Entry::gated(text_response("the answer"));
    let stack = setup(vec![first, Entry::chunks(text_response("parent ack"))]).await;
    // The shipped durability checkpoint is fail-closed at the step boundary, so
    // a rejected write ends the turn after it claimed its messages and before
    // it entered a step.
    child_pre_step(&stack.context, |_, turn| async move {
        if turn < 2 {
            return Ok(None);
        }
        anyhow::bail!("ENOSPC: no space left on device")
    });

    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    followup(
        &stack,
        &stack.parent.agent,
        &started.child_id,
        message("second task"),
    )
    .await
    .unwrap();
    release_first.open_sender();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;

    let notices = wait_notices(&stack.parent.agent, 1).await;
    let child = load(&stack.context, &started.child_id).await;
    assert!(!has_user_text(&child.events, "second task"));
    assert_eq!(
        notices[0].text,
        format!(
            "Background subagent {} failed before it finished.\nIts closing message:\nthe answer",
            started.child_id
        )
    );
}

#[tokio::test]
async fn reports_accepted_work_cut_short_before_its_first_step_as_stopped() {
    let (first, release_first) = Entry::gated(text_response("the answer"));
    let stack = setup(vec![first]).await;
    let at_checkpoint = Gate::new();
    let release_checkpoint = Gate::new();
    let reached = Arc::clone(&at_checkpoint);
    let released = Arc::clone(&release_checkpoint);
    child_pre_step(&stack.context, move |_, turn| {
        let reached = Arc::clone(&reached);
        let released = Arc::clone(&released);
        async move {
            if turn < 2 {
                return Ok(None);
            }
            reached.open();
            released.wait().await;
            Ok(None)
        }
    });

    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    followup(
        &stack,
        &stack.parent.agent,
        &started.child_id,
        message("second task"),
    )
    .await
    .unwrap();
    release_first.open_sender();
    at_checkpoint.wait().await;
    let drained = drain_manager(&stack.subagents);
    release_checkpoint.open();
    drained.await.unwrap();

    let notices = wait_notices(&stack.parent.agent, 1).await;
    assert_eq!(
        notices[0].text,
        format!(
            "Background subagent {} was stopped before it finished.\nIts closing message:\nthe answer",
            started.child_id
        )
    );
}

#[tokio::test]
async fn reports_a_child_stopped_before_it_ever_reached_the_model_as_stopped() {
    let stack = setup(vec![]).await;
    let at_checkpoint = Gate::new();
    let release_checkpoint = Gate::new();
    let reached = Arc::clone(&at_checkpoint);
    let released = Arc::clone(&release_checkpoint);
    child_pre_step(&stack.context, move |_, _| {
        let reached = Arc::clone(&reached);
        let released = Arc::clone(&released);
        async move {
            reached.open();
            released.wait().await;
            Ok(None)
        }
    });

    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    at_checkpoint.wait().await;
    let drained = drain_manager(&stack.subagents);
    release_checkpoint.open();
    drained.await.unwrap();

    let notices = wait_notices(&stack.parent.agent, 1).await;
    assert_eq!(
        notices[0].text,
        format!(
            "Background subagent {} was stopped before it finished.\nIt left no closing message.",
            started.child_id
        )
    );
}

#[tokio::test]
async fn reports_a_child_an_ancestor_interrupted_before_its_first_step_as_stopped() {
    let stack = setup(vec![Entry::chunks(text_response("parent ack"))]).await;
    let at_checkpoint = Gate::new();
    let release_checkpoint = Gate::new();
    let reached = Arc::clone(&at_checkpoint);
    let released = Arc::clone(&release_checkpoint);
    child_pre_step(&stack.context, move |_, _| {
        let reached = Arc::clone(&reached);
        let released = Arc::clone(&released);
        async move {
            reached.open();
            released.wait().await;
            Ok(None)
        }
    });

    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    at_checkpoint.wait().await;
    stack
        .subagents
        .interrupt(
            started.child_id.clone(),
            SubagentInterruptAuthority::Ancestor {
                agent: Arc::clone(&stack.parent.agent),
            },
        )
        .unwrap();
    release_checkpoint.open();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;

    let notices = wait_notices(&stack.parent.agent, 1).await;
    assert_eq!(
        notices[0].text,
        format!(
            "Background subagent {} was stopped before it finished.\nIt left no closing message.",
            started.child_id
        )
    );
}

#[tokio::test]
async fn reports_accepted_work_cancelled_before_any_turn_could_open_as_stopped() {
    let (child_entry, release_child) = Entry::gated(text_response("the answer"));
    let (grandchild_entry, release_grandchild) = Entry::gated(text_response("grandchild"));
    let stack = setup(vec![child_entry, grandchild_entry]).await;
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
    release_child.open_sender();
    wait_for(Duration::from_secs(5), || {
        (child.status() == AgentStatus::Idle).then_some(())
    })
    .await;

    let release_maintenance = Gate::new();
    let held = Arc::clone(&release_maintenance);
    let maintaining = child
        .run_maintenance(move |_| async move { held.wait().await })
        .unwrap();
    followup(
        &stack,
        &stack.parent.agent,
        &started.child_id,
        message("never runs"),
    )
    .await
    .unwrap();
    let drained = drain_manager(&stack.subagents);
    release_maintenance.open();
    release_grandchild.open_sender();
    maintaining.await.unwrap();
    drained.await.unwrap();

    assert!(!has_user_text(&child.session().events(), "never runs"));
    let notices = wait_notices(&stack.parent.agent, 1).await;
    assert_eq!(
        notices[0].text,
        format!(
            "Background subagent {} was stopped before it finished.\nIts closing message:\nthe answer",
            started.child_id
        )
    );
}

#[tokio::test]
async fn gives_an_idle_parent_one_ordinary_turn_on_the_notice() {
    let stack = setup(vec![
        Entry::chunks(text_response("the answer")),
        Entry::chunks(text_response("parent ack")),
    ])
    .await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    wait_for(Duration::from_secs(5), || {
        let parent_requests = stack
            .adapter
            .requests
            .lock()
            .iter()
            .filter(|request| request.session_id.as_ref() == Some(stack.parent.agent.id()))
            .count();
        (parent_requests == 1).then_some(())
    })
    .await;
    assert_eq!(turn_starts(&stack.parent.agent), [1]);
}

#[tokio::test]
async fn batches_simultaneous_notices_into_one_step_of_a_busy_parent() {
    let (parent_entry, release_parent) = Entry::gated(text_response("parent works"));
    let (first_child, release_first) = Entry::gated(text_response("first child"));
    let (second_child, release_second) = Entry::gated(text_response("second child"));
    let stack = setup(vec![
        parent_entry,
        first_child,
        second_child,
        Entry::chunks(text_response("parent reacts")),
    ])
    .await;
    stack
        .parent
        .agent
        .followup(UserMessage::new(
            message("start working"),
            MessageSource::user(),
        ))
        .unwrap();
    wait_for(Duration::from_secs(5), || {
        (stack.parent.agent.status() == AgentStatus::Running).then_some(())
    })
    .await;

    let first = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    let second = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    release_first.open_sender();
    release_second.open_sender();
    wait_no_activation(&stack.dependencies.agents, &first.child_id).await;
    wait_no_activation(&stack.dependencies.agents, &second.child_id).await;

    assert_eq!(stack.parent.agent.inbox().next_step().len(), 2);
    assert_eq!(stack.parent.agent.inbox().next_turn().len(), 0);
    let turns_before = turn_starts(&stack.parent.agent);
    release_parent.open_sender();
    let notices = wait_notices(&stack.parent.agent, 2).await;
    assert_eq!(turn_starts(&stack.parent.agent), turns_before);
    let mut senders = notices
        .iter()
        .map(|notice| notice.sender.clone())
        .collect::<Vec<_>>();
    senders.sort();
    let mut expected = vec![first.child_id.to_string(), second.child_id.to_string()];
    expected.sort();
    assert_eq!(senders, expected);
}

#[tokio::test]
async fn holds_a_maintaining_parent_live_until_it_can_read_the_notice() {
    let (first_inner, release_first) = Entry::gated(text_response("first inner"));
    let (second_inner, release_second) = Entry::gated(text_response("second inner"));
    let stack = setup(vec![
        Entry::chunks(text_response("outer")),
        first_inner,
        second_inner,
        Entry::chunks(text_response("outer reacts")),
        Entry::chunks(text_response("root reacts")),
    ])
    .await;
    let outer = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    let middle = wait_activation(&stack.dependencies.agents, &outer.child_id).await;
    let first = stack
        .subagents
        .start_continuable(spawn_spec(&middle))
        .await
        .unwrap();
    let second = stack
        .subagents
        .start_continuable(spawn_spec(&middle))
        .await
        .unwrap();
    wait_for(Duration::from_secs(5), || {
        (middle.status() == AgentStatus::Idle).then_some(())
    })
    .await;

    let maintaining_gate = Gate::new();
    let held = Arc::clone(&maintaining_gate);
    let maintenance = middle
        .run_maintenance(move |_| async move { held.wait().await })
        .unwrap();
    release_first.open_sender();
    wait_no_activation(&stack.dependencies.agents, &first.child_id).await;
    release_second.open_sender();
    wait_no_activation(&stack.dependencies.agents, &second.child_id).await;
    assert!(Arc::ptr_eq(
        &stack.dependencies.agents.get(&outer.child_id).unwrap(),
        &middle
    ));

    maintaining_gate.open();
    maintenance.await.unwrap();
    let notices = wait_notices(&middle, 2).await;
    assert_eq!(
        notices
            .iter()
            .map(|notice| notice.sender.clone())
            .collect::<Vec<_>>(),
        [first.child_id.to_string(), second.child_id.to_string()]
    );
    wait_no_activation(&stack.dependencies.agents, &outer.child_id).await;
}

#[tokio::test]
async fn does_not_wake_a_parent_whose_own_teardown_already_began() {
    let (entry, hold) = Entry::gated(text_response("interrupted"));
    let stack = setup(vec![entry]).await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_activation(&stack.dependencies.agents, &started.child_id).await;

    let drained = drain_manager(&stack.subagents);
    hold.open_sender();
    drained.await.unwrap();

    let notices = settlement_notices(&stack.parent.agent);
    assert_eq!(notices.len(), 1);
    assert_eq!(
        notices[0].text,
        format!(
            "Background subagent {} was stopped before it finished.\nIt left no closing message.",
            started.child_id
        )
    );
    let events = stack.parent.agent.session().events();
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "agent/inbox/spliced")
    );
    assert!(!events.iter().any(|event| event.event_type == "turn/start"));
    assert_eq!(stack.parent.agent.status(), AgentStatus::Idle);
}

#[tokio::test]
async fn does_not_wake_a_parent_below_a_scoped_teardown_root() {
    let (entry, hold) = Entry::gated(text_response("interrupted"));
    let stack = setup(vec![entry]).await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_activation(&stack.dependencies.agents, &started.child_id).await;

    let drained = stack
        .subagents
        .drain_continuable_descendants(std::slice::from_ref(&stack.parent.agent));
    hold.open_sender();
    drained.await.unwrap();

    assert_eq!(settlement_notices(&stack.parent.agent).len(), 1);
    assert!(
        !stack
            .parent
            .agent
            .session()
            .events()
            .iter()
            .any(|event| event.event_type == "turn/start")
    );
}

#[tokio::test]
async fn records_but_cannot_deliver_a_teardown_notice_once_the_parent_is_disposed_too() {
    let (entry, hold) = Entry::gated(text_response("interrupted"));
    let stack = setup(vec![entry]).await;
    let host = create_agent(&stack.dependencies.agents, "closing-parent", true).await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&host.agent))
        .await
        .unwrap();
    wait_activation(&stack.dependencies.agents, &started.child_id).await;

    let drained = stack
        .subagents
        .drain_continuable_descendants(std::slice::from_ref(&host.agent));
    hold.open_sender();
    drained.await.unwrap();
    assert_eq!(settlement_notices(&host.agent).len(), 1);

    // Disposal is a keep-inbox: false cancel, so it durably cancels the notice
    // it never claimed; a resumed parent reads the log, not a pending message.
    host.dispose().await.unwrap();
    let resumed = stack
        .dependencies
        .agents
        .resume(seekdeep_agent::ResumeAgentOptions {
            resume_session_id: SessionId::new("closing-parent"),
            agent_options: seekdeep_agent::AgentOptions {
                provider: Some(seekdeep_llm::ProviderId::new("mock")),
                model: Some(seekdeep_llm::ModelId::new("mock")),
                max_tokens: None,
                subagent_depth: None,
            },
            signal: None,
            setup: None,
            owner_agent: None,
        })
        .await
        .unwrap();
    assert!(settlement_notices(&resumed.agent).is_empty());
}
