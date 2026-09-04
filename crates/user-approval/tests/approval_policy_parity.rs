//! Exact policy-fold and prompt-context mirror of the source approval oracle.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_cordis::{Context, EventOptions};
use seekdeep_core::session::{AppendOptions, Session, SessionId};
use seekdeep_llm::{ContentBlock, MessageSource, UserMessage};
use seekdeep_scope::ScopeKey;
use seekdeep_system_prompt::{AssembleContext, SystemPromptConfig, install as install_prompt};
use seekdeep_user_approval::{
    ASK_SENTENCE, ApprovalAnswer, ApprovalConfig, ApprovalOutcome, ApprovalPolicy, ApprovalRequest,
    ApprovalService, NEVER_SENTENCE, effective_approval_policy, install, set_approval_policy,
    set_approval_policy_str,
};
use serde_json::json;

static NEXT_SESSION: AtomicUsize = AtomicUsize::new(1);

fn session() -> Arc<Session> {
    let id = SessionId::new(format!(
        "approval-policy-{}",
        NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
    ));
    let session = Session::create(&id, None, None).expect("session");
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("turn start");
    session
}

fn request(session: Arc<Session>) -> ApprovalRequest {
    ApprovalRequest::new(agent(session, ScopeKey::new()), "echo")
}

fn agent(session: Arc<Session>, scope: ScopeKey) -> Arc<Agent> {
    let inbox =
        Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).expect("inbox"));
    Arc::new(Agent::new(
        session.id().clone(),
        AgentOptions::default(),
        session,
        inbox,
        Context::new(),
        scope,
    ))
}

#[test]
fn fold_returns_the_last_policy_or_none() {
    let session = session();
    assert_eq!(effective_approval_policy(&session.events()), None);
    set_approval_policy(&session, ApprovalPolicy::Never).expect("never");
    set_approval_policy(&session, ApprovalPolicy::Ask).expect("ask");
    assert_eq!(
        effective_approval_policy(&session.events()),
        Some(ApprovalPolicy::Ask)
    );
    let last = session.events().pop().expect("last event");
    assert_eq!(last.event_type, "approval/policy");
    assert_eq!(last.data, json!({"policy": "ask"}));
}

#[test]
fn invalid_untrusted_policy_is_rejected_before_append() {
    let session = session();
    let before = session.seq();
    let error = set_approval_policy_str(&session, "sometimes").expect_err("invalid policy");
    assert_eq!(
        error.to_string(),
        "approval policy must be one of \"ask\" or \"never\""
    );
    assert_eq!(session.seq(), before);
}

#[tokio::test]
async fn direct_schema_less_construction_defaults_to_ask() {
    let context = Context::new();
    let service = ApprovalService::new(context.clone(), ApprovalConfig::default());
    service
        .on_request(
            &context,
            |_, _| async { Ok(ApprovalOutcome::AllowedOnce.into()) },
            EventOptions::default(),
        )
        .expect("answerer");
    assert_eq!(
        service.request(request(session())).await.expect("request"),
        ApprovalOutcome::AllowedOnce
    );
}

#[tokio::test]
async fn synchronously_panicking_answerer_is_contained_as_unavailable() {
    let context = Context::new();
    let approval = install(&context, ApprovalConfig::default()).expect("approval");
    approval
        .on_request(
            &context,
            |_, _| {
                panic!("sync bug");
                #[allow(unreachable_code)]
                async {
                    Ok(ApprovalOutcome::AllowedOnce.into())
                }
            },
            EventOptions::default(),
        )
        .expect("answerer");
    assert_eq!(
        approval.request(request(session())).await.expect("request"),
        ApprovalOutcome::Unavailable
    );
}

#[tokio::test]
async fn never_config_rejects_without_consulting_answerers_and_still_audits() {
    let context = Context::new();
    let approval = install(
        &context,
        ApprovalConfig {
            policy: ApprovalPolicy::Never,
        },
    )
    .expect("approval");
    let consulted = Arc::new(AtomicUsize::new(0));
    let answer_consulted = consulted.clone();
    approval
        .on_request(
            &context,
            move |_, next| {
                answer_consulted.fetch_add(1, Ordering::SeqCst);
                async move { next.run().await }
            },
            EventOptions::default(),
        )
        .expect("answerer");
    let session = session();
    assert_eq!(
        approval
            .request(request(session.clone()))
            .await
            .expect("request"),
        ApprovalOutcome::Rejected
    );
    assert_eq!(consulted.load(Ordering::SeqCst), 0);
    let audit = session
        .events()
        .into_iter()
        .filter(|event| event.event_type.starts_with("approval/"))
        .collect::<Vec<_>>();
    assert_eq!(audit.len(), 2);
}

