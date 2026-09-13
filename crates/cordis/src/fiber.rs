//! Plugin lifecycle ownership and reversible effects.

use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

use parking_lot::Mutex;
use thiserror::Error;
use tokio::sync::Notify;
use uuid::Uuid;

/// Boxed asynchronous disposer result.
#[cfg(not(target_arch = "wasm32"))]
pub type DisposeFuture = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'static>>;
/// Browser disposers stay on the page's single-threaded local executor.
#[cfg(target_arch = "wasm32")]
pub type DisposeFuture = Pin<Box<dyn Future<Output = anyhow::Result<()>> + 'static>>;

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
    /// A service value was assigned before any provider registered it.
    #[error("cannot set property {0:?} without provide")]
    MissingService(String),
    /// A service value was assigned from a fiber other than its provider.
    #[error("cannot set property {0:?} in multiple fibers")]
    ServiceOwner(String),
    /// A service or accessor already declared the same reflected property.
    #[error("property {name:?} is already declared as {kind}")]
    PropertyDeclared {
        /// Reflected property name.
        name: String,
        /// Existing declaration kind.
        kind: &'static str,
    },
    /// A synchronous `internal/plugin` creation observer rejected publication.
    #[error("plugin publication failed: {0}")]
    PluginPublication(String),
    /// Original JavaScript exception from browser plugin publication.
    #[cfg(target_arch = "wasm32")]
    #[error("plugin publication failed: {}", crate::wasm::js_anyhow(.0))]
    BrowserPublication(wasm_bindgen::JsValue),
    /// A synchronous service-publication guard rejected the new provider.
    #[error("service publication failed: {0}")]
    ServicePublication(String),
    /// A browser event-hook table rejected a native listener registration.
    #[error("event publication failed: {0}")]
    EventPublication(String),
    /// A browser disposable list rejected registration.
    #[cfg(target_arch = "wasm32")]
    #[error("effect registration failed: {}", crate::wasm::js_anyhow(.0))]
    BrowserEffect(wasm_bindgen::JsValue),
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

#[cfg(target_arch = "wasm32")]
#[derive(Clone)]
pub(crate) struct BrowserFiberObserver {
    pub changed: Arc<dyn Fn(FiberState) + Send + Sync>,
    pub prepare: Arc<dyn Fn(bool) + Send + Sync>,
    pub dispose: Arc<dyn Fn() -> DisposeFuture + Send + Sync>,
    pub settled: Arc<dyn Fn() -> DisposeFuture + Send + Sync>,
}

#[cfg(target_arch = "wasm32")]
impl std::fmt::Debug for BrowserFiberObserver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("BrowserFiberObserver")
    }
}

/// Scheduling policy for one captured Fiber teardown batch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DisposalScheduling {
    /// Finish each disposer before starting the next, in reverse registration order.
    #[default]
    Serial,
    /// Start in reverse registration order and join all captured disposers concurrently.
    Concurrent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum EffectOutcome {
    Ok,
    Error(String),
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) type SharedDisposal =
    futures::future::Shared<futures::future::BoxFuture<'static, Result<(), String>>>;

enum EffectState {
    Pending(Option<Disposer>),
    Running,
    Done(EffectOutcome),
}

struct EffectInner {
    // State transitions never await. Synchronous service-change cleanup must not
    // yield to the enclosing executor just to claim or complete its disposer.
    state: Mutex<EffectState>,
    notify: Notify,
    label: String,
    #[cfg(not(target_arch = "wasm32"))]
    disposal: Mutex<Option<SharedDisposal>>,
    #[cfg(target_arch = "wasm32")]
    browser_disposer: Mutex<Option<js_sys::WeakRef<js_sys::Function>>>,
    #[cfg(target_arch = "wasm32")]
    browser_error: Mutex<Option<wasm_bindgen::JsValue>>,
}

/// Single-shot disposer shared by its caller and structural owner.
#[derive(Clone)]
pub struct EffectHandle {
    inner: Arc<EffectInner>,
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct WeakEffectHandle {
    inner: Weak<EffectInner>,
}

#[cfg(not(target_arch = "wasm32"))]
impl WeakEffectHandle {
    pub(crate) fn upgrade(&self) -> Option<EffectHandle> {
        self.inner.upgrade().map(|inner| EffectHandle { inner })
    }
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
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn downgrade(&self) -> WeakEffectHandle {
        WeakEffectHandle {
            inner: Arc::downgrade(&self.inner),
        }
    }

