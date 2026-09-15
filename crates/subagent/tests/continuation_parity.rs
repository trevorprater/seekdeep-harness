//! `SubagentRuntime.startContinuable` and followup residency routing, ported
//! from the pinned `continuation.spec.ts` against the assembled Rust stack.

mod support;

use std::{sync::Arc, time::Duration};

use seekdeep_cordis::{EventOptions, EventReply};
use seekdeep_core::session::{SessionId, SessionOrigin};
use seekdeep_llm::{AbortSignal, ContentBlock, MessageSource, UserMessage};
use seekdeep_subagent::{
    ContinuableStartRequest, ContinuableStartSpec, SUBAGENT_DESCRIPTOR_VERSION,
};
use seekdeep_tools::{ContentToolFixtureOptions, ToolRestriction, define_content_tool_fixture};
use serde_json::json;
use support::continuation::*;

#[tokio::test]
async fn returns_both_identities_at_inbox_acceptance_without_waiting_for_the_turn_or_the_log() {
    let stack = setup(vec![Entry::chunks(text_response("first answer"))]).await;
    let enqueued: Shared<Vec<(String, bool)>> = Arc::default();
    let observed = Arc::clone(&enqueued);
    stack
        .context
        .events()
        .on_sync(
            &stack.context,
            "agent/inbox/inserted",
            move |_, args| {
                let payload = args
                    .get::<seekdeep_agent::AgentEvent<seekdeep_agent_loop::AgentInboxMessage>>(0)
                    .ok_or_else(|| anyhow::anyhow!("missing inbox payload"))?;
                // Acceptance is the boundary `startContinuable` resolves at, so
                // observe the log state exactly there.
                let logged_yet = has_user_text(&payload.agent.session().events(), "child task");
                observed
                    .lock()
                    .unwrap()
                    .push((payload.payload.message.id().to_string(), logged_yet));
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
    let child_id = started.child_id.to_string();
    assert_eq!(child_id.len(), 36);
    assert!(child_id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
    assert_eq!(
        *enqueued.lock().unwrap(),
        vec![(started.message_id.to_string(), false)]
    );
    assert_eq!(stack.adapter.request_count(), 0);

    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    let loaded = load(&stack.context, &started.child_id).await;
    assert!(has_user_text(&loaded.events, "child task"));
}

#[tokio::test]
async fn rejects_without_ids_when_the_provider_has_no_prepare_continuable_capability() {
    let stack = setup(vec![]).await;
    let provider = support::providers::ScriptedProvider::one_shot_only("one-shot");
    let _registration = stack.subagents.register_provider(provider.clone()).unwrap();
    let error = stack
        .subagents
        .start_continuable(start_spec(
            &stack.parent.agent,
            "one-shot",
            AbortSignal::default(),
        ))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("does not support continuable children"),
        "{error}"
    );
    assert_eq!(provider.starts(), 0);
    assert_eq!(agent_ids(&stack.dependencies.agents), ["parent"]);
}

#[tokio::test]
async fn rejects_synchronously_when_persistence_is_not_configured() {
    let stack = setup_with(
        vec![Entry::chunks(text_response("unused"))],
        SetupOptions {
            persistence: false,
            ..SetupOptions::default()
        },
    )
    .await;
    let error = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("require session persistence"),
        "{error}"
    );
}

#[tokio::test]
async fn publishes_the_reserved_child_id_and_appends_the_pre_turn_descriptor() {
    let stack = setup(vec![Entry::chunks(text_response("answer"))]).await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;

    let loaded = load(&stack.context, &started.child_id).await;
    let descriptor_index = loaded
        .events
        .iter()
        .position(|event| event.event_type == "subagent/descriptor")
        .unwrap();
    let turn_start_index = loaded
        .events
        .iter()
        .position(|event| event.event_type == "turn/start")
        .unwrap();
    assert!(descriptor_index < turn_start_index);
    let descriptor = &loaded.events[descriptor_index];
    assert_eq!(
        descriptor.data,
        json!({
            "version": SUBAGENT_DESCRIPTOR_VERSION,
            "mode": "continuable",
            "provider": "spawn",
            "label": "child task",
            "agentProvider": "mock",
            "agentModel": "mock",
        })
    );
    assert!(descriptor.surface_op.is_none());
    assert_eq!(loaded.meta.id, started.child_id);
    assert_eq!(loaded.meta.parent_session, Some(SessionId::new("parent")));
    assert_eq!(loaded.meta.origin, Some(SessionOrigin::Subagent));
}

#[tokio::test]
async fn rolls_the_child_back_completely_when_the_caller_signal_aborts_before_acceptance() {
    let stack = setup(vec![Entry::chunks(text_response("unused"))]).await;
    let signal = AbortSignal::default();
    let aborter = signal.clone();
    let parent_id = stack.parent.agent.id().clone();
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
                    aborter.abort_with_reason(json!("caller gave up"));
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();

    assert!(
        stack
            .subagents
            .start_continuable(start_spec(&stack.parent.agent, "spawn", signal))
            .await
            .is_err()
    );
    wait_for(Duration::from_secs(5), || {
        (agent_ids(&stack.dependencies.agents) == ["parent"]).then_some(())
    })
    .await;
}

#[tokio::test]
async fn rolls_the_child_back_when_the_signal_aborts_between_publication_and_acceptance() {
    let stack = setup(vec![Entry::chunks(text_response("unused"))]).await;
    let signal = AbortSignal::default();
    let aborter = signal.clone();
    stack
        .context
        .events()
        .on_sync(
            &stack.context,
            "subagent/start",
            move |_, _| {
                aborter.abort_with_reason(json!("caller gave up"));
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();

    assert!(
        stack
            .subagents
            .start_continuable(start_spec(&stack.parent.agent, "spawn", signal))
            .await
            .is_err()
    );
    wait_for(Duration::from_secs(5), || {
        (agent_ids(&stack.dependencies.agents) == ["parent"]).then_some(())
    })
    .await;
}

#[tokio::test]
async fn rejects_a_continuable_child_that_would_exceed_the_configured_depth_cap() {
    let stack = setup(vec![]).await;
    let mut spec = spawn_spec(&stack.parent.agent);
    spec.request = ContinuableStartRequest {
        prompt: message("deep"),
        parent: Arc::clone(&stack.parent.agent),
        max_depth: Some(0),
        ..spec.request
    };
    let error = stack.subagents.start_continuable(spec).await.unwrap_err();
    assert!(error.to_string().contains("exceeds maxDepth 0"), "{error}");
    assert_eq!(agent_ids(&stack.dependencies.agents), ["parent"]);
}

#[tokio::test]
async fn omits_undeclared_composition_fields_from_the_descriptor() {
    let stack = setup(vec![]).await;
    let routeless = create_agent(&stack.dependencies.agents, "routeless", false).await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&routeless.agent))
        .await
        .unwrap();
    let child = wait_activation(&stack.dependencies.agents, &started.child_id).await;
    let descriptor = events_of(&child.session().events(), "subagent/descriptor");
    assert_eq!(
        descriptor[0].data,
        json!({
            "version": SUBAGENT_DESCRIPTOR_VERSION,
            "mode": "continuable",
            "provider": "spawn",
            "label": "child task",
        })
    );
    drain_manager(&stack.subagents).await.unwrap();
}

#[tokio::test]
async fn records_a_declared_tool_filter_in_the_descriptor() {
    let stack = setup(vec![]).await;
    let noop = define_content_tool_fixture(ContentToolFixtureOptions::new(
        "noop",
        "does nothing",
        json!({}),
        Arc::new(|_args: serde_json::Value, _run| {
            Box::pin(async {
                Ok(vec![ContentBlock::Text {
                    text: "noop".into(),
                }])
            })
        }),
    ))
    .unwrap();
    stack
        .dependencies
        .tools
        .register(&stack.context, noop)
        .unwrap();
    let routeless = create_agent(&stack.dependencies.agents, "routeless-filtered", false).await;
    let spec = ContinuableStartSpec {
        request: ContinuableStartRequest {
            prompt: message("filtered work"),
            parent: Arc::clone(&routeless.agent),
            agent_options: None,
            max_depth: None,
            tool_filter: Some(ToolRestriction {
                allow: None,
                deny: Some(vec!["noop".to_owned()]),
            }),
            persona: None,
        },
        ..spawn_spec(&routeless.agent)
    };
    let started = stack.subagents.start_continuable(spec).await.unwrap();
    let child = wait_activation(&stack.dependencies.agents, &started.child_id).await;
    let descriptor = events_of(&child.session().events(), "subagent/descriptor");
    assert_eq!(
        descriptor[0].data,
        json!({
            "version": SUBAGENT_DESCRIPTOR_VERSION,
            "mode": "continuable",
            "provider": "spawn",
            "label": "child task",
            "toolFilter": { "deny": ["noop"] },
        })
    );
    drain_manager(&stack.subagents).await.unwrap();
}

#[tokio::test]
async fn cold_resumes_without_inventing_a_model_route_the_descriptor_never_declared() {
    let stack = setup(vec![Entry::chunks(text_response("first"))]).await;
    let routeless = create_agent(&stack.dependencies.agents, "routeless-resume", false).await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&routeless.agent))
        .await
        .unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;

    let adapter = MockAdapter::new(vec![]);
    let (fresh, fresh_dependencies, fresh_subagents) =
        boot(Some(stack.root.as_ref().unwrap().path()), &adapter, false).await;
    let fresh_parent = create_agent(&fresh_dependencies.agents, "routeless-resume", false).await;
    followup_with(
        &fresh_subagents,
        &fresh_parent.agent,
        &started.child_id,
        message("resume routeless"),
        AbortSignal::default(),
    )
    .await
    .unwrap();
    let resumed = wait_activation(&fresh_dependencies.agents, &started.child_id).await;
    assert_eq!(resumed.options().provider, None);
    assert_eq!(resumed.options().model, None);
    drain_manager(&fresh_subagents).await.unwrap();
    drop(fresh);
}

