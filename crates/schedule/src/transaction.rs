//! Agent-scoped serialization for Schedule reads and durable mutations.

use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Weak},
};

use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_agent::Agent;

/// One serialization slot per live agent, keyed by pointer identity and paired with a
/// weak handle so a slot whose agent is gone is dropped before the next lookup (the
/// source's `WeakMap` semantics), including one whose address a new agent reuses.
type Slot = (Weak<Agent>, Arc<tokio::sync::Mutex<()>>);

static TAILS: LazyLock<Mutex<HashMap<usize, Slot>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Runs one complete Schedule transaction after its exact agent's prior
/// transaction settles.
///
/// Each agent is serialized independently; the source's `WeakMap` key maps to the
/// agent pointer identity, and slots whose agents were dropped are retired on the
/// next transaction, so a long-lived process holds one slot per live agent only.
pub async fn run_schedule_transaction<T, F>(agent: Arc<Agent>, operation: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> BoxFuture<'static, T> + Send + 'static,
{
    let lock = slot_for(&mut TAILS.lock(), &agent);
    let _guard = lock.lock().await;
    operation().await
}

/// Retires the slots of dropped agents, then returns the exact agent's slot, creating it
/// on first use.
fn slot_for(tails: &mut HashMap<usize, Slot>, agent: &Arc<Agent>) -> Arc<tokio::sync::Mutex<()>> {
    tails.retain(|_, (owner, _)| owner.strong_count() > 0);
    tails
        .entry(Arc::as_ptr(agent) as usize)
        .or_insert_with(|| (Arc::downgrade(agent), Arc::new(tokio::sync::Mutex::new(()))))
        .1
        .clone()
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

    #[test]
    fn slots_retire_with_their_agents() {
        let mut tails = HashMap::new();
        let first = agent("retire-first");
        let first_slot = super::slot_for(&mut tails, &first);
        assert!(Arc::ptr_eq(
            &super::slot_for(&mut tails, &first),
            &first_slot
        ));
        assert_eq!(tails.len(), 1);
        drop(first);
        let second = agent("retire-second");
        let second_slot = super::slot_for(&mut tails, &second);
        assert_eq!(tails.len(), 1, "the dropped agent's slot is gone");
        assert!(!Arc::ptr_eq(&second_slot, &first_slot));
        drop(second);
        let third = agent("retire-third");
        super::slot_for(&mut tails, &third);
        assert_eq!(tails.len(), 1);
    }
}