#[tokio::test]
async fn never_gate_precedes_answerer_registered_before_service() {
    let context = Context::new();
    let unprovided = ApprovalService::new(context.clone(), ApprovalConfig::default());
    let consulted = Arc::new(AtomicUsize::new(0));
    let answer_consulted = consulted.clone();
    unprovided
        .on_request(
            &context,
            move |_, _| {
                answer_consulted.fetch_add(1, Ordering::SeqCst);
                async { Ok(ApprovalOutcome::AllowedOnce.into()) }
            },
            EventOptions::default(),
        )
        .expect("early answerer");
    let approval = install(
        &context,
        ApprovalConfig {
            policy: ApprovalPolicy::Never,
        },
    )
    .expect("approval");
    assert_eq!(
        approval.request(request(session())).await.expect("request"),
        ApprovalOutcome::Rejected
    );
    assert_eq!(consulted.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn never_is_unbypassable_by_a_later_prepended_answerer() {
    let context = Context::new();
    let approval = install(
        &context,
        ApprovalConfig {
            policy: ApprovalPolicy::Never,
        },
    )
    .expect("approval");
    let consulted = Arc::new(AtomicUsize::new(0));
    let answer_consulted = consulted.clone();
    approval
        .on_request(
            &context,
            move |_, _| {
                answer_consulted.fetch_add(1, Ordering::SeqCst);
                async { Ok(ApprovalOutcome::AllowedOnce.into()) }
            },
            EventOptions {
                prepend: true,
                ..EventOptions::default()
            },
        )
        .expect("prepended answerer");
    assert_eq!(
        approval.request(request(session())).await.expect("request"),
        ApprovalOutcome::Rejected
    );
    assert_eq!(consulted.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn session_override_outranks_default_in_both_directions() {
    let context = Context::new();
    let approval = install(
        &context,
        ApprovalConfig {
            policy: ApprovalPolicy::Never,
        },
    )
    .expect("approval");
    approval
        .on_request(
            &context,
            |_, _| async { Ok(ApprovalOutcome::AllowedOnce.into()) },
            EventOptions::default(),
        )
        .expect("answerer");
    let session = session();
    assert_eq!(approval.override_of(&session), None);
    set_approval_policy(&session, ApprovalPolicy::Ask).expect("ask override");
    assert_eq!(approval.override_of(&session), Some(ApprovalPolicy::Ask));
    assert_eq!(
        approval
            .request(request(session.clone()))
            .await
            .expect("ask request"),
        ApprovalOutcome::AllowedOnce
    );
    set_approval_policy(&session, ApprovalPolicy::Never).expect("never override");
    assert_eq!(
        approval
            .request(request(session))
            .await
            .expect("never request"),
        ApprovalOutcome::Rejected
    );
}

#[test]
fn live_policy_switch_appends_once_and_queues_one_notice() {
    let context = Context::new();
    let service = ApprovalService::new(context, ApprovalConfig::default());
    let session = session();
    let injected = Arc::new(Mutex::new(Vec::<UserMessage>::new()));
    let first = injected.clone();
    service
        .set_policy_with_inject(&session, ApprovalPolicy::Never, move |message| {
            first.lock().push(message);
            Ok(())
        })
        .expect("switch");
    let second = injected.clone();
    service
        .set_policy_with_inject(&session, ApprovalPolicy::Never, move |message| {
            second.lock().push(message);
            Ok(())
        })
        .expect("idempotent switch");
    assert_eq!(
        effective_approval_policy(&session.events()),
        Some(ApprovalPolicy::Never)
    );
    let injected = injected.lock();
    assert_eq!(injected.len(), 1);
    assert_eq!(
        injected[0].content(),
        [ContentBlock::Text {
            text: "The approval policy changed from \"ask\" to \"never\" (changed by the user)."
                .to_owned(),
        }]
    );
    assert_eq!(
        injected[0].source(),
        &MessageSource::plugin("user-approval")
    );
}

async fn policy_context(
    prompt: &seekdeep_system_prompt::SystemPrompt,
    session: Option<Arc<Session>>,
) -> Option<String> {
    prompt
        .assemble(AssembleContext {
            agent_session: session,
            ..AssembleContext::default()
        })
        .await
        .expect("assemble")
        .contexts
        .into_iter()
        .find(|context| context.name == "approval:policy")
        .map(|context| context.text)
}

#[tokio::test]
async fn prompt_context_contributes_complete_current_ask_or_never_policy() {
    let context = Context::new();
    let prompt = install_prompt(&context, SystemPromptConfig::default()).expect("prompt");
    let _approval = install(&context, ApprovalConfig::default()).expect("approval");
    let ask = session();
    let never = session();
    set_approval_policy(&never, ApprovalPolicy::Never).expect("never");
    assert_eq!(
        policy_context(&prompt, Some(ask)).await.as_deref(),
        Some(ASK_SENTENCE)
    );
    assert_eq!(
        policy_context(&prompt, Some(never)).await.as_deref(),
        Some(NEVER_SENTENCE)
    );
    assert_eq!(policy_context(&prompt, None).await.as_deref(), Some(""));
}

#[tokio::test]
async fn prompt_context_tracks_latest_durable_switch_and_is_byte_stable() {
    let context = Context::new();
    let prompt = install_prompt(&context, SystemPromptConfig::default()).expect("prompt");
    let _approval = install(&context, ApprovalConfig::default()).expect("approval");
    let session = session();
    assert_eq!(
        policy_context(&prompt, Some(session.clone())).await,
        policy_context(&prompt, Some(session.clone())).await
    );
    set_approval_policy(&session, ApprovalPolicy::Never).expect("never");
    set_approval_policy(&session, ApprovalPolicy::Ask).expect("ask");
    set_approval_policy(&session, ApprovalPolicy::Never).expect("never again");
    assert_eq!(
        policy_context(&prompt, Some(session.clone()))
            .await
            .as_deref(),
        Some(NEVER_SENTENCE)
    );
    assert_eq!(
        policy_context(&prompt, Some(session)).await.as_deref(),
        Some(NEVER_SENTENCE)
    );
}

#[tokio::test]
async fn disposing_service_removes_its_prompt_context_contribution() {
    let context = Context::new();
    let prompt = install_prompt(&context, SystemPromptConfig::default()).expect("prompt");
    let approval = install(&context, ApprovalConfig::default()).expect("approval");
    let session = session();
    assert!(
        policy_context(&prompt, Some(session.clone()))
            .await
            .is_some()
    );
    approval.dispose().await.expect("dispose");
    assert!(policy_context(&prompt, Some(session)).await.is_none());
}

#[tokio::test]
async fn hostile_answer_variant_still_normalizes_under_ask_policy() {
    let context = Context::new();
    let approval = install(&context, ApprovalConfig::default()).expect("approval");
    approval
        .on_request(
            &context,
            |_, _| async { Ok(ApprovalAnswer::Unknown("always".to_owned())) },
            EventOptions::default(),
        )
        .expect("hostile answerer");
    assert_eq!(
        approval.request(request(session())).await.expect("request"),
        ApprovalOutcome::Unavailable
    );
}

#[tokio::test]
async fn loader_approval_is_available_without_a_system_prompt_provider() {
    let context = Context::new();
    let fiber = context
        .plugin(seekdeep_user_approval::plugin(), json!({}))
        .unwrap();
    fiber.await_settled().await.unwrap();
    let approval = context
        .get(seekdeep_user_approval::APPROVAL)
        .expect("approval is not contingent on prompt presentation");
    assert_eq!(
        approval.request(request(session())).await.unwrap(),
        ApprovalOutcome::Unavailable
    );
    fiber.dispose().await.unwrap();
}

#[tokio::test]
async fn optional_prompt_contribution_tracks_late_provider_replacement_and_owner_teardown() {
    let context = Context::new();
    let approval = install(&context, ApprovalConfig::default()).unwrap();
    let exact_service = context.get(seekdeep_user_approval::APPROVAL).unwrap();
    let first_owner = seekdeep_cordis::Fiber::active_child("first-prompt");
    let first = install_prompt(
        &context.with_fiber(first_owner.clone()),
        SystemPromptConfig::default(),
    )
    .unwrap();
    assert_eq!(
        policy_context(&first, Some(session())).await.as_deref(),
        Some(ASK_SENTENCE)
    );
    first_owner.dispose().await.unwrap();
    assert!(policy_context(&first, Some(session())).await.is_none());
    assert!(Arc::ptr_eq(
        &exact_service,
        &context.get(seekdeep_user_approval::APPROVAL).unwrap()
    ));
    let second_owner = seekdeep_cordis::Fiber::active_child("second-prompt");
    let second = install_prompt(
        &context.with_fiber(second_owner.clone()),
        SystemPromptConfig::default(),
    )
    .unwrap();
    assert_eq!(
        policy_context(&second, Some(session())).await.as_deref(),
        Some(ASK_SENTENCE)
    );
    approval.dispose().await.unwrap();
    assert!(policy_context(&second, Some(session())).await.is_none());
    second_owner.dispose().await.unwrap();
    let last = install_prompt(&context, SystemPromptConfig::default()).unwrap();
    assert!(policy_context(&last, Some(session())).await.is_none());
}

#[test]
fn prompt_change_observers_can_publish_an_unrelated_service_reentrantly() {
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let context = Context::new();
        let _prompt = install_prompt(&context, SystemPromptConfig::default()).unwrap();
        let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let notified = observed.clone();
        let publisher = context.clone();
        context
            .events()
            .on_sync(
                &context,
                "system-prompt/change",
                move |_, _| {
                    if !notified.swap(true, Ordering::SeqCst) {
                        publisher.provide(
                            seekdeep_cordis::ServiceKey::new("prompt-observer-value"),
                            Arc::new(42_u32),
                        )?;
                    }
                    Ok(seekdeep_cordis::EventReply::Undefined)
                },
                EventOptions::default(),
            )
            .unwrap();
        let approval = install(&context, ApprovalConfig::default()).unwrap();
        assert!(observed.load(Ordering::SeqCst));
        futures::executor::block_on(approval.dispose()).unwrap();
        send.send(()).unwrap();
    });
    receive
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("prompt registration deadlocked a reentrant service notification");
}
