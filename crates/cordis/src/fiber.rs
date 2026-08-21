//! Plugin lifecycle ownership and reversible effects.

use std::{future::Future, pin::Pin, sync::Arc};

use parking_lot::Mutex;
use thiserror::Error;
use tokio::sync::Notify;
use uuid::Uuid;

/// Boxed asynchronous disposer result.
pub type DisposeFuture = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'static>>;

type Disposer = Box<dyn FnOnce() -> DisposeFuture + Send + 'static>;

/// Stable framework errors exposed to plugin callers.
#[derive(Debug, Error)]
pub enum CordisError {
    /// An effect was registered after its owning fiber began disposal.
    #[error("cannot create effect on inactive context")]
    InactiveEffect,
    /// Another provider already owns the service slot.
    #[error("service {0:?} is already provided in this scope")]
    DuplicateService(String),
}

/// Lifecycle state for one mounted plugin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FiberState {
    /// Waiting for required services.
    Pending,
    /// Plugin callback is running.
    Loading,
    /// Plugin is loaded and providing its effects.
    Active,
    /// Plugin callback or configuration failed.
    Failed,
    /// Disposers are running.
    Unloading,
    /// Fiber was removed and cannot restart.
    Disposed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum EffectOutcome {
    Ok,
    Error(String),
}

enum EffectState {
    Pending(Option<Disposer>),
    Running,
    Done(EffectOutcome),
}

struct EffectInner {
    state: tokio::sync::Mutex<EffectState>,
    notify: Notify,
    label: String,
}

/// Single-shot disposer shared by its caller and structural owner.
#[derive(Clone)]
pub struct EffectHandle {
    inner: Arc<EffectInner>,
}

impl std::fmt::Debug for EffectHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EffectHandle")
            .field("label", &self.inner.label)
            .finish_non_exhaustive()
    }
}

impl EffectHandle {
    /// Creates a handle for an asynchronous cleanup operation.
    pub fn new(
        label: impl Into<String>,
        disposer: impl FnOnce() -> DisposeFuture + Send + 'static,
    ) -> Self {
        Self {
            inner: Arc::new(EffectInner {
                state: tokio::sync::Mutex::new(EffectState::Pending(Some(Box::new(disposer)))),
                notify: Notify::new(),
                label: label.into(),
            }),
        }
    }

    /// Creates a handle for synchronous cleanup.
    pub fn synchronous(
        label: impl Into<String>,
        disposer: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
    ) -> Self {
        Self::new(label, || Box::pin(async move { disposer() }))
    }

    /// Human-readable diagnostic label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.inner.label
    }

    /// Runs cleanup once and joins a cleanup already started by another owner.
    ///
    /// # Errors
    ///
    /// Returns the cleanup failure to every caller that joins the disposal.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        loop {
            let notified = self.inner.notify.notified();
            let disposer = {
                let mut state = self.inner.state.lock().await;
                match &mut *state {
                    EffectState::Pending(disposer) => {
                        let Some(disposer) = disposer.take() else {
                            return Err(anyhow::anyhow!("pending effect has no disposer"));
                        };
                        *state = EffectState::Running;
                        Some(disposer)
                    }
                    EffectState::Running => None,
                    EffectState::Done(EffectOutcome::Ok) => return Ok(()),
                    EffectState::Done(EffectOutcome::Error(message)) => {
                        return Err(anyhow::anyhow!(message.clone()));
                    }
                }
            };

            if let Some(disposer) = disposer {
                let outcome = disposer().await.map_or_else(
                    |error| EffectOutcome::Error(format!("{error:#}")),
                    |()| EffectOutcome::Ok,
                );
                let result = match &outcome {
                    EffectOutcome::Ok => Ok(()),
                    EffectOutcome::Error(message) => Err(anyhow::anyhow!(message.clone())),
                };
                *self.inner.state.lock().await = EffectState::Done(outcome);
                self.inner.notify.notify_waiters();
                return result;
            }

            notified.await;
        }
    }
}

#[derive(Debug)]
struct FiberInner {
    state: FiberState,
    effects: Vec<EffectHandle>,
    transition: Option<Arc<FiberTransition>>,
    disposed_outcome: Option<EffectOutcome>,
}

#[derive(Debug, Default)]
struct FiberTransition {
    outcome: Mutex<Option<EffectOutcome>>,
    notify: Notify,
}

impl FiberTransition {
    fn complete(&self, outcome: EffectOutcome) {
        *self.outcome.lock() = Some(outcome);
        self.notify.notify_waiters();
    }

    async fn wait(&self) -> EffectOutcome {
        loop {
            let notified = self.notify.notified();
            if let Some(outcome) = self.outcome.lock().clone() {
                return outcome;
            }
            notified.await;
        }
    }
}

/// Runtime instance of one plugin application.
#[derive(Debug)]
pub struct Fiber {
    id: Uuid,
    name: String,
    root: bool,
    inner: Mutex<FiberInner>,
}

impl Fiber {
    /// Creates the permanently active root fiber.
    #[must_use]
    pub fn root() -> Arc<Self> {
        Arc::new(Self {
            id: Uuid::nil(),
            name: "root".to_owned(),
            root: true,
            inner: Mutex::new(FiberInner {
                state: FiberState::Active,
                effects: Vec::new(),
                transition: None,
                disposed_outcome: None,
            }),
        })
    }

