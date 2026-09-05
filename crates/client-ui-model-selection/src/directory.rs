//! Portable per-session model directory with exact-generation publication.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::{Rc, Weak},
};

use futures::{FutureExt as _, future::LocalBoxFuture};
use seekdeep_identity::SessionId;

use crate::{ModelDirectoryState, ModelDirectoryStatus, ModelSelection, SessionModels};

/// Model RPC failure identity and copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelDirectoryFailure {
    /// Stable failure code.
    pub code: String,
    /// Human diagnostic.
    pub message: String,
}

impl ModelDirectoryFailure {
    fn compact(&self) -> String {
        format!("{}: {}", self.code, self.message)
    }
}

/// Injected Session model transport.
pub trait ModelDirectoryTransport {
    /// Loads the advisory directory.
    fn models(
        &self,
        session_id: SessionId,
    ) -> LocalBoxFuture<'static, Result<SessionModels, ModelDirectoryFailure>>;

    /// Selects a complete route.
    fn select_model(
        &self,
        session_id: SessionId,
        selection: ModelSelection,
    ) -> LocalBoxFuture<'static, Result<ModelSelection, ModelDirectoryFailure>>;
}

type Listener = Rc<dyn Fn()>;

struct DirectoryInner {
    generation: u64,
    disposed: bool,
    snapshot: Rc<ModelDirectoryState>,
    listeners: BTreeMap<u64, Listener>,
    next_listener: u64,
}

/// One session's shared model directory.
pub struct ModelDirectory {
    transport: Rc<dyn ModelDirectoryTransport>,
    session_id: SessionId,
    available: Rc<dyn Fn() -> bool>,
    inner: RefCell<DirectoryInner>,
}

impl ModelDirectory {
    /// Creates an idle directory.
    #[must_use]
    pub fn new(
        transport: Rc<dyn ModelDirectoryTransport>,
        session_id: SessionId,
        available: Rc<dyn Fn() -> bool>,
    ) -> Rc<Self> {
        Rc::new(Self {
            transport,
            session_id,
            available,
            inner: RefCell::new(DirectoryInner {
                generation: 0,
                disposed: false,
                snapshot: Rc::new(ModelDirectoryState::default()),
                listeners: BTreeMap::new(),
                next_listener: 0,
            }),
        })
    }

    /// Returns a stable snapshot identity until mutation.
    #[must_use]
    pub fn snapshot(&self) -> Rc<ModelDirectoryState> {
        self.inner.borrow().snapshot.clone()
    }

    /// Subscribes to every synchronous snapshot replacement.
    ///
    /// # Panics
    ///
    /// Panics after exhausting every `u64` listener id rather than aliasing a live listener.
    #[must_use]
    pub fn subscribe(self: &Rc<Self>, listener: Listener) -> ModelDirectorySubscription {
        let id = {
            let mut inner = self.inner.borrow_mut();
            inner.next_listener = inner
                .next_listener
                .checked_add(1)
                .expect("model directory listener id exhausted");
            let id = inner.next_listener;
            inner.listeners.insert(id, listener);
            id
        };
        ModelDirectorySubscription {
            directory: Rc::downgrade(self),
            id: Some(id),
        }
    }

