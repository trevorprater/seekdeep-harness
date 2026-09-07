//! Continuable-child delegation policy, ported from the pinned
//! `continuation-inheritance.spec.ts`: a fresh continuable start seeds the
//! parent's explicit sandbox override and the pinned `approval/policy: never`
//! onto the child's own log as delegation events, and a cold resume replays
//! that persisted snapshot instead of re-capturing the parent.

mod support;

use std::sync::Arc;

use seekdeep_cordis::{EventOptions, EventReply};
use seekdeep_core::session::SessionEvent;
use seekdeep_llm::{AbortSignal, MessageSource, UserMessage};
use seekdeep_sandbox::SandboxMode;
use seekdeep_sandbox_policy::{
    SANDBOX_POLICY, SandboxPolicyConfig, effective_sandbox_mode, set_sandbox_mode,
};
use seekdeep_user_approval::{APPROVAL, ApprovalConfig, ApprovalPolicy, effective_approval_policy};
use serde_json::json;
use support::continuation::*;

/// Boot the continuable stack plus both policy services the manager consumes opportunistically.
async fn setup_with_policies(script: Vec<Entry>) -> Stack {
    let stack = setup(script).await;
    seekdeep_sandbox_policy::install(
        &stack.context,
        SandboxPolicyConfig {
            mode: SandboxMode::WorkspaceWrite,
            workspace_root: Some(stack.root.as_ref().unwrap().path().to_owned()),
        },
    )
    .unwrap();
    seekdeep_user_approval::install(&stack.context, ApprovalConfig::default()).unwrap();
    stack.context.registry().await_quiescent().await;
    stack
}

fn policy_events(events: &[SessionEvent]) -> Vec<serde_json::Value> {
    events
        .iter()
        .filter(|event| event.event_type == "sandbox/mode" || event.event_type == "approval/policy")
        .map(|event| json!({ "type": event.event_type, "data": event.data }))
        .collect()
}

/// Observe the continuable child at creation, before its first turn.
fn observe_child(stack: &Stack) -> Shared<Option<Arc<seekdeep_agent::Agent>>> {
    let child: Shared<Option<Arc<seekdeep_agent::Agent>>> = Arc::default();
    let observed = Arc::clone(&child);
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
                    *observed.lock().unwrap() = Some(Arc::clone(&payload.agent));
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    child
}

#[tokio::test]
async fn seeds_the_parent_sandbox_override_and_pins_approval_to_never() {
    let stack = setup_with_policies(vec![Entry::chunks(text_response("child done"))]).await;
    let sandbox = stack.context.get(SANDBOX_POLICY).unwrap();
    let approval = stack.context.get(APPROVAL).unwrap();
    set_sandbox_mode(stack.parent.agent.session(), SandboxMode::DangerFullAccess).unwrap();
    assert_eq!(approval.override_of(stack.parent.agent.session()), None);
    let child = observe_child(&stack);

    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    let child = child
        .lock()
        .unwrap()
        .clone()
        .expect("expected the continuable child to be created");
    assert_eq!(
        sandbox.override_of(child.session()),
        Some(SandboxMode::DangerFullAccess)
    );
    assert_eq!(
        approval.override_of(child.session()),
        Some(ApprovalPolicy::Never)
    );

    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    let loaded = load(&stack.context, &started.child_id).await;
    let policies = policy_events(&loaded.events);
    assert_eq!(policies.len(), 2);
    assert_eq!(policies[0]["type"], "sandbox/mode");
    assert_eq!(policies[0]["data"]["mode"], "danger-full-access");
    assert_eq!(policies[0]["data"]["source"], "delegation");
    assert_eq!(policies[1]["type"], "approval/policy");
    assert_eq!(policies[1]["data"]["policy"], "never");
    assert_eq!(policies[1]["data"]["source"], "delegation");
    assert_eq!(
        effective_sandbox_mode(&loaded.events),
        Some(SandboxMode::DangerFullAccess)
    );
    assert_eq!(
        effective_approval_policy(&loaded.events),
        Some(ApprovalPolicy::Never)
    );
    assert_eq!(approval.override_of(stack.parent.agent.session()), None);
    let runtime_context = loaded
        .events
        .iter()
        .find(|event| {
            event.event_type == "user/message"
                && event.data["source"]["kind"] == "plugin"
                && event.data["source"]["plugin"] == "@seekdeep-ai/seekdeep-system-prompt"
        })
        .expect("runtime context message");
    let context_text = runtime_context.data["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|block| block["type"] == "text")
        .map(|block| block["text"].as_str().unwrap_or_default().to_owned())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(context_text.contains("You are a delegated subagent"));
}

#[tokio::test]
async fn captures_policy_at_delegation_before_asynchronous_child_creation() {
    let stack = setup_with_policies(vec![Entry::chunks(text_response("child done"))]).await;
    let sandbox = stack.context.get(SANDBOX_POLICY).unwrap();
    set_sandbox_mode(stack.parent.agent.session(), SandboxMode::ReadOnly).unwrap();

    let starting = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent));
    // The delegation snapshot is captured synchronously at the call; a parent
    // switch after it belongs to the parent's future, not to this child.
    let starting = {
        let mut starting = std::pin::pin!(starting);
        // Poll once so the synchronous capture prefix runs before the switch.
        let first = futures::poll!(starting.as_mut());
        set_sandbox_mode(stack.parent.agent.session(), SandboxMode::DangerFullAccess).unwrap();
        match first {
            std::task::Poll::Ready(result) => result,
            std::task::Poll::Pending => starting.await,
        }
    };
    let started = starting.unwrap();

    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    let loaded = load(&stack.context, &started.child_id).await;
    assert_eq!(
        sandbox.override_of(stack.parent.agent.session()),
        Some(SandboxMode::DangerFullAccess)
    );
    assert_eq!(
        effective_sandbox_mode(&loaded.events),
        Some(SandboxMode::ReadOnly)
    );
}

