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
        let effects = {
            let mut inner = self.inner.lock();
            if inner.state == FiberState::Disposed {
                return Ok(());
            }
            inner.state = FiberState::Unloading;
            std::mem::take(&mut inner.effects)
        };
        let mut errors = Vec::new();
        for effect in effects.into_iter().rev() {
            if let Err(error) = effect.dispose().await {
                errors.push(format!("{}: {error:#}", effect.label()));
            }
        }
        self.inner.lock().state = final_state;
        if errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(errors.join("\n")))
        }
    }
}
