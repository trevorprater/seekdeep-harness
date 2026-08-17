//! Approval-routing slice of `packages/core/tools/tests/tools.spec.ts`.

use std::{
    sync::Arc,
    sync::atomic::{AtomicUsize, Ordering},
};

use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_cordis::{Context, EventOptions};
use seekdeep_core::session::{AppendOptions, Session, SessionId};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock};
use seekdeep_scope::ScopeKey;
use seekdeep_tools::{
    PreToolDecision, TOOL_ABORTED_BEFORE_DISPATCH, ToolDefinition, ToolErrorInfo,
    ToolExecutionInput, ToolExecutionResult, ToolOutputDefinition, ToolRuntime, ToolRuntimeConfig,
    assert_supported_json_schema,
};
use seekdeep_user_approval::{
    ApprovalAnswer, ApprovalConfig, ApprovalOutcome, ApprovalRequest, install as install_approval,
};
use serde_json::{Map, Value, json};

struct Harness {
    root: Context,
    tools: Arc<ToolRuntime>,
    agent: Arc<Agent>,
    scope: ScopeKey,
    session: Arc<Session>,
}

fn harness(with_approval: bool) -> Harness {
    let root = Context::new();
    if with_approval {
        let _approval = install_approval(&root, ApprovalConfig::default()).expect("approval");
    }
    let tools = ToolRuntime::new(root.clone(), ToolRuntimeConfig::default()).expect("tools");
    tools
        .register(&root, echo_tool("echo"))
        .expect("register echo");
    let scope = ScopeKey::new();
    let session =
        Session::create(&SessionId::new("approval-routing"), None, None).expect("session");
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("open turn");
    let inbox =
        Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).expect("inbox"));
    let agent = Arc::new(Agent::new(
        session.id().clone(),
        AgentOptions::default(),
        session.clone(),
        inbox,
        root.clone(),
        scope,
    ));
    Harness {
        root,
        tools,
        agent,
        scope,
        session,
    }
}

fn echo_tool(name: &str) -> ToolDefinition {
    let schema =
        Arc::new(assert_supported_json_schema(json!({"type": "string"})).expect("string schema"));
    ToolDefinition::new(
        name,
        "echo",
        Map::from_iter([
            ("type".to_owned(), json!("object")),
            ("properties".to_owned(), json!({"text": {"type": "string"}})),
        ]),
        ToolOutputDefinition::new(
            schema,
            Arc::new(|_, value| {
                Ok(vec![ContentBlock::Text {
                    text: value.as_str().unwrap_or_default().to_owned(),
                }])
            }),
        ),
        Arc::new(|arguments, _| {
            Box::pin(async move {
                Ok(Value::String(
                    arguments
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                ))
            })
        }),
    )
}

fn call(harness: &Harness, signal: AbortSignal) -> ToolExecutionInput {
    ToolExecutionInput::new(CallId::new("c1"), "echo", json!({"text": "hi"}), signal)
        .with_agent(harness.agent.clone())
}

fn ask(harness: &Harness, reason: Option<&str>) -> seekdeep_cordis::fiber::EffectHandle {
    let reason = reason.map(str::to_owned);
    harness
        .tools
        .on_pre_execute(
            &harness.root,
            move |_, _| {
                let reason = reason.clone();
                async move { Ok(PreToolDecision::Ask { reason }) }
            },
            EventOptions::default(),
        )
        .expect("ask policy")
}

fn failure_text(result: &ToolExecutionResult) -> &str {
    match result.content().first() {
        Some(ContentBlock::Text { text }) => text,
        other => panic!("expected text failure, got {other:?}"),
    }
}

#[tokio::test]
async fn ask_degrades_to_its_reason_without_an_approval_service() {
    let harness = harness(false);
    let _ask = ask(&harness, Some("needs approval"));
    let result = harness
        .tools
        .execute(call(&harness, AbortSignal::default()))
        .await;
    assert!(result.is_error());
    assert_eq!(failure_text(&result), "Error: needs approval");
}

#[tokio::test]
async fn reasonless_ask_degrades_to_the_historical_default_without_a_service() {
    let harness = harness(false);
    let _ask = ask(&harness, None);
    let result = harness
        .tools
        .execute(call(&harness, AbortSignal::default()))
        .await;
    assert_eq!(
        failure_text(&result),
        "Error: tool \"echo\" requires approval (not yet supported)"
    );
}

#[tokio::test]
async fn allowed_once_dispatches_and_forwards_every_ask_field() {
    let harness = harness(true);
    let service = harness
        .root
        .get(seekdeep_user_approval::APPROVAL)
        .expect("approval service");
    let seen = Arc::new(Mutex::new(Vec::<ApprovalRequest>::new()));
    let answer_seen = seen.clone();
    service
        .on_request(
            &harness.root,
            move |request, _| {
                answer_seen.lock().push(request);
                async { Ok(ApprovalOutcome::AllowedOnce.into()) }
            },
            EventOptions::default(),
        )
        .expect("answerer");
    let _ask = ask(&harness, Some("hook wants a human"));
    let signal = AbortSignal::default();
    let result = harness.tools.execute(call(&harness, signal.clone())).await;
    assert!(!result.is_error());
    assert_eq!(failure_text(&result), "hi");
    let seen = seen.lock();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].tool_name, "echo");
    assert_eq!(seen[0].call_id.as_ref().map(CallId::as_str), Some("c1"));
    assert_eq!(seen[0].reason.as_deref(), Some("hook wants a human"));
    assert_eq!(seen[0].signal.as_ref(), Some(&signal));
    assert!(Arc::ptr_eq(&seen[0].agent, &harness.agent));
    assert_eq!(seen[0].agent.scope_key(), harness.scope);
    assert!(Arc::ptr_eq(seen[0].agent.session(), &harness.session));
}

