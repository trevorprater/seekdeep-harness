//! Ordered reversible capabilities composed into continuable child activations.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use indexmap::{IndexMap, IndexSet};
use parking_lot::Mutex;
use seekdeep_agent::AgentSetupCommit;
use seekdeep_cordis::{Context, fiber::EffectHandle};

use crate::error::SubagentError;

/// One deployment capability installed into a continuable child's unpublished context.
pub type ContinuableSetupContribution =
    Arc<dyn Fn(&Context) -> anyhow::Result<EffectHandle> + Send + Sync>;

struct Registration {
    contribution: ContinuableSetupContribution,
    removed: AtomicBool,
    installations: Mutex<IndexSet<usize>>,
}

struct Installation {
    registration_id: usize,
    child_id: usize,
    invalidated: Arc<AtomicBool>,
    released: AtomicBool,
    effect: EffectHandle,
}

/// Owns setup registrations, provisioning rollback, child cleanup, and live revocation.
pub struct SubagentActivationSetupRegistry {
    registrations: Mutex<IndexMap<usize, Arc<Registration>>>,
    installations: Mutex<IndexMap<usize, Arc<Installation>>>,
    by_child: Mutex<IndexMap<usize, IndexSet<usize>>>,
    next_id: AtomicUsize,
}

impl SubagentActivationSetupRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            registrations: Mutex::new(IndexMap::new()),
            installations: Mutex::new(IndexMap::new()),
            by_child: Mutex::new(IndexMap::new()),
            next_id: AtomicUsize::new(1),
        })
    }

    fn alloc_id(&self) -> usize {
        self.next_id.fetch_add(1, Ordering::AcqRel)
    }

    /// Registers one contribution and returns its idempotent fallible revoker.
    #[must_use]
    pub fn register(self: &Arc<Self>, contribution: ContinuableSetupContribution) -> EffectHandle {
        let id = self.alloc_id();
        self.registrations.lock().insert(
            id,
            Arc::new(Registration {
                contribution,
                removed: AtomicBool::new(false),
                installations: Mutex::new(IndexSet::new()),
            }),
        );
        let registry = Arc::downgrade(self);
        EffectHandle::new("subagents.registerContinuableSetup()", move || {
            let registry = registry.clone();
            Box::pin(async move {
                let Some(registry) = registry.upgrade() else {
                    return Ok(());
                };
                let Some(registration) = registry.registrations.lock().get(&id).cloned() else {
                    return Ok(());
                };
                if registration.removed.swap(true, Ordering::AcqRel) {
                    return Ok(());
                }
                registry.registrations.lock().shift_remove(&id);
                let ids = registration
                    .installations
                    .lock()
                    .drain(..)
                    .collect::<Vec<_>>();
                registry.release_all(ids).await
            })
        })
    }

    /// Installs every live contribution into one unpublished child context.
    ///
    /// # Errors
    ///
    /// Returns the installer or ownership failure after rolling back all earlier
    /// installations; cleanup failures are aggregated with the primary error.
    pub fn apply(
        self: &Arc<Self>,
        child_context: &Context,
    ) -> anyhow::Result<Arc<dyn AgentSetupCommit>> {
        let child_id = self.alloc_id();
        let invalidated = Arc::new(AtomicBool::new(false));
        let registration_ids = self
            .registrations
            .lock()
            .keys()
            .copied()
            .collect::<Vec<_>>();
        let mut installed = Vec::new();
        for registration_id in registration_ids {
            let Some(registration) = self.registrations.lock().get(&registration_id).cloned()
            else {
                continue;
            };
            if registration.removed.load(Ordering::Acquire) {
                continue;
            }
            let effect = match (registration.contribution)(child_context) {
                Ok(effect) => effect,
                Err(error) => return Err(self.rollback_sync(installed, error)),
            };
            let installation_id = self.alloc_id();
            self.installations.lock().insert(
                installation_id,
                Arc::new(Installation {
                    registration_id,
                    child_id,
                    invalidated: Arc::clone(&invalidated),
                    released: AtomicBool::new(false),
                    effect,
                }),
            );
            registration.installations.lock().insert(installation_id);
            self.by_child
                .lock()
                .entry(child_id)
                .or_default()
                .insert(installation_id);
            installed.push(installation_id);
            if registration.removed.load(Ordering::Acquire)
                && let Err(error) = futures::executor::block_on(self.release(installation_id))
            {
                return Err(self.rollback_sync(installed, error));
            }
        }

        let registry = Arc::downgrade(self);
        let cleanup = EffectHandle::new("subagents.activationSetup()", move || {
            let registry = registry.clone();
            Box::pin(async move {
                let Some(registry) = registry.upgrade() else {
                    return Ok(());
                };
                let ids = registry
                    .by_child
                    .lock()
                    .get(&child_id)
                    .map(|ids| ids.iter().copied().collect())
                    .unwrap_or_default();
                registry.release_all(ids).await
            })
        });
        if let Err(error) = child_context.own(cleanup) {
            return Err(self.rollback_sync(installed, error.into()));
        }
        Ok(Arc::new(SetupCommit { invalidated }))
    }

    fn rollback_sync(&self, ids: Vec<usize>, primary: anyhow::Error) -> anyhow::Error {
        match futures::executor::block_on(self.release_all(ids)) {
            Ok(()) => primary,
            Err(cleanup) => anyhow::anyhow!("{primary:#}; setup rollback failed: {cleanup:#}"),
        }
    }

    async fn release_all(&self, ids: Vec<usize>) -> anyhow::Result<()> {
        let mut failures = Vec::new();
        for id in ids {
            if let Err(error) = self.release(id).await {
                failures.push(format!("{error:#}"));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "failed to release {} installation(s): {}",
                failures.len(),
                failures.join("; ")
            ))
        }
    }

    async fn release(&self, installation_id: usize) -> anyhow::Result<()> {
        let Some(installation) = self.installations.lock().get(&installation_id).cloned() else {
            return Ok(());
        };
        if installation.released.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        installation.invalidated.store(true, Ordering::Release);
        self.installations.lock().shift_remove(&installation_id);
        if let Some(registration) = self.registrations.lock().get(&installation.registration_id) {
            registration
                .installations
                .lock()
                .shift_remove(&installation_id);
        }
        {
            let mut children = self.by_child.lock();
            if let Some(ids) = children.get_mut(&installation.child_id) {
                ids.shift_remove(&installation_id);
                if ids.is_empty() {
                    children.shift_remove(&installation.child_id);
                }
            }
        }
        installation.effect.dispose().await
    }
}