    /// Loads the directory and returns the response even when stale or disposed.
    #[must_use]
    pub fn load(self: &Rc<Self>) -> LocalBoxFuture<'static, Result<SessionModels, String>> {
        if !(self.available)() {
            return futures::future::ready(Err(
                "model selection is unavailable for addressed subagent sessions".to_owned(),
            ))
            .boxed_local();
        }
        let generation = self.begin(ModelDirectoryStatus::Loading);
        let directory = self.clone();
        async move {
            let result = directory
                .transport
                .models(directory.session_id.clone())
                .await;
            if directory.disposed_or_stale(generation) {
                return result.map_err(|error| error.compact());
            }
            match result {
                Ok(models) => {
                    let published = models.clone();
                    directory.update(move |state| {
                        state.current = Some(published.current);
                        state.routable = Some(published.routable);
                        state.groups = published.groups;
                        state.failures = published.failures;
                        state.status = ModelDirectoryStatus::Ready;
                        state.error = None;
                    });
                    Ok(models)
                }
                Err(error) => {
                    let compact = error.compact();
                    directory.fail(compact.clone());
                    Err(format!("session.models failed: {compact}"))
                }
            }
        }
        .boxed_local()
    }

    /// Selects a complete route through the shared state.
    #[must_use]
    pub fn select(
        self: &Rc<Self>,
        selection: ModelSelection,
    ) -> LocalBoxFuture<'static, Result<(), String>> {
        if !(self.available)() {
            return futures::future::ready(Err(
                "model selection is unavailable for addressed subagent sessions".to_owned(),
            ))
            .boxed_local();
        }
        let generation = self.begin(ModelDirectoryStatus::Selecting);
        let directory = self.clone();
        async move {
            let result = directory
                .transport
                .select_model(directory.session_id.clone(), selection)
                .await;
            if directory.disposed_or_stale(generation) {
                return result.map(|_| ()).map_err(|error| error.compact());
            }
            match result {
                Ok(selected) => {
                    directory.update(move |state| {
                        state.current = Some(selected);
                        state.routable = Some(true);
                        state.status = ModelDirectoryStatus::Ready;
                        state.error = None;
                    });
                    Ok(())
                }
                Err(error) => {
                    let compact = error.compact();
                    directory.fail(compact.clone());
                    Err(format!("session.selectModel failed: {compact}"))
                }
            }
        }
        .boxed_local()
    }

    /// Clears process-local state and returns a repull only when still available.
    ///
    /// # Panics
    ///
    /// Panics after exhausting every `u64` generation rather than permitting stale publication.
    #[must_use]
    pub fn reset_connected(
        self: &Rc<Self>,
    ) -> Option<LocalBoxFuture<'static, Result<SessionModels, String>>> {
        {
            let mut inner = self.inner.borrow_mut();
            if inner.disposed {
                return None;
            }
            inner.generation = inner
                .generation
                .checked_add(1)
                .expect("model directory generation exhausted");
        }
        self.update(|state| *state = ModelDirectoryState::default());
        (self.available)().then(|| self.load())
    }

    /// Prevents every later settlement from writing.
    pub fn dispose(&self) {
        self.inner.borrow_mut().disposed = true;
    }

    fn begin(&self, status: ModelDirectoryStatus) -> u64 {
        let generation = {
            let mut inner = self.inner.borrow_mut();
            inner.generation = inner
                .generation
                .checked_add(1)
                .expect("model directory generation exhausted");
            inner.generation
        };
        self.update(|state| {
            state.status = status;
            state.error = None;
        });
        generation
    }

    fn disposed_or_stale(&self, generation: u64) -> bool {
        let inner = self.inner.borrow();
        inner.disposed || inner.generation != generation
    }

    fn fail(&self, error: String) {
        self.update(move |state| {
            state.status = ModelDirectoryStatus::Error;
            state.error = Some(error);
        });
    }

    fn update(&self, mutate: impl FnOnce(&mut ModelDirectoryState)) {
        let listeners = {
            let mut inner = self.inner.borrow_mut();
            let mut snapshot = (*inner.snapshot).clone();
            mutate(&mut snapshot);
            inner.snapshot = Rc::new(snapshot);
            inner.listeners.values().cloned().collect::<Vec<_>>()
        };
        for listener in listeners {
            listener();
        }
    }

    fn unsubscribe(&self, id: u64) {
        self.inner.borrow_mut().listeners.remove(&id);
    }
}

/// Idempotent model directory subscription.
pub struct ModelDirectorySubscription {
    directory: Weak<ModelDirectory>,
    id: Option<u64>,
}

impl ModelDirectorySubscription {
    /// Removes the listener once.
    pub fn dispose(&mut self) {
        let Some(id) = self.id.take() else {
            return;
        };
        if let Some(directory) = self.directory.upgrade() {
            directory.unsubscribe(id);
        }
    }
}

impl Drop for ModelDirectorySubscription {
    fn drop(&mut self) {
        self.dispose();
    }
}
