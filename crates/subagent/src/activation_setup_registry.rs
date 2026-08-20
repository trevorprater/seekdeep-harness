//! Internal registry of deployment capabilities composed into every
//! continuable child's unpublished creation context.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use parking_lot::Mutex;
use seekdeep_agent::AgentSetupCommit;
use seekdeep_cordis::{Context, fiber::EffectHandle};

use crate::error::SubagentError;

/// One deployment capability installed into a continuable child's unpublished
/// creation context.
pub type ContinuableSetupContribution =
    Arc<dyn Fn(&Context) -> Box<dyn Fn() + Send + Sync> + Send + Sync>;

struct Registration {
    contribution: ContinuableSetupContribution,
    removed: AtomicBool,
    installations: Mutex<HashSet<usize>>,
}

struct Installation {
    registration_id: usize,
    released: AtomicBool,
    dispose: Box<dyn Fn() + Send + Sync>,
}

struct Transaction {
    installations: Vec<usize>,
    invalidated: bool,
}

/// Owns continuable-child setup registrations, installations, rollback, child
/// cleanup, and immediate live revocation.
pub struct SubagentActivationSetupRegistry {
    registrations: Mutex<HashMap<usize, Arc<Registration>>>,
    installations: Mutex<HashMap<usize, Arc<Installation>>>,
    by_child: Mutex<HashMap<usize, HashSet<usize>>>,
    next_id: AtomicUsize,
}

impl SubagentActivationSetupRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            registrations: Mutex::new(HashMap::new()),
            installations: Mutex::new(HashMap::new()),
            by_child: Mutex::new(HashMap::new()),
            next_id: AtomicUsize::new(1),
        })
    }

    fn alloc_id(&self) -> usize {
        self.next_id.fetch_add(1, Ordering::AcqRel)
    }

    /// Registers one contribution and returns an idempotent undo.
    pub fn register(
        self: &Arc<Self>,
        contribution: ContinuableSetupContribution,
    ) -> impl Fn() + Send + Sync {
        let id = self.alloc_id();
        self.registrations.lock().insert(
            id,
            Arc::new(Registration {
                contribution,
                removed: AtomicBool::new(false),
                installations: Mutex::new(HashSet::new()),
            }),
        );
        let registry = Arc::downgrade(self);
        move || {
            let Some(registry) = registry.upgrade() else {
                return;
            };
            let Some(registration) = registry.registrations.lock().get(&id).cloned() else {
                return;
            };
            if registration.removed.swap(true, Ordering::AcqRel) {
                return;
            }
            registry.registrations.lock().remove(&id);
            let install_ids: Vec<usize> = registration.installations.lock().drain().collect();
            for iid in install_ids {
                registry.release(iid);
            }
        }
    }

    /// Installs every live contribution into one unpublished child context.
    ///
    /// # Errors
    ///
    /// Returns when an installer throws.
    pub fn apply(
        self: &Arc<Self>,
        child_ctx: &Context,
    ) -> anyhow::Result<Arc<dyn AgentSetupCommit>> {
        let child_key = std::ptr::from_ref(child_ctx) as usize;
        let mut transaction = Transaction {
            installations: Vec::new(),
            invalidated: false,
        };
        let registration_ids: Vec<usize> = self.registrations.lock().keys().copied().collect();
        for rid in registration_ids {
            let Some(registration) = self.registrations.lock().get(&rid).cloned() else {
                continue;
            };
            if registration.removed.load(Ordering::Acquire) {
                continue;
            }
            let result = (registration.contribution)(child_ctx);
            let iid = self.alloc_id();
            self.installations.lock().insert(
                iid,
                Arc::new(Installation {
                    registration_id: rid,
                    released: AtomicBool::new(false),
                    dispose: result,
                }),
            );
            registration.installations.lock().insert(iid);
            self.by_child
                .lock()
                .entry(child_key)
                .or_default()
                .insert(iid);
            transaction.installations.push(iid);
            if registration.removed.load(Ordering::Acquire) {
                self.release(iid);
            }
        }

        let registry = Arc::downgrade(self);
        let effect = EffectHandle::synchronous("subagents.activationSetup()", move || {
            if let Some(registry) = registry.upgrade() {
                let install_ids: Vec<usize> = registry
                    .by_child
                    .lock()
                    .get(&child_key)
                    .map(|set| set.iter().copied().collect())
                    .unwrap_or_default();
                for iid in install_ids {
                    registry.release(iid);
                }
            }
            Ok(())
        });
        child_ctx.own(effect)?;

        Ok(Arc::new(SetupCommit { transaction }))
    }

    fn release(&self, iid: usize) {
        let Some(installation) = self.installations.lock().get(&iid).cloned() else {
            return;
        };
        if installation.released.swap(true, Ordering::AcqRel) {
            return;
        }
        self.installations.lock().remove(&iid);
        if let Some(registration) = self.registrations.lock().get(&installation.registration_id) {
            registration.installations.lock().remove(&iid);
        }
        let mut children = self.by_child.lock();
        let mut empty_keys = Vec::new();
        for (key, set) in children.iter_mut() {
            if set.remove(&iid) && set.is_empty() {
                empty_keys.push(*key);
            }
        }
        for key in empty_keys {
            children.remove(&key);
        }
        (installation.dispose)();
    }
}

struct SetupCommit {
    transaction: Transaction,
}

impl AgentSetupCommit for SetupCommit {
    fn commit(&self) -> anyhow::Result<()> {
        if self.transaction.invalidated {
            return Err(SubagentError::new(
                "a continuable-subagent setup contribution was revoked while this child was being built; the child was not established",
                "ACTIVATION_SETUP_REVOKED",
            )
            .into());
        }
        Ok(())
    }
}
