//! Target-portable local settings-document metadata and native-open state owner.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::{Rc, Weak},
};

use futures::{FutureExt, future::LocalBoxFuture};

/// Provider-owned settings-document availability phase.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsDocumentStatus {
    /// No surface has requested metadata yet.
    #[default]
    Idle,
    /// A metadata request is crossing the Host boundary.
    Loading,
    /// The provider exposes a local document.
    Ready,
    /// No document is exposed or the metadata request failed.
    Unavailable,
}

/// Immutable browser state of the Host-owned settings document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsDocumentState {
    /// Metadata phase.
    pub status: SettingsDocumentStatus,
    /// Whether one native-open request is in flight.
    pub opening: bool,
    /// Last metadata or native-open diagnostic.
    pub error: Option<String>,
}

/// Successful metadata payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingsDocumentDescription {
    /// Whether the provider reports a local document.
    pub has_document: bool,
}

/// Generated settings RPC settlement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsDocumentCall<T> {
    /// Host business success.
    Success(T),
    /// Host business rejection carrying its stable message.
    Rejected(String),
    /// Transport failure stringified at the boundary.
    Failed(String),
}

/// Injected generated settings API boundary.
pub trait SettingsDocumentTransport {
    /// Reads provider metadata.
    fn describe(
        &self,
    ) -> LocalBoxFuture<'static, SettingsDocumentCall<SettingsDocumentDescription>>;
    /// Requests the Host-owned native document handoff.
    fn open_document(&self) -> LocalBoxFuture<'static, SettingsDocumentCall<()>>;
}

/// Injected owner for reconnect refresh work.
pub trait SettingsDocumentTaskSpawner {
    /// Owns one local refresh until settlement.
    fn spawn(&self, task: LocalBoxFuture<'static, ()>);
}

type DocumentListener = Rc<dyn Fn() -> Result<(), String>>;
type DisposalCallback = Rc<RefCell<Option<Box<dyn FnOnce()>>>>;

/// Idempotent document-state listener cleanup.
#[derive(Clone)]
pub struct SettingsDocumentSubscription {
    callback: DisposalCallback,
}

impl SettingsDocumentSubscription {
    /// Runs listener cleanup at most once.
    pub fn dispose(&self) {
        if let Some(callback) = self.callback.borrow_mut().take() {
            callback();
        }
    }
}

struct DocumentStoreState {
    snapshot: Rc<SettingsDocumentState>,
    listeners: BTreeMap<u64, DocumentListener>,
    next_listener: u64,
    generation: u64,
}

/// Loads local-document availability and invokes the pathless Host-owned open operation.
pub struct SettingsDocumentStore {
    weak_self: Weak<Self>,
    transport: Rc<dyn SettingsDocumentTransport>,
    state: RefCell<DocumentStoreState>,
}

impl std::fmt::Debug for SettingsDocumentStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.borrow();
        formatter
            .debug_struct("SettingsDocumentStore")
            .field("status", &state.snapshot.status)
            .field("opening", &state.snapshot.opening)
            .field("listeners", &state.listeners.len())
            .field("generation", &state.generation)
            .finish_non_exhaustive()
    }
}

impl SettingsDocumentStore {
    /// Creates an idle state owner over the loopback settings wire.
    #[must_use]
    pub fn new(transport: Rc<dyn SettingsDocumentTransport>) -> Rc<Self> {
        Rc::new_cyclic(|weak_self| Self {
            weak_self: weak_self.clone(),
            transport,
            state: RefCell::new(DocumentStoreState {
                snapshot: Rc::new(SettingsDocumentState::default()),
                listeners: BTreeMap::new(),
                next_listener: 0,
                generation: 0,
            }),
        })
    }

    /// Current reference-stable state snapshot.
    #[must_use]
    pub fn snapshot(&self) -> Rc<SettingsDocumentState> {
        self.state.borrow().snapshot.clone()
    }