#[tokio::test]
async fn leaves_an_unswitched_sandbox_on_the_deployment_default_while_still_pinning_approval() {
    let stack = setup_with_policies(vec![Entry::chunks(text_response("child done"))]).await;
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;

    let loaded = load(&stack.context, &started.child_id).await;
    let policies = policy_events(&loaded.events);
    assert_eq!(policies.len(), 1);
    assert_eq!(policies[0]["type"], "approval/policy");
    assert_eq!(policies[0]["data"]["policy"], "never");
    assert_eq!(policies[0]["data"]["source"], "delegation");
    assert_eq!(effective_sandbox_mode(&loaded.events), None);
}

#[tokio::test]
async fn pins_approval_after_the_fork_prefix_of_an_unswitched_fork_child() {
    let stack = setup_with_policies(vec![
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
    assert!(loaded.meta.seed_length.unwrap_or(0) > 0);
    let policies = policy_events(&loaded.events);
    assert_eq!(policies.len(), 1);
    assert_eq!(policies[0]["type"], "approval/policy");
    assert_eq!(policies[0]["data"]["policy"], "never");
    assert_eq!(policies[0]["data"]["source"], "delegation");
    assert_eq!(effective_sandbox_mode(&loaded.events), None);
}

#[tokio::test]
async fn lets_a_later_child_side_switch_win_over_the_delegation_snapshot() {
    let stack = setup_with_policies(vec![Entry::chunks(text_response("child done"))]).await;
    let sandbox = stack.context.get(SANDBOX_POLICY).unwrap();
    set_sandbox_mode(stack.parent.agent.session(), SandboxMode::DangerFullAccess).unwrap();
    let child = observe_child(&stack);

    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    let child = child
        .lock()
        .unwrap()
        .clone()
        .expect("expected the continuable child to be created");
    assert_eq!(
        sandbox.override_of(child.session()),
        Some(SandboxMode::DangerFullAccess)
    );
    set_sandbox_mode(child.session(), SandboxMode::ReadOnly).unwrap();
    assert_eq!(
        sandbox.override_of(child.session()),
        Some(SandboxMode::ReadOnly)
    );

    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;
    let loaded = load(&stack.context, &started.child_id).await;
    assert_eq!(
        effective_sandbox_mode(&loaded.events),
        Some(SandboxMode::ReadOnly)
    );
}

#[tokio::test]
async fn cold_resumes_on_the_persisted_snapshot_without_re_capturing_the_parent() {
    let stack = setup_with_policies(vec![
        Entry::chunks(text_response("first")),
        Entry::chunks(text_response("after resume")),
    ])
    .await;
    set_sandbox_mode(stack.parent.agent.session(), SandboxMode::ReadOnly).unwrap();
    let started = stack
        .subagents
        .start_continuable(spawn_spec(&stack.parent.agent))
        .await
        .unwrap();
    wait_no_activation(&stack.dependencies.agents, &started.child_id).await;

    set_sandbox_mode(stack.parent.agent.session(), SandboxMode::DangerFullAccess).unwrap();
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
    let modes = events_of(&loaded.events, "sandbox/mode");
    assert_eq!(modes.len(), 1);
    assert_eq!(modes[0].data["mode"], "read-only");
    assert_eq!(modes[0].data["source"], "delegation");
    assert_eq!(
        effective_sandbox_mode(&loaded.events),
        Some(SandboxMode::ReadOnly)
    );
    let approvals = events_of(&loaded.events, "approval/policy");
    assert_eq!(approvals.len(), 1);
    assert_eq!(approvals[0].data["policy"], "never");
    assert_eq!(approvals[0].data["source"], "delegation");
}

#[tokio::test]
async fn places_inherited_events_after_a_fork_prefix_so_fresh_policy_wins_stale_seed_state() {
    let stack = setup_with_policies(vec![
        Entry::chunks(text_response("parent turn")),
        Entry::chunks(text_response("forked child")),
    ])
    .await;
    set_sandbox_mode(stack.parent.agent.session(), SandboxMode::WorkspaceWrite).unwrap();
    stack
        .parent
        .agent
        .followup(UserMessage::new(
            message("parent work"),
            MessageSource::user(),
        ))
        .unwrap();
    stack.parent.agent.when_idle().unwrap().await.unwrap();
    set_sandbox_mode(stack.parent.agent.session(), SandboxMode::ReadOnly).unwrap();

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
    assert!(loaded.meta.seed_length.unwrap_or(0) > 0);
    let modes = events_of(&loaded.events, "sandbox/mode");
    assert_eq!(modes.len(), 2);
    assert_eq!(modes[0].data["mode"], "workspace-write");
    assert_eq!(modes[1].data["mode"], "read-only");
    assert_eq!(modes[1].data["source"], "delegation");
    assert_eq!(
        effective_sandbox_mode(&loaded.events),
        Some(SandboxMode::ReadOnly)
    );
}
