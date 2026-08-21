//! Agent-subject event dispatch coupled to exact scope routing.

use std::{any::Any, future::Future, sync::Arc};

use seekdeep_cordis::{Context, EventArgs, EventReply, events::ListenerFuture};
use seekdeep_llm::AbortSignal;
use seekdeep_scope::{scope_target, scoped_event_args};
use seekdeep_system_prompt::AssembleContext;

use crate::Agent;

/// One agent-subject event with the subject injected by the dispatcher.
#[derive(Clone, Debug)]
pub struct AgentEvent<T> {
    /// Exact agent subject and scope key.
    pub agent: Arc<Agent>,
    /// Event-specific payload fields.
    pub payload: T,
}

/// Reusable dispatcher coupling one agent subject to its scope carrier.
#[derive(Clone, Debug)]
pub struct AgentEvents {
    context: Context,
    agent: Arc<Agent>,
}

impl AgentEvents {
    /// Builds a dispatcher for one exact agent.
    #[must_use]
    pub fn new(context: Context, agent: Arc<Agent>) -> Self {
        Self { context, agent }
    }

    /// Emits a non-vetoing notification and contains every observer failure.
    pub fn emit<T>(&self, name: &str, payload: T)
    where
        T: Any + Send + Sync,
    {
        let args = scoped_event_args(
            self.agent.scope_key(),
            EventArgs::one(AgentEvent {
                agent: self.agent.clone(),
                payload,
            }),
        );
        let dispatch = scope_target(&self.context, Some(self.agent.scope_key()));
        match self.context.events().prepare_emit(&dispatch, name, &args) {
            Ok(emission) => emission.emit_contained(|error| {
                tracing::warn!(event = name, %error, "agent event listener failed");
            }),
            Err(error) => {
                tracing::warn!(event = name, %error, "agent event dispatch failed");
            }
        }
    }

    /// Runs selected listeners in order until one bails.
    ///
    /// # Errors
    ///
    /// Returns listener or dispatch-interception failures.
    pub async fn serial<T>(&self, name: &str, payload: T) -> anyhow::Result<EventReply>
    where
        T: Any + Send + Sync,
    {
        let args = scoped_event_args(
            self.agent.scope_key(),
            EventArgs::one(AgentEvent {
                agent: self.agent.clone(),
                payload,
            }),
        );
        self.context
            .events()
            .serial(
                &scope_target(&self.context, Some(self.agent.scope_key())),
                name,
                &args,
            )
            .await
    }

    /// Runs typed around middleware over an agent-subject payload.
    ///
    /// # Errors
    ///
    /// Returns middleware, inner-operation, or reply-type failures.
    pub async fn waterfall<T, R, F, Fut>(
        &self,
        name: &str,
        payload: T,
        inner: F,
    ) -> anyhow::Result<R>
    where
        T: Any + Send + Sync,
        R: Any + Send + Sync + Clone,
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = anyhow::Result<R>> + Send + 'static,
    {
        let args = scoped_event_args(
            self.agent.scope_key(),
            EventArgs::one(AgentEvent {
                agent: self.agent.clone(),
                payload,
            }),
        );
        let reply = self
            .context
            .events()
            .waterfall(
                &scope_target(&self.context, Some(self.agent.scope_key())),
                name,
                &args,
                move || -> ListenerFuture {
                    Box::pin(async move { Ok(EventReply::Value(Arc::new(inner().await?))) })
                },
            )
            .await?;
        reply
            .downcast::<R>()
            .map(|value| (*value).clone())
            .ok_or_else(|| anyhow::anyhow!("agent event {name:?} returned an invalid value"))
    }
}

/// Builds a prompt assembly context with the agent scope and signal coupled.
#[must_use]
pub fn assemble_context_for(agent: &Agent, signal: Option<AbortSignal>) -> AssembleContext {
    AssembleContext {
        scope: Some(agent.scope_key()),
        signal,
        agent_session: Some(agent.session().clone()),
        ..AssembleContext::default()
    }
}