#[tokio::test]
async fn continues_turn_numbering_after_an_inherited_fork_prefix_and_pre_turn_descriptor() {
    let stack = setup(vec![
        Entry::chunks(text_response("parent turn")),
        Entry::chunks(text_response("forked child")),
    ])
    .await;
    stack
        .parent
        .agent
        .followup(UserMessage::new(
            message("parent work"),
            MessageSource::user(),
        ))
        .unwrap();
    stack.parent.agent.when_idle().unwrap().await.unwrap();

    let started = stack
        .subagents
        .start_continuable(start_spec(
            &stack.parent.agent,
            "fork",
            AbortSignal::default(),
        ))
        .await
        .unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;

    let loaded = load(&stack.context, &started.child_id).await;
    let descriptor_index = loaded
        .events
        .iter()
        .position(|event| event.event_type == "subagent/descriptor")
        .unwrap();
    let child_turn = loaded.events[descriptor_index + 1..]
        .iter()
        .find(|event| event.event_type == "turn/start")
        .unwrap();
    assert_eq!(child_turn.data["turn"], 2);
    assert!(loaded.meta.seed_length.unwrap_or(0) > 0);
}

#[tokio::test]
async fn records_the_declared_persona_in_the_descriptor_and_reapplies_it_on_cold_resume() {
    let stack = setup(vec![
        Entry::chunks(text_response("scoped")),
        Entry::chunks(text_response("resumed")),
    ])
    .await;
    let spec = ContinuableStartSpec {
        request: ContinuableStartRequest {
            prompt: message("scoped work"),
            parent: Arc::clone(&stack.parent.agent),
            agent_options: None,
            max_depth: None,
            tool_filter: None,
            persona: Some("You are scoped.".to_owned()),
        },
        ..spawn_spec(&stack.parent.agent)
    };
    let started = stack.subagents.start_continuable(spec).await.unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;

    let loaded = load(&stack.context, &started.child_id).await;
    let descriptor = events_of(&loaded.events, "subagent/descriptor");
    assert_eq!(descriptor[0].data["persona"], "You are scoped.");

    followup(
        &stack,
        &stack.parent.agent,
        &started.child_id,
        message("resume it"),
    )
    .await
    .unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    let resumed = load(&stack.context, &started.child_id).await;
    assert!(has_user_text(&resumed.events, "resume it"));
}