#[tokio::test]
async fn rejected_denies_with_the_user_rejection_reason() {
    let harness = harness(true);
    let service = harness.root.get(seekdeep_user_approval::APPROVAL).unwrap();
    service
        .on_request(
            &harness.root,
            |_, _| async { Ok(ApprovalOutcome::Rejected.into()) },
            EventOptions::default(),
        )
        .unwrap();
    let _ask = ask(&harness, None);
    let result = harness
        .tools
        .execute(call(&harness, AbortSignal::default()))
        .await;
    assert_eq!(
        failure_text(&result),
        "Error: the user rejected tool \"echo\""
    );
}

#[tokio::test]
async fn cancelled_denies_with_the_distinct_cancellation_reason() {
    let harness = harness(true);
    let service = harness.root.get(seekdeep_user_approval::APPROVAL).unwrap();
    service
        .on_request(
            &harness.root,
            |_, _| async { Ok(ApprovalOutcome::Cancelled.into()) },
            EventOptions::default(),
        )
        .unwrap();
    let _ask = ask(&harness, None);
    let result = harness
        .tools
        .execute(call(&harness, AbortSignal::default()))
        .await;
    assert_eq!(
        failure_text(&result),
        "Error: approval for tool \"echo\" was cancelled"
    );
}

#[tokio::test]
async fn caller_cancellation_overtaking_approval_is_aborted_before_dispatch() {
    let harness = harness(true);
    let dispatched = Arc::new(AtomicUsize::new(0));
    let body_dispatched = dispatched.clone();
    let mut probe = echo_tool("approval-probe");
    probe.execute = Arc::new(move |_, _| {
        body_dispatched.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(json!("ran")) })
    });
    harness.tools.register(&harness.root, probe).expect("probe");
    let service = harness.root.get(seekdeep_user_approval::APPROVAL).unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let answer_entered = entered.clone();
    service
        .on_request(
            &harness.root,
            move |_, _| {
                answer_entered.notify_one();
                async {
                    futures::future::pending::<()>().await;
                    Ok(ApprovalOutcome::AllowedOnce.into())
                }
            },
            EventOptions::default(),
        )
        .unwrap();
    let _ask = ask(&harness, None);
    let signal = AbortSignal::default();
    let pending = harness.tools.execute(
        ToolExecutionInput::new(
            CallId::new("approval-cancelled"),
            "approval-probe",
            json!({}),
            signal.clone(),
        )
        .with_agent(harness.agent.clone()),
    );
    tokio::pin!(pending);
    tokio::select! {
        () = entered.notified() => {}
        result = &mut pending => panic!("approval settled too soon: {result:?}"),
    }
    signal.abort_with_reason(json!("caller cancelled approval"));
    let result = pending.await;
    assert_eq!(
        result.error().and_then(|failure| failure.info.as_ref()),
        Some(&ToolErrorInfo {
            name: "AbortError".to_owned(),
            code: TOOL_ABORTED_BEFORE_DISPATCH.to_owned(),
        })
    );
    assert_eq!(dispatched.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn missing_answerer_denies_with_no_channel_reason() {
    let harness = harness(true);
    let _ask = ask(&harness, None);
    let result = harness
        .tools
        .execute(call(&harness, AbortSignal::default()))
        .await;
    assert_eq!(
        failure_text(&result),
        "Error: tool \"echo\" requires approval, but no approval channel is available"
    );
}

#[tokio::test]
async fn agentless_execution_denies_without_dispatching_an_approval_request() {
    let harness = harness(true);
    let service = harness.root.get(seekdeep_user_approval::APPROVAL).unwrap();
    let asked = Arc::new(AtomicUsize::new(0));
    let answer_asked = asked.clone();
    service
        .on_request(
            &harness.root,
            move |_, _| {
                answer_asked.fetch_add(1, Ordering::SeqCst);
                async { Ok(ApprovalOutcome::AllowedOnce.into()) }
            },
            EventOptions::default(),
        )
        .unwrap();
    let _ask = ask(&harness, None);
    let result = harness
        .tools
        .execute(ToolExecutionInput::new(
            CallId::new("c1"),
            "echo",
            json!({}),
            AbortSignal::default(),
        ))
        .await;
    assert_eq!(asked.load(Ordering::SeqCst), 0);
    assert_eq!(
        failure_text(&result),
        "Error: tool \"echo\" requires approval, but the call has no agent to route it through"
    );
}

#[tokio::test]
async fn rogue_answerer_value_is_normalized_fail_closed_by_the_typed_service() {
    let harness = harness(true);
    let service = harness.root.get(seekdeep_user_approval::APPROVAL).unwrap();
    service
        .on_request(
            &harness.root,
            |_, _| async { Ok(ApprovalAnswer::Unknown("yolo".to_owned())) },
            EventOptions::default(),
        )
        .unwrap();
    let _ask = ask(&harness, None);
    let result = harness
        .tools
        .execute(call(&harness, AbortSignal::default()))
        .await;
    assert_eq!(
        failure_text(&result),
        "Error: tool \"echo\" requires approval, but no approval channel is available"
    );
}
