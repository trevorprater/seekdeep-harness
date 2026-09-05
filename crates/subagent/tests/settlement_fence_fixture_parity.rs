//! Deterministic ordering fixture for continuable-child settlement delivery.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use seekdeep_agent::{
    Agent, AgentEvent, AgentEvents, AgentOptions, Inbox, NoopInboxNotifications, PreStepDecision,
};
use seekdeep_agent_loop::{AgentInboxMessage, AgentPreStepEvent};
use seekdeep_cordis::{Context, EventOptions, EventReply, Plugin};
use seekdeep_core::session::{Session, SessionHeader, SessionId};
use seekdeep_llm::{AbortSignal, ContentBlock, MessageSource, UserMessage};
use seekdeep_scope::ScopeKey;
use tokio::sync::Notify;

const fn global_events() -> EventOptions {
    EventOptions {
        global: true,
        prepend: false,
    }
}

fn settlement_fence_plugin(entered: Arc<Notify>) -> Plugin {
    Plugin::new(
        "subagent-settlement-fence",
        std::iter::empty::<&str>(),
        move |context, _| {
            let entered = entered.clone();
            Box::pin(async move {
                let delivered = Arc::new(Notify::new());
                let has_delivered = Arc::new(AtomicBool::new(false));

                let inbox_delivered = delivered.clone();
                let inbox_has_delivered = has_delivered.clone();
                context.events().on_sync(
                    &context,
                    "agent/inbox/inserted",
                    move |_, args| {
                        let event =
                            args.get::<AgentEvent<AgentInboxMessage>>(0)
                                .ok_or_else(|| {
                                    anyhow::anyhow!("agent/inbox/inserted lacks its event")
                                })?;
                        if event.agent.session().header().parent_session.is_none()
                            && event.payload.message.source().kind == "subagent-settled"
                        {
                            inbox_has_delivered.store(true, Ordering::Release);
                            inbox_delivered.notify_waiters();
                        }
                        Ok(EventReply::Undefined)
                    },
                    global_events(),
                )?;

                context.events().on_waterfall(
                    &context,
                    "agent/pre-step",
                    move |_, args, next| {
                        let delivered = delivered.clone();
                        let has_delivered = has_delivered.clone();
                        let entered = entered.clone();
                        Box::pin(async move {
                            let event = args
                                .get::<AgentEvent<AgentPreStepEvent>>(0)
                                .ok_or_else(|| anyhow::anyhow!("agent/pre-step lacks its event"))?;
                            if event.agent.session().header().parent_session.is_none()
                                && event.payload.turn == 1
                                && event.payload.step == 2
                                && !has_delivered.load(Ordering::Acquire)
                            {
                                entered.notify_one();
                                loop {
                                    let notified = delivered.notified();
                                    if has_delivered.load(Ordering::Acquire) {
                                        break;
                                    }
                                    notified.await;
                                }
                            }
                            next.run().await
                        })
                    },
                    global_events(),
                )?;
                Ok(())
            })
        },
    )
}

fn agent(context: &Context, id: &str, parent_session: Option<SessionId>) -> Arc<Agent> {
    let id = SessionId::new(id);
    let mut header = SessionHeader::new(id.clone());
    header.parent_session = parent_session;
    let session = Session::create(&id, None, Some(header)).unwrap();
    let inbox = Arc::new(
        Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).expect("agent inbox"),
    );
    Arc::new(Agent::new(
        id,
        AgentOptions::default(),
        session,
        inbox,
        context.clone(),
        ScopeKey::new(),
    ))
}

fn message(kind: &str) -> UserMessage {
    UserMessage::new(
        vec![ContentBlock::Text {
            text: kind.to_owned(),
        }],
        MessageSource {
            kind: kind.to_owned(),
            fields: serde_json::Map::new(),
        },
    )
}

async fn parent_step_two(context: Context, parent: Arc<Agent>) -> PreStepDecision {
    AgentEvents::new(context, parent)
        .waterfall(
            "agent/pre-step",
            AgentPreStepEvent {
                messages: Vec::new(),
                turn: 1,
                step: 2,
                signal: AbortSignal::default(),
            },
            || async {
                Ok(PreStepDecision::Enter {
                    messages: Vec::new(),
                })
            },
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn parent_second_step_waits_only_for_its_settlement_notice_and_disposes_exactly() {
    let context = Context::new();
    let entered = Arc::new(Notify::new());
    let mounted = context
        .plugin(
            settlement_fence_plugin(entered.clone()),
            serde_json::Value::Null,
        )
        .unwrap();
    mounted.await_settled().await.unwrap();
    assert_eq!(
        context
            .events()
            .listener_count(&context, "agent/inbox/inserted"),
        1
    );
    let parent = agent(&context, "parent", None);
    let child = agent(
        &context,
        "child",
        Some(parent.session().header().id.clone()),
    );
    let pending = tokio::spawn(parent_step_two(context.clone(), parent.clone()));
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("parent pre-step entered the fence");
    assert!(!pending.is_finished());

    AgentEvents::new(context.clone(), child).emit(
        "agent/inbox/inserted",
        AgentInboxMessage {
            message: message("subagent-settled"),
        },
    );
    AgentEvents::new(context.clone(), parent.clone()).emit(
        "agent/inbox/inserted",
        AgentInboxMessage {
            message: message("user"),
        },
    );
    tokio::task::yield_now().await;
    assert!(!pending.is_finished());

    AgentEvents::new(context.clone(), parent.clone()).emit(
        "agent/inbox/inserted",
        AgentInboxMessage {
            message: message("subagent-settled"),
        },
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), pending)
            .await
            .expect("settlement notice releases the fence")
            .unwrap(),
        PreStepDecision::Enter {
            messages: Vec::new()
        }
    );
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(1),
            parent_step_two(context.clone(), parent),
        )
        .await
        .expect("delivered fence stays open"),
        PreStepDecision::Enter {
            messages: Vec::new()
        }
    );

    mounted.dispose().await.unwrap();
    assert_eq!(
        context
            .events()
            .listener_count(&context, "agent/inbox/inserted"),
        0
    );
    context.root_fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn disposing_an_undelivered_fence_removes_its_pre_step_barrier() {
    let context = Context::new();
    let mounted = context
        .plugin(
            settlement_fence_plugin(Arc::new(Notify::new())),
            serde_json::Value::Null,
        )
        .unwrap();
    mounted.await_settled().await.unwrap();
    mounted.dispose().await.unwrap();
    let parent = agent(&context, "after-disposal", None);
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(1),
            parent_step_two(context.clone(), parent),
        )
        .await
        .expect("disposed waterfall cannot hold the next pre-step"),
        PreStepDecision::Enter {
            messages: Vec::new()
        }
    );
    assert_eq!(
        context
            .events()
            .listener_count(&context, "agent/inbox/inserted"),
        0
    );
    context.root_fiber().dispose().await.unwrap();
}