    /// Creates a pending child fiber.
    #[must_use]
    pub fn child(name: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            id: Uuid::now_v7(),
            name: name.into(),
            root: false,
            inner: Mutex::new(FiberInner {
                state: FiberState::Pending,
                effects: Vec::new(),
                transition: None,
                disposed_outcome: None,
            }),
        })
    }

    /// Creates an immediately active child for a manually managed scope.
    #[must_use]
    pub fn active_child(name: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            id: Uuid::now_v7(),
            name: name.into(),
            root: false,
            inner: Mutex::new(FiberInner {
                state: FiberState::Active,
                effects: Vec::new(),
                transition: None,
                disposed_outcome: None,
            }),
        })
    }

    /// Stable identifier within the current process.
    #[must_use]
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// Display name inherited by diagnostics.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Current lifecycle state.
    #[must_use]
    pub fn state(&self) -> FiberState {
        self.inner.lock().state
    }

    pub(crate) fn set_state(&self, state: FiberState) {
        self.inner.lock().state = state;
    }

    /// Registers an effect for reverse-order teardown.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::InactiveEffect`] after this fiber begins disposal.
    pub fn own(&self, effect: EffectHandle) -> Result<EffectHandle, CordisError> {
        let mut inner = self.inner.lock();
        if matches!(inner.state, FiberState::Unloading | FiberState::Disposed) {
            return Err(CordisError::InactiveEffect);
        }
        inner.effects.push(effect.clone());
        Ok(effect)
    }

    /// Unloads every effect in reverse registration order.
    ///
    /// # Errors
    ///
    /// Returns an aggregate of cleanup failures after attempting every disposer.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        if self.root {
            return self.restart().await;
        }
        self.clear_effects(FiberState::Disposed).await
    }

    /// Disposes root-owned effects and leaves the root active for another composition.
    ///
    /// # Errors
    ///
    /// Returns an aggregate of cleanup failures after attempting every disposer.
    pub async fn restart(&self) -> anyhow::Result<()> {
        self.clear_effects(FiberState::Active).await
    }

    pub(crate) async fn deactivate(&self) -> anyhow::Result<()> {
        self.clear_effects(FiberState::Pending).await
    }

    pub(crate) async fn fail(&self) -> anyhow::Result<()> {
        self.clear_effects(FiberState::Failed).await
    }

    async fn clear_effects(&self, final_state: FiberState) -> anyhow::Result<()> {
        enum Clear {
            Run {
                effects: Vec<EffectHandle>,
                transition: Arc<FiberTransition>,
            },
            Join(Arc<FiberTransition>),
            Done(EffectOutcome),
        }

        let clear = {
            let mut inner = self.inner.lock();
            if inner.state == FiberState::Disposed {
                Clear::Done(inner.disposed_outcome.clone().unwrap_or(EffectOutcome::Ok))
            } else if let Some(transition) = &inner.transition {
                Clear::Join(transition.clone())
            } else {
                let transition = Arc::new(FiberTransition::default());
                inner.state = FiberState::Unloading;
                inner.transition = Some(transition.clone());
                Clear::Run {
                    effects: std::mem::take(&mut inner.effects),
                    transition,
                }
            }
        };
        let (effects, transition) = match clear {
            Clear::Run {
                effects,
                transition,
            } => (effects, transition),
            Clear::Join(transition) => return effect_outcome(transition.wait().await),
            Clear::Done(outcome) => return effect_outcome(outcome),
        };
        let mut errors = Vec::new();
        for effect in effects.into_iter().rev() {
            if let Err(error) = effect.dispose().await {
                errors.push(format!("{}: {error:#}", effect.label()));
            }
        }
        let outcome = if errors.is_empty() {
            EffectOutcome::Ok
        } else {
            EffectOutcome::Error(errors.join("\n"))
        };
        {
            let mut inner = self.inner.lock();
            inner.state = final_state;
            inner.transition = None;
            inner.disposed_outcome = (final_state == FiberState::Disposed).then(|| outcome.clone());
        }
        transition.complete(outcome.clone());
        effect_outcome(outcome)
    }
}

fn effect_outcome(outcome: EffectOutcome) -> anyhow::Result<()> {
    match outcome {
        EffectOutcome::Ok => Ok(()),
        EffectOutcome::Error(message) => Err(anyhow::anyhow!(message)),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use tokio::sync::oneshot;

    use super::*;

    #[tokio::test]
    async fn concurrent_disposal_joins_quiescence_and_replays_the_same_failure() {
        let fiber = Fiber::active_child("concurrent");
        let (started, started_rx) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let effect_calls = calls.clone();
        fiber
            .own(EffectHandle::new("delayed", move || {
                effect_calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    let _ = started.send(());
                    let _ = release_rx.await;
                    anyhow::bail!("cleanup exploded")
                })
            }))
            .unwrap();
        let first_fiber = fiber.clone();
        let first = tokio::spawn(async move { first_fiber.dispose().await });
        started_rx.await.unwrap();
        let second_fiber = fiber.clone();
        let mut second = tokio::spawn(async move { second_fiber.dispose().await });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut second)
                .await
                .is_err(),
            "a racing disposer returned before quiescence"
        );
        release.send(()).unwrap();
        let first_error = first.await.unwrap().unwrap_err().to_string();
        let second_error = second.await.unwrap().unwrap_err().to_string();
        assert_eq!(first_error, "delayed: cleanup exploded");
        assert_eq!(second_error, first_error);
        assert_eq!(fiber.dispose().await.unwrap_err().to_string(), first_error);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(fiber.state(), FiberState::Disposed);
    }

    #[tokio::test]
    async fn root_restart_can_own_a_fresh_generation_after_each_joined_transition() {
        let root = Fiber::root();
        let calls = Arc::new(AtomicUsize::new(0));
        for expected in 1..=2 {
            let effect_calls = calls.clone();
            root.own(EffectHandle::synchronous("generation", move || {
                effect_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }))
            .unwrap();
            let (left, right) = tokio::join!(root.restart(), root.restart());
            left.unwrap();
            right.unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), expected);
            assert_eq!(root.state(), FiberState::Active);
        }
    }
}
