//! Package-owned live Agent status invariants.

use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use seekdeep_cordis::{EventOptions, EventReply};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

use crate::{Agent, AgentEvent, AgentStatus, AgentStatusChanged};

const PACKAGE_NAME: &str = "@seekdeep-ai/seekdeep-agent";

/// Cordis invariant companion plugin name.
pub const INVARIANT_NAME: &str = "agent-invariant";
/// Service required before the companion can register.
pub const INVARIANT_INJECT: &[&str] = &["invariants"];

#[derive(Debug)]
struct LastStatus {
    agent: Weak<Agent>,
    status: AgentStatus,
}

/// Registers the per-Agent status-transition invariant.
///
/// # Errors
///
/// Returns ordinary invariant registration or installer failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        PACKAGE_NAME,
        InvariantInstaller::new(
            std::iter::empty::<String>(),
            |context, failure| async move {
                let statuses = Arc::new(Mutex::new(Vec::<LastStatus>::new()));
                context.events().on_sync(
                    &context,
                    "agent/status",
                    move |_, args| {
                        let event = args
                            .get::<AgentEvent<AgentStatusChanged>>(0)
                            .ok_or_else(|| anyhow::anyhow!("agent/status lacks its Agent event"))?;
                        let mut statuses = statuses.lock();
                        statuses.retain(|entry| entry.agent.strong_count() > 0);
                        if let Some(previous) = statuses.iter_mut().find(|entry| {
                            entry
                                .agent
                                .upgrade()
                                .is_some_and(|agent| Arc::ptr_eq(&agent, &event.agent))
                        }) {
                            if previous.status == event.payload.status {
                                return Err(failure
                                    .fail(format!(
                                        "agent/status repeated {} (no-op transition)",
                                        status_name(event.payload.status)
                                    ))
                                    .into());
                            }
                            previous.status = event.payload.status;
                        } else {
                            statuses.push(LastStatus {
                                agent: Arc::downgrade(&event.agent),
                                status: event.payload.status,
                            });
                        }
                        Ok(EventReply::Undefined)
                    },
                    EventOptions {
                        global: true,
                        ..EventOptions::default()
                    },
                )?;
                Ok(())
            },
        ),
    )
}

const fn status_name(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Idle => "idle",
        AgentStatus::Running => "running",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use seekdeep_cordis::{Context, EventArgs, EventOptions, EventReply};
    use seekdeep_core::session::{Session, SessionId};
    use seekdeep_invariants::InvariantConfig;
    use seekdeep_scope::{ScopeKey, scope_target};

    use crate::{AgentOptions, Inbox, NoopInboxNotifications};

    use super::*;

    fn agent(context: &Context, id: &str) -> Arc<Agent> {
        let id = SessionId::new(id);
        let session = Session::create(&id, None, None).unwrap();
        let inbox =
            Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
        Arc::new(Agent::new(
            id,
            AgentOptions::default(),
            session,
            inbox,
            context.clone(),
            ScopeKey::new(),
        ))
    }

    fn emit(context: &Context, agent: &Arc<Agent>, status: AgentStatus) -> anyhow::Result<()> {
        context.events().emit(
            &scope_target(context, Some(agent.scope_key())),
            "agent/status",
            &EventArgs::one(AgentEvent {
                agent: agent.clone(),
                payload: AgentStatusChanged { status },
            }),
        )
    }

    #[tokio::test]
    async fn accepts_real_transitions_rejects_repeats_and_tracks_exact_agents() {
        assert_eq!(INVARIANT_NAME, "agent-invariant");
        assert_eq!(INVARIANT_INJECT, ["invariants"]);
        let context = Context::new();
        let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
        let registration = register_invariant(&registry).unwrap();
        registration.await_ready().await.unwrap();
        let first = agent(&context, "first");
        let second = agent(&context, "second");

        emit(&context, &first, AgentStatus::Idle).unwrap();
        emit(&context, &first, AgentStatus::Running).unwrap();
        emit(&context, &first, AgentStatus::Idle).unwrap();
        emit(&context, &second, AgentStatus::Idle).unwrap();
        let error = emit(&context, &first, AgentStatus::Idle).unwrap_err();
        assert_eq!(
            error.to_string(),
            "invariant violated by \"@seekdeep-ai/seekdeep-agent\": agent/status repeated idle (no-op transition)"
        );

        registration.dispose().await.unwrap();
        emit(&context, &first, AgentStatus::Idle).unwrap();
    }

    #[tokio::test]
    async fn agent_dispatcher_carries_the_payload_subject_through_scope_validation() {
        let context = Context::new();
        let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
        let scope_registration = seekdeep_scope::invariant::register_invariant(&registry).unwrap();
        scope_registration.await_ready().await.unwrap();
        let seen = Arc::new(AtomicBool::new(false));
        let observed = seen.clone();
        context
            .events()
            .on_sync(
                &context,
                "agent/status",
                move |_, _| {
                    observed.store(true, Ordering::Release);
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )
            .unwrap();
        let agent = agent(&context, "subject-carrier");
        crate::AgentEvents::new(context.clone(), agent).emit(
            "agent/status",
            AgentStatusChanged {
                status: AgentStatus::Running,
            },
        );
        assert!(seen.load(Ordering::Acquire));
        scope_registration.dispose().await.unwrap();
    }
}