    /// Creates a handle for an asynchronous cleanup operation.
    pub fn new(
        label: impl Into<String>,
        disposer: impl FnOnce() -> DisposeFuture + Send + 'static,
    ) -> Self {
        Self {
            inner: Arc::new(EffectInner {
                state: Mutex::new(EffectState::Pending(Some(Box::new(disposer)))),
                notify: Notify::new(),
                label: label.into(),
                #[cfg(not(target_arch = "wasm32"))]
                disposal: Mutex::new(None),
                #[cfg(target_arch = "wasm32")]
                browser_disposer: Mutex::new(None),
                #[cfg(target_arch = "wasm32")]
                browser_error: Mutex::new(None),
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

    #[cfg_attr(not(target_arch = "wasm32"), allow(clippy::unused_self))]
    fn disposal_error(&self, message: &str) -> anyhow::Error {
        #[cfg(target_arch = "wasm32")]
        if let Some(error) = self.inner.browser_error.lock().as_ref() {
            return crate::wasm::js_anyhow(error);
        }
        anyhow::anyhow!(message.to_owned())
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn browser_disposer(&self) -> Option<js_sys::Function> {
        self.inner
            .browser_disposer
            .lock()
            .as_ref()
            .and_then(js_sys::WeakRef::deref)
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn set_browser_disposer(&self, disposer: &js_sys::Function) {
        *self.inner.browser_disposer.lock() = Some(js_sys::WeakRef::new(disposer));
    }

    /// Runs cleanup once and joins a cleanup already started by another owner.
    /// An uncontended synchronous disposer completes without an executor handoff.
    ///
    /// # Errors
    ///
    /// Returns the cleanup failure to every caller that joins the disposal.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            use futures::FutureExt as _;

            let disposal = self
                .inner
                .disposal
                .lock()
                .get_or_insert_with(|| {
                    let owned = self.clone();
                    async move {
                        owned
                            .dispose_once()
                            .await
                            .map_err(|error| format!("{error:#}"))
                    }
                    .boxed()
                    .shared()
                })
                .clone();
            disposal.await.map_err(anyhow::Error::msg)
        }
        #[cfg(target_arch = "wasm32")]
        self.dispose_once().await
    }

    async fn dispose_once(&self) -> anyhow::Result<()> {
        loop {
            let notified = self.inner.notify.notified();
            let disposer = {
                let mut state = self.inner.state.lock();
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
                        return Err(self.disposal_error(message));
                    }
                }
            };

            if let Some(disposer) = disposer {
                let outcome = disposer().await.map_or_else(
                    |error| {
                        #[cfg(target_arch = "wasm32")]
                        {
                            *self.inner.browser_error.lock() = crate::wasm::js_cause(&error);
                        }
                        EffectOutcome::Error(format!("{error:#}"))
                    },
                    |()| EffectOutcome::Ok,
                );
                let result = match &outcome {
                    EffectOutcome::Ok => Ok(()),
                    EffectOutcome::Error(message) => Err(self.disposal_error(message)),
                };
                *self.inner.state.lock() = EffectState::Done(outcome);
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
    parent: Option<Weak<Fiber>>,
    disposal_scheduling: DisposalScheduling,
    inner: Mutex<FiberInner>,
    disposal_requested: AtomicBool,
    disposal_notify: Notify,
    #[cfg(not(target_arch = "wasm32"))]
    disposal: Mutex<Option<SharedDisposal>>,
    #[cfg(target_arch = "wasm32")]
    browser_observer: Mutex<Option<BrowserFiberObserver>>,
    #[cfg(target_arch = "wasm32")]
    browser_lookup: std::sync::atomic::AtomicU8,
    #[cfg(target_arch = "wasm32")]
    browser_context: Mutex<wasm_bindgen::JsValue>,
    #[cfg(target_arch = "wasm32")]
    browser_effect_owner: Mutex<wasm_bindgen::JsValue>,
}

impl Fiber {
    /// Creates the permanently active root fiber.
    #[must_use]
    pub fn root() -> Arc<Self> {
        Self::root_with_disposal_scheduling(DisposalScheduling::Serial)
    }

    /// Creates a root with an explicit teardown policy inherited by linked children.
    #[must_use]
    pub fn root_with_disposal_scheduling(disposal_scheduling: DisposalScheduling) -> Arc<Self> {
        Arc::new(Self {
            id: Uuid::nil(),
            name: "root".to_owned(),
            root: true,
            parent: None,
            disposal_scheduling,
            inner: Mutex::new(FiberInner {
                state: FiberState::Active,
                effects: Vec::new(),
                transition: None,
                disposed_outcome: None,
            }),
            disposal_requested: AtomicBool::new(false),
            disposal_notify: Notify::new(),
            #[cfg(not(target_arch = "wasm32"))]
            disposal: Mutex::new(None),
            #[cfg(target_arch = "wasm32")]
            browser_observer: Mutex::new(None),
            #[cfg(target_arch = "wasm32")]
            browser_lookup: std::sync::atomic::AtomicU8::new(2),
            #[cfg(target_arch = "wasm32")]
            browser_context: Mutex::new(wasm_bindgen::JsValue::UNDEFINED),
            #[cfg(target_arch = "wasm32")]
            browser_effect_owner: Mutex::new(wasm_bindgen::JsValue::UNDEFINED),
        })
    }

    /// Creates a pending child fiber.
    #[must_use]
    pub fn child(name: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            id: Uuid::now_v7(),
            name: name.into(),
            root: false,
            parent: None,
            disposal_scheduling: DisposalScheduling::Serial,
            inner: Mutex::new(FiberInner {
                state: FiberState::Pending,
                effects: Vec::new(),
                transition: None,
                disposed_outcome: None,
            }),
            disposal_requested: AtomicBool::new(false),
            disposal_notify: Notify::new(),
            #[cfg(not(target_arch = "wasm32"))]
            disposal: Mutex::new(None),
            #[cfg(target_arch = "wasm32")]
            browser_observer: Mutex::new(None),
            #[cfg(target_arch = "wasm32")]
            browser_lookup: std::sync::atomic::AtomicU8::new(2),
            #[cfg(target_arch = "wasm32")]
            browser_context: Mutex::new(wasm_bindgen::JsValue::UNDEFINED),
            #[cfg(target_arch = "wasm32")]
            browser_effect_owner: Mutex::new(wasm_bindgen::JsValue::UNDEFINED),
        })
    }

    /// Creates a pending child linked to its owning parent fiber.
    #[must_use]
    pub fn child_of(name: impl Into<String>, parent: &Arc<Fiber>) -> Arc<Self> {
        Arc::new(Self {
            id: Uuid::now_v7(),
            name: name.into(),
            root: false,
            parent: Some(Arc::downgrade(parent)),
            disposal_scheduling: parent.disposal_scheduling,
            inner: Mutex::new(FiberInner {
                state: FiberState::Pending,
                effects: Vec::new(),
                transition: None,
                disposed_outcome: None,
            }),
            disposal_requested: AtomicBool::new(false),
            disposal_notify: Notify::new(),
            #[cfg(not(target_arch = "wasm32"))]
            disposal: Mutex::new(None),
            #[cfg(target_arch = "wasm32")]
            browser_observer: Mutex::new(None),
            #[cfg(target_arch = "wasm32")]
            browser_lookup: std::sync::atomic::AtomicU8::new(2),
            #[cfg(target_arch = "wasm32")]
            browser_context: Mutex::new(wasm_bindgen::JsValue::UNDEFINED),
            #[cfg(target_arch = "wasm32")]
            browser_effect_owner: Mutex::new(wasm_bindgen::JsValue::UNDEFINED),
        })
    }

    /// Creates an immediately active child for a manually managed scope.
    #[must_use]
    pub fn active_child(name: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            id: Uuid::now_v7(),
            name: name.into(),
            root: false,
            parent: None,
            disposal_scheduling: DisposalScheduling::Serial,
            inner: Mutex::new(FiberInner {
                state: FiberState::Active,
                effects: Vec::new(),
                transition: None,
                disposed_outcome: None,
            }),
            disposal_requested: AtomicBool::new(false),
            disposal_notify: Notify::new(),
            #[cfg(not(target_arch = "wasm32"))]
            disposal: Mutex::new(None),
            #[cfg(target_arch = "wasm32")]
            browser_observer: Mutex::new(None),
            #[cfg(target_arch = "wasm32")]
            browser_lookup: std::sync::atomic::AtomicU8::new(2),
            #[cfg(target_arch = "wasm32")]
            browser_context: Mutex::new(wasm_bindgen::JsValue::UNDEFINED),
            #[cfg(target_arch = "wasm32")]
            browser_effect_owner: Mutex::new(wasm_bindgen::JsValue::UNDEFINED),
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

    /// Whether this fiber is `root` or belongs to its linked descendant tree.
    #[must_use]
    pub fn is_within(self: &Arc<Self>, root: &Arc<Self>) -> bool {
        let mut current = Some(self.clone());
        while let Some(fiber) = current {
            if Arc::ptr_eq(&fiber, root) {
                return true;
            }
            current = fiber.parent.as_ref().and_then(Weak::upgrade);
        }
        false
    }

    /// Current lifecycle state.
    #[must_use]
    pub fn state(&self) -> FiberState {
        self.inner.lock().state
    }

    /// Whether the structural plugin owner has requested permanent disposal.
    #[must_use]
    pub fn is_disposal_requested(&self) -> bool {
        self.disposal_requested.load(Ordering::Acquire)
    }

    /// Waits until the structural plugin owner requests permanent disposal.
    pub async fn when_disposing(&self) {
        loop {
            let notified = self.disposal_notify.notified();
            if self.is_disposal_requested() {
                return;
            }
            notified.await;
        }
    }

    pub(crate) fn request_disposal(&self) {
        if !self.disposal_requested.swap(true, Ordering::AcqRel) {
            self.disposal_notify.notify_waiters();
        }
    }

    pub(crate) fn set_state(&self, state: FiberState) {
        self.inner.lock().state = state;
        #[cfg(target_arch = "wasm32")]
        self.notify_browser_state(state);
    }

    pub(crate) fn active_for_lookup(&self) -> bool {
        #[cfg(target_arch = "wasm32")]
        match self.browser_lookup.load(Ordering::Acquire) {
            0 => return false,
            1 => return true,
            _ => {}
        }
        self.state() == FiberState::Active
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn set_browser_lookup(&self, active: bool) {
        self.browser_lookup
            .store(u8::from(active), Ordering::Release);
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn set_browser_context(&self, context: wasm_bindgen::JsValue) {
        *self.browser_context.lock() = context;
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn set_browser_state(&self, state: FiberState) {
        self.inner.lock().state = state;
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn take_browser_effects(&self) -> Vec<EffectHandle> {
        let owner = self.browser_effect_owner.lock().clone();
        self.take_browser_effects_for(&owner)
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn take_browser_effects_for(
        &self,
        owner: &wasm_bindgen::JsValue,
    ) -> Vec<EffectHandle> {
        let mut effects = std::mem::take(&mut self.inner.lock().effects);
        if !owner.is_undefined() {
            match crate::wasm::browser_effects::snapshot(owner) {
                Ok(browser) => effects.extend(browser),
                Err(error) => effects.push(EffectHandle::synchronous(
                    "browser effect snapshot",
                    move || Err(crate::wasm::js_anyhow(&error)),
                )),
            }
        }
        effects
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn install_browser_effects(
        &self,
        owner: &wasm_bindgen::JsValue,
    ) -> Result<(), wasm_bindgen::JsValue> {
        crate::wasm::browser_effects::install(owner)?;
        *self.browser_effect_owner.lock() = owner.clone();
        let effects = std::mem::take(&mut self.inner.lock().effects);
        for effect in effects {
            crate::wasm::browser_effects::register_native(owner, &effect)?;
        }
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn browser_disposal(&self) -> Option<DisposeFuture> {
        let observer = self.browser_observer.lock().clone();
        observer.map(|observer| (observer.dispose)())
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn browser_settled(&self) -> Option<DisposeFuture> {
        let observer = self.browser_observer.lock().clone();
        observer.map(|observer| (observer.settled)())
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn browser_context(&self) -> wasm_bindgen::JsValue {
        self.browser_context.lock().clone()
    }

    pub(crate) fn can_register_effect(&self) -> bool {
        self.can_register_in_state(self.state())
    }

    fn can_register_in_state(&self, state: FiberState) -> bool {
        if state == FiberState::Disposed || self.is_disposal_requested() {
            return false;
        }
        if state != FiberState::Unloading {
            return true;
        }
        #[cfg(target_arch = "wasm32")]
        if self.browser_lookup.load(Ordering::Acquire) == 1 {
            return true;
        }
        false
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn observe_browser(&self, observer: BrowserFiberObserver) {
        *self.browser_observer.lock() = Some(observer);
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn prepare_browser_transition(&self, explicit: bool) {
        let observer = self.browser_observer.lock().clone();
        if let Some(observer) = observer {
            (observer.prepare)(explicit);
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn notify_browser_state(&self, state: FiberState) {
        let observer = self.browser_observer.lock().clone();
        if let Some(observer) = observer {
            (observer.changed)(state);
        }
    }

    /// Registers an effect for reverse-order teardown.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::InactiveEffect`] after this fiber begins disposal.
    pub fn own(&self, effect: EffectHandle) -> Result<EffectHandle, CordisError> {
        let mut inner = self.inner.lock();
        if !self.can_register_in_state(inner.state) {
            return Err(CordisError::InactiveEffect);
        }
        #[cfg(target_arch = "wasm32")]
        {
            let owner = self.browser_effect_owner.lock().clone();
            if !owner.is_undefined() {
                drop(inner);
                crate::wasm::browser_effects::register_native(&owner, &effect)
                    .map_err(CordisError::BrowserEffect)?;
                return Ok(effect);
            }
        }
        inner.effects.push(effect.clone());
        Ok(effect)
    }

    /// Unloads every effect in reverse registration order.
    ///
    /// # Errors
    ///
    /// Returns an aggregate of cleanup failures after attempting every disposer.
    pub async fn dispose(self: &Arc<Self>) -> anyhow::Result<()> {
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
    pub async fn restart(self: &Arc<Self>) -> anyhow::Result<()> {
        #[cfg(target_arch = "wasm32")]
        if self.root
            && let Some(disposal) = self.browser_disposal()
        {
            return disposal.await;
        }
        self.clear_effects(FiberState::Active).await
    }

    pub(crate) async fn deactivate(self: &Arc<Self>) -> anyhow::Result<()> {
        self.clear_effects(FiberState::Pending).await
    }

    pub(crate) async fn fail(self: &Arc<Self>) -> anyhow::Result<()> {
        self.clear_effects(FiberState::Failed).await
    }

    async fn clear_effects(self: &Arc<Self>, final_state: FiberState) -> anyhow::Result<()> {
        self.clear_effects_with_scheduling(final_state, self.disposal_scheduling)
            .await
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) async fn dispose_with_scheduling(
        self: &Arc<Self>,
        scheduling: DisposalScheduling,
    ) -> anyhow::Result<()> {
        self.clear_effects_with_scheduling(
            if self.root {
                FiberState::Active
            } else {
                FiberState::Disposed
            },
            scheduling,
        )
        .await
    }

    async fn clear_effects_with_scheduling(
        self: &Arc<Self>,
        final_state: FiberState,
        scheduling: DisposalScheduling,
    ) -> anyhow::Result<()> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            use futures::FutureExt as _;

            let disposal = {
                let mut current = self.disposal.lock();
                if current
                    .as_ref()
                    .is_some_and(|current| current.peek().is_some())
                {
                    *current = None;
                }
                current
                    .get_or_insert_with(|| {
                        let owned = self.clone();
                        async move {
                            owned
                                .clear_effects_once(final_state, scheduling)
                                .await
                                .map_err(|error| format!("{error:#}"))
                        }
                        .boxed()
                        .shared()
                    })
                    .clone()
            };
            disposal.await.map_err(anyhow::Error::msg)
        }
        #[cfg(target_arch = "wasm32")]
        self.clear_effects_once(final_state, scheduling).await
    }

    async fn clear_effects_once(
        &self,
        final_state: FiberState,
        scheduling: DisposalScheduling,
    ) -> anyhow::Result<()> {
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
                let effects = std::mem::take(&mut inner.effects);
                Clear::Run {
                    effects,
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
        #[cfg(target_arch = "wasm32")]
        let effects = {
            let mut effects = effects;
            effects.extend(self.take_browser_effects());
            effects
        };
        #[cfg(target_arch = "wasm32")]
        self.notify_browser_state(FiberState::Unloading);
        let errors = match scheduling {
            DisposalScheduling::Serial => {
                let mut errors = Vec::new();
                for effect in effects.into_iter().rev() {
                    if let Err(error) = effect.dispose().await {
                        errors.push(format!("{}: {error:#}", effect.label()));
                    }
                }
                errors
            }
            DisposalScheduling::Concurrent => {
                futures::future::join_all(effects.into_iter().rev().map(|effect| async move {
                    effect
                        .dispose()
                        .await
                        .err()
                        .map(|error| format!("{}: {error:#}", effect.label()))
                }))
                .await
                .into_iter()
                .flatten()
                .collect()
            }
        };
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
        #[cfg(target_arch = "wasm32")]
        self.notify_browser_state(final_state);
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

// Native-only: these lifecycle tests drive tokio timers, which the wasm32 face never links.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use futures::FutureExt as _;
    use tokio::sync::oneshot;

    use super::*;

    #[test]
    fn disposal_request_rejects_effects_before_cleanup_changes_state() {
        let fiber = Fiber::active_child("admitted disposal");
        fiber.request_disposal();
        assert_eq!(fiber.state(), FiberState::Active);
        let effect = EffectHandle::synchronous("late effect", || Ok(()));
        assert!(matches!(
            fiber.own(effect),
            Err(CordisError::InactiveEffect)
        ));
    }

    #[test]
    fn concurrent_disposal_inherits_policy_starts_every_effect_and_joins_errors() {
        let root = Fiber::root_with_disposal_scheduling(DisposalScheduling::Concurrent);
        let fiber = Fiber::child_of("concurrent child", &root);
        let started = Arc::new(Mutex::new(Vec::new()));
        let (release, released) = futures::channel::oneshot::channel::<()>();
        let gate = released.shared();
        for index in 0..64 {
            let started = started.clone();
            let gate = gate.clone();
            fiber
                .own(EffectHandle::new(format!("effect-{index}"), move || {
                    Box::pin(async move {
                        started.lock().push(index);
                        gate.await?;
                        anyhow::ensure!(index != 0 && index != 63, "failure {index}");
                        Ok(())
                    })
                }))
                .unwrap();
        }
        let mut first = Box::pin(fiber.dispose());
        assert!(first.as_mut().now_or_never().is_none());
        let mut second = Box::pin(fiber.dispose());
        assert!(second.as_mut().now_or_never().is_none());
        let before_release = started.lock().clone();
        release.send(()).unwrap();
        let (first, second) = futures::executor::block_on(async { futures::join!(first, second) });
        assert_eq!(before_release, (0..64).rev().collect::<Vec<_>>());
        let expected = "effect-63: failure 63\neffect-0: failure 0";
        assert_eq!(first.unwrap_err().to_string(), expected);
        assert_eq!(second.unwrap_err().to_string(), expected);
        assert_eq!(fiber.state(), FiberState::Disposed);
        assert_eq!(
            futures::executor::block_on(fiber.dispose())
                .unwrap_err()
                .to_string(),
            expected
        );
    }

    #[tokio::test]
    async fn synchronous_disposal_is_ready_after_cooperative_budget_exhaustion() {
        let calls = Arc::new(AtomicUsize::new(0));
        let recorded = calls.clone();
        let effect = EffectHandle::synchronous("synchronous registration", move || {
            recorded.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        while tokio::task::coop::has_budget_remaining() {
            tokio::task::consume_budget().await;
        }
        // Service-change callbacks synchronously withdraw registrations. Their cleanup
        // cannot require the enclosing Tokio task to yield before returning.
        let result = effect.dispose().now_or_never();
        assert!(
            matches!(result, Some(Ok(()))),
            "synchronous disposal yielded: {result:?}"
        );
        assert!(matches!(effect.dispose().now_or_never(), Some(Ok(()))));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

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