struct SetupCommit {
    invalidated: Arc<AtomicBool>,
}

impl AgentSetupCommit for SetupCommit {
    fn commit(&self) -> anyhow::Result<()> {
        if self.invalidated.load(Ordering::Acquire) {
            return Err(SubagentError::new(
                "a continuable-subagent setup contribution was revoked while this child was being built; the child was not established",
                "ACTIVATION_SETUP_REVOKED",
            )
            .into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cleanup(
        label: &'static str,
        action: impl Fn() -> anyhow::Result<()> + Send + Sync + 'static,
    ) -> EffectHandle {
        EffectHandle::synchronous(label, action)
    }

    #[test]
    fn installs_in_registration_order_and_commits() {
        let registry = SubagentActivationSetupRegistry::new();
        let order = Arc::new(Mutex::new(Vec::new()));
        for name in ["first", "second"] {
            let order = Arc::clone(&order);
            let _ = registry.register(Arc::new(move |_| {
                order.lock().push(name);
                Ok(cleanup(name, || Ok(())))
            }));
        }
        let child = Context::new();
        let transaction = registry.apply(&child).unwrap();
        assert_eq!(*order.lock(), ["first", "second"]);
        transaction.commit().unwrap();
    }

    #[tokio::test]
    async fn revocation_and_child_cleanup_converge_idempotently_in_either_order() {
        for revoke_first in [true, false] {
            let registry = SubagentActivationSetupRegistry::new();
            let disposals = Arc::new(AtomicUsize::new(0));
            let observed = Arc::clone(&disposals);
            let remove = registry.register(Arc::new(move |_| {
                let observed = Arc::clone(&observed);
                Ok(cleanup("count", move || {
                    observed.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }))
            }));
            let child = Context::new();
            registry.apply(&child).unwrap().commit().unwrap();
            if revoke_first {
                remove.dispose().await.unwrap();
                remove.dispose().await.unwrap();
                child.root_fiber().dispose().await.unwrap();
            } else {
                child.root_fiber().dispose().await.unwrap();
                remove.dispose().await.unwrap();
            }
            assert_eq!(disposals.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn removed_registration_is_skipped_and_precommit_revocation_invalidates() {
        let registry = SubagentActivationSetupRegistry::new();
        let installed = Arc::new(Mutex::new(Vec::new()));
        let gone = Arc::clone(&installed);
        let remove = registry.register(Arc::new(move |_| {
            gone.lock().push("gone");
            Ok(cleanup("gone", || Ok(())))
        }));
        remove.dispose().await.unwrap();
        let kept = Arc::clone(&installed);
        let _ = registry.register(Arc::new(move |_| {
            kept.lock().push("kept");
            Ok(cleanup("kept", || Ok(())))
        }));
        registry.apply(&Context::new()).unwrap().commit().unwrap();
        assert_eq!(*installed.lock(), ["kept"]);

        let registry = SubagentActivationSetupRegistry::new();
        let remove = registry.register(Arc::new(|_| Ok(cleanup("x", || Ok(())))));
        let transaction = registry.apply(&Context::new()).unwrap();
        remove.dispose().await.unwrap();
        assert!(
            transaction
                .commit()
                .unwrap_err()
                .to_string()
                .contains("revoked")
        );
    }

    #[test]
    fn installer_failure_rolls_back_earlier_installations() {
        let registry = SubagentActivationSetupRegistry::new();
        let released = Arc::new(Mutex::new(Vec::new()));
        let first = Arc::clone(&released);
        let _ = registry.register(Arc::new(move |_| {
            let first = Arc::clone(&first);
            Ok(cleanup("first", move || {
                first.lock().push("first");
                Ok(())
            }))
        }));
        let _ = registry.register(Arc::new(|_| anyhow::bail!("installer exploded")));
        let error = match registry.apply(&Context::new()) {
            Ok(_) => panic!("installer failure must reject apply"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("installer exploded"));
        assert_eq!(*released.lock(), ["first"]);
    }

    #[tokio::test]
    async fn revocation_and_child_cleanup_attempt_every_disposer_and_isolate_children() {
        let registry = SubagentActivationSetupRegistry::new();
        let released = Arc::new(Mutex::new(Vec::new()));
        let sequence = Arc::new(AtomicUsize::new(0));
        let output = Arc::clone(&released);
        let seq = Arc::clone(&sequence);
        let remove = registry.register(Arc::new(move |_| {
            let id = seq.fetch_add(1, Ordering::SeqCst) + 1;
            let output = Arc::clone(&output);
            Ok(cleanup("child", move || {
                output.lock().push(id);
                if id == 1 {
                    anyhow::bail!("first failed");
                }
                Ok(())
            }))
        }));
        let first = Context::new();
        let second = Context::new();
        registry.apply(&first).unwrap().commit().unwrap();
        registry.apply(&second).unwrap().commit().unwrap();
        let error = remove.dispose().await.unwrap_err().to_string();
        assert!(error.contains("failed to release 1 installation"));
        assert_eq!(*released.lock(), [1, 2]);

        let isolated = SubagentActivationSetupRegistry::new();
        let disposed = Arc::new(Mutex::new(Vec::new()));
        let seq = Arc::new(AtomicUsize::new(0));
        let output = Arc::clone(&disposed);
        let _ = isolated.register(Arc::new(move |_| {
            let id = seq.fetch_add(1, Ordering::SeqCst) + 1;
            let output = Arc::clone(&output);
            Ok(cleanup("isolated", move || {
                output.lock().push(id);
                Ok(())
            }))
        }));
        let one = Context::new();
        let two = Context::new();
        isolated.apply(&one).unwrap().commit().unwrap();
        isolated.apply(&two).unwrap().commit().unwrap();
        one.root_fiber().dispose().await.unwrap();
        assert_eq!(*disposed.lock(), [1]);
        two.root_fiber().dispose().await.unwrap();
        assert_eq!(*disposed.lock(), [1, 2]);
    }
}