#[tokio::test]
async fn rolls_an_unpublished_activation_back_when_lifecycle_publication_fails() {
    let stack = setup(vec![Entry::chunks(text_response("unused"))]).await;
    let ends: Shared<Vec<seekdeep_subagent::SubagentRunEndInfo>> = Arc::default();
    let observed = Arc::clone(&ends);
    stack
        .context
        .events()
        .on_sync(
            &stack.context,
            "subagent/end",
            move |_, args| {
                if let Some(info) = args.get::<seekdeep_subagent::SubagentRunEndInfo>(0) {
                    observed.lock().unwrap().push((*info).clone());
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    stack
        .context
        .events()
        .on_sync(
            &stack.context,
            "internal/dispatch",
            |_, args| {
                if args
                    .get::<String>(1)
                    .is_some_and(|name| *name == "subagent/start")
                {
                    anyhow::bail!("start publication failed");
                }
                Ok(EventReply::Undefined)
            },
            EventOptions {
                global: true,
                ..EventOptions::default()
            },
        )
        .unwrap();

    let error = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("start publication failed"),
        "{error:#}"
    );
    wait_for(Duration::from_secs(5), || {
        (agent_ids(&stack.dependencies.agents) == ["parent"]).then_some(())
    })
    .await;
    assert!(ends.lock().unwrap().is_empty());
    drain_manager(&stack.subagents).await.unwrap();
}