    /// Observes committed state replacements.
    #[must_use]
    pub fn subscribe(&self, listener: DocumentListener) -> SettingsDocumentSubscription {
        let id = {
            let mut state = self.state.borrow_mut();
            state.next_listener = state.next_listener.wrapping_add(1);
            let id = state.next_listener;
            state.listeners.insert(id, listener);
            id
        };
        let weak = self.weak_self.clone();
        SettingsDocumentSubscription {
            callback: Rc::new(RefCell::new(Some(Box::new(move || {
                if let Some(store) = weak.upgrade() {
                    store.state.borrow_mut().listeners.remove(&id);
                }
            })))),
        }
    }

    /// Loads whether the current provider owns a local document.
    pub fn load(self: &Rc<Self>) -> LocalBoxFuture<'static, Result<(), String>> {
        let generation = {
            let mut state = self.state.borrow_mut();
            state.generation = state.generation.wrapping_add(1);
            state.generation
        };
        if let Err(error) = self.update(|state| {
            state.status = SettingsDocumentStatus::Loading;
            state.error = None;
        }) {
            return futures::future::ready(Err(error)).boxed_local();
        }
        let store = self.clone();
        async move {
            let response = store.transport.describe().await;
            if generation != store.state.borrow().generation {
                return Ok(());
            }
            match response {
                SettingsDocumentCall::Success(description) => store.update(|state| {
                    state.status = if description.has_document {
                        SettingsDocumentStatus::Ready
                    } else {
                        SettingsDocumentStatus::Unavailable
                    };
                    state.error = None;
                }),
                SettingsDocumentCall::Rejected(message) | SettingsDocumentCall::Failed(message) => {
                    store.update(|state| {
                        state.status = SettingsDocumentStatus::Unavailable;
                        state.error = Some(message);
                    })
                }
            }
        }
        .boxed_local()
    }

    /// Opens the loaded document once; concurrent gestures collapse behind the crossing action.
    pub fn open(self: &Rc<Self>) -> LocalBoxFuture<'static, Result<(), String>> {
        let current = self.snapshot();
        if current.status != SettingsDocumentStatus::Ready || current.opening {
            return futures::future::ready(Ok(())).boxed_local();
        }
        if let Err(error) = self.update(|state| {
            state.opening = true;
            state.error = None;
        }) {
            return futures::future::ready(Err(error)).boxed_local();
        }
        let store = self.clone();
        async move {
            let response = store.transport.open_document().await;
            let failure = match response {
                SettingsDocumentCall::Success(()) => None,
                SettingsDocumentCall::Rejected(message) | SettingsDocumentCall::Failed(message) => {
                    Some(message)
                }
            };
            let failure_result = if let Some(message) = failure {
                store.update(|state| state.error = Some(message))
            } else {
                Ok(())
            };
            let final_result = store.update(|state| state.opening = false);
            failure_result.and(final_result)
        }
        .boxed_local()
    }

    fn update(&self, update: impl FnOnce(&mut SettingsDocumentState)) -> Result<(), String> {
        let listeners = {
            let mut state = self.state.borrow_mut();
            let mut next = state.snapshot.as_ref().clone();
            update(&mut next);
            state.snapshot = Rc::new(next);
            state.listeners.values().cloned().collect::<Vec<_>>()
        };
        for listener in listeners {
            listener()?;
        }
        Ok(())
    }
}

/// Refreshes document availability after reconnect only after a surface requested it.
pub fn refresh_document_if_loaded(
    controller: Option<&Rc<SettingsDocumentStore>>,
    spawner: &dyn SettingsDocumentTaskSpawner,
) {
    let Some(controller) = controller else {
        return;
    };
    if controller.snapshot().status == SettingsDocumentStatus::Idle {
        return;
    }
    let refresh = controller.load();
    spawner.spawn(
        async move {
            let _ = refresh.await;
        }
        .boxed_local(),
    );
}
