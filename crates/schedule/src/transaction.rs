//! Agent-scoped serialization for Schedule reads and durable mutations.

use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
};

use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_agent::Agent;

static TAILS: LazyLock<Mutex<HashMap<usize, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Runs one complete Schedule transaction after its exact agent's prior
/// transaction settles.
///
/// Each agent is serialized independently; the source's `WeakMap` key maps to the
/// agent pointer identity, so a long-lived process retains one mutex slot per
/// agent it has ever serialized.
pub async fn run_schedule_transaction<T, F>(agent: Arc<Agent>, operation: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> BoxFuture<'static, T> + Send + 'static,
{
    let key = Arc::as_ptr(&agent) as usize;
    let lock = {
        let mut tails = TAILS.lock();
        tails
            .entry(key)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    let _guard = lock.lock().await;
    operation().await
}

#[cfg(test)]
mod tests {
    use seekdeep_agent::{AgentOptions, Inbox, NoopInboxNotifications};
    use seekdeep_cordis::Context;
    use seekdeep_core::session::{Session, SessionId};
    use seekdeep_scope::ScopeKey;

    use super::*;

    fn agent(id: &str) -> Arc<Agent> {
        let session = Session::create(&SessionId::new(id), None, None).expect("session");
        let inbox = Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).expect("inbox");
        Arc::new(Agent::new(
            SessionId::new(id),
            AgentOptions::default(),
            session,
            Arc::new(inbox),
            Context::new(),
            ScopeKey::new(),
        ))
    }

    #[tokio::test]
    async fn serializes_transactions_for_one_agent() {
        let agent = agent("s");
        let order = Arc::new(parking_lot::Mutex::new(Vec::new()));

        let first = {
            let order = order.clone();
            let agent = agent.clone();
            tokio::spawn(run_schedule_transaction(agent.clone(), move || {
                Box::pin(async move {
                    order.lock().push("first-start");
                    tokio::task::yield_now().await;
                    order.lock().push("first-end");
                })
            }))
        };
        let second = {
            let order = order.clone();
            let agent = agent.clone();
            tokio::spawn(run_schedule_transaction(agent.clone(), move || {
                Box::pin(async move {
                    order.lock().push("second-start");
                    order.lock().push("second-end");
                })
            }))
        };

        first.await.expect("first");
        second.await.expect("second");
        let observed = order.lock().clone();
        assert_eq!(
            observed,
            ["first-start", "first-end", "second-start", "second-end"]
        );
    }
}
