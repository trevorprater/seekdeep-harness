//! Target-portable settings namespace read/write queue and snapshot owner.

use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    rc::{Rc, Weak},
};

use futures::{FutureExt, channel::oneshot, future::LocalBoxFuture};
use seekdeep_client_settings_contract::{
    ClientSettingsDisposer, ClientSettingsMode, ClientSettingsScope, ClientSettingsScopeSnapshot,
    ClientSettingsScopeSpec, ClientSettingsStatus,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Successful or business-rejected generated RPC result.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingsRpcResult<T> {
    /// The Host accepted the operation.
    Success(T),
    /// The Host returned a structured business failure.
    Rejected,
}

/// Redacted wire view of one registered settings namespace.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientSettingsNamespaceView {
    /// Exact namespace identity.
    pub ns: String,
    /// Serialized Schemastery envelope.
    pub schema: Value,
    /// Redacted resolved section.
    pub value: Value,
    /// Optional redacted composition base; explicit JSON null remains present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<Value>,
    /// Optional redacted raw user section; explicit JSON null remains present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<Value>,
    /// Monotonic raw-user-section revision.
    pub revision: f64,
}

/// Successful `settings.describe` value used by the Client scope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientSettingsDescribeValue {
    /// Whether the current provider accepts writes.
    pub writable: bool,
    /// Every namespace exposed to this browser.
    pub namespaces: Vec<ClientSettingsNamespaceView>,
}

/// One path-addressed namespace mutation.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum ClientSettingsPathOperation {
    /// Replace one field value.
    Set {
        /// Path from the namespace root.
        path: Vec<String>,
        /// Exact JSON-compatible replacement.
        value: Value,
    },
    /// Remove one field from the user layer.
    Unset {
        /// Path from the namespace root.
        path: Vec<String>,
    },
}

/// Revision-fenced `settings.mutate` request.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientSettingsMutateRequest {
    /// Namespace being changed.
    pub ns: String,
    /// Ordered path operations; the source Client sends one per request.
    pub ops: Vec<ClientSettingsPathOperation>,
    /// Last accepted namespace revision, when one is known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<f64>,
}

/// Injected generated settings API boundary.
pub trait ClientSettingsTransport {
    /// Reads every currently exposed namespace.
    fn describe(
        &self,
    ) -> LocalBoxFuture<'static, Result<SettingsRpcResult<ClientSettingsDescribeValue>, String>>;

    /// Applies one revision-fenced path operation.
    fn mutate(
        &self,
        request: ClientSettingsMutateRequest,
    ) -> LocalBoxFuture<'static, Result<SettingsRpcResult<ClientSettingsNamespaceView>, String>>;
}

/// Injected owner for eager local queue work.
pub trait ClientSettingsTaskSpawner {
    /// Owns one queue drain until it reaches quiescence.
    fn spawn(&self, task: LocalBoxFuture<'static, ()>);
}

/// Observable operation failure. Transport and Host rejections are recovered or swallowed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ClientSettingsOperationError(String);

impl ClientSettingsOperationError {
    /// Creates one exact decoder or subscriber diagnostic.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

type FallibleListener = Rc<dyn Fn() -> Result<(), ClientSettingsOperationError>>;
type Completion = oneshot::Sender<Result<(), ClientSettingsOperationError>>;

enum QueuedOperationKind {
    Read {
        generation: u64,
    },
    Write {
        generation: u64,
        operation: ClientSettingsPathOperation,
    },
}

struct QueuedOperation {
    kind: QueuedOperationKind,
    completion: Completion,
}

struct ControllerState {
    snapshot: Rc<ClientSettingsScopeSnapshot<Value>>,
    listeners: BTreeMap<u64, FallibleListener>,
    next_listener: u64,
    queue: VecDeque<QueuedOperation>,
    draining: bool,
    read_generation: u64,
    write_generation: u64,
    disposed: bool,
    dispose_waiters: Vec<oneshot::Sender<()>>,
}

/// Serializes one namespace's Host reads and writes behind a stable snapshot.
pub struct ClientSettingsScopeController {
    weak_self: Weak<Self>,
    transport: Rc<dyn ClientSettingsTransport>,
    spawner: Rc<dyn ClientSettingsTaskSpawner>,
    spec: ClientSettingsScopeSpec<Value>,
    mode: ClientSettingsMode,
    state: RefCell<ControllerState>,
}

impl std::fmt::Debug for ClientSettingsScopeController {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.borrow();
        formatter
            .debug_struct("ClientSettingsScopeController")
            .field("namespace", &self.spec.namespace)
            .field("mode", &self.mode)
            .field("queued", &state.queue.len())
            .field("draining", &state.draining)
            .field("disposed", &state.disposed)
            .finish_non_exhaustive()
    }
}

impl ClientSettingsScopeController {
    /// Creates a Host-backed or inert memory-backed namespace scope.
    #[must_use]
    pub fn new(
        transport: Rc<dyn ClientSettingsTransport>,
        spawner: Rc<dyn ClientSettingsTaskSpawner>,
        spec: ClientSettingsScopeSpec<Value>,
        mode: ClientSettingsMode,
    ) -> Rc<Self> {
        Rc::new_cyclic(|weak_self| Self {
            weak_self: weak_self.clone(),
            transport,
            spawner,
            spec,
            mode,
            state: RefCell::new(ControllerState {
                snapshot: Rc::new(ClientSettingsScopeSnapshot {
                    status: match mode {
                        ClientSettingsMode::Host => ClientSettingsStatus::Loading,
                        ClientSettingsMode::Memory => ClientSettingsStatus::Unavailable,
                    },
                    value: None,
                    base: None,
                    user: None,
                    revision: None,
                    writable: false,
                    mode,
                }),
                listeners: BTreeMap::new(),
                next_listener: 0,
                queue: VecDeque::new(),
                draining: false,
                read_generation: 0,
                write_generation: 0,
                disposed: false,
                dispose_waiters: Vec::new(),
            }),
        })
    }

    /// Current reference-stable snapshot.
    #[must_use]
    pub fn snapshot(&self) -> Rc<ClientSettingsScopeSnapshot<Value>> {
        self.state.borrow().snapshot.clone()
    }

    /// Observes committed snapshot replacements, including fallible browser listeners.
    #[must_use]
    pub fn subscribe_fallible(&self, listener: FallibleListener) -> ClientSettingsDisposer {
        let id = {
            let mut state = self.state.borrow_mut();
            state.next_listener = state.next_listener.wrapping_add(1);
            let id = state.next_listener;
            state.listeners.insert(id, listener);
            id
        };
        let weak = self.weak_self.clone();
        ClientSettingsDisposer::new(move || {
            if let Some(controller) = weak.upgrade() {
                controller.state.borrow_mut().listeners.remove(&id);
            }
        })
    }

    /// Eagerly queues a Host refresh.
    pub fn load(&self) -> LocalBoxFuture<'static, Result<(), ClientSettingsOperationError>> {
        let generation = {
            let mut state = self.state.borrow_mut();
            state.read_generation = state.read_generation.wrapping_add(1);
            state.read_generation
        };
        self.enqueue(QueuedOperationKind::Read { generation })
    }

    /// Eagerly queues one field replacement.
    pub fn set_field(
        &self,
        field: impl Into<String>,
        value: Value,
    ) -> LocalBoxFuture<'static, Result<(), ClientSettingsOperationError>> {
        self.write(ClientSettingsPathOperation::Set {
            path: vec![field.into()],
            value,
        })
    }

    /// Eagerly queues one user-layer field removal.
    pub fn unset_field(
        &self,
        field: impl Into<String>,
    ) -> LocalBoxFuture<'static, Result<(), ClientSettingsOperationError>> {
        self.write(ClientSettingsPathOperation::Unset {
            path: vec![field.into()],
        })
    }

    /// Stops queued and future operations and waits for the crossing call to settle.
    pub fn dispose(&self) -> LocalBoxFuture<'static, ()> {
        let receiver = {
            let mut state = self.state.borrow_mut();
            state.disposed = true;
            state.read_generation = state.read_generation.wrapping_add(1);
            state.write_generation = state.write_generation.wrapping_add(1);
            if state.draining {
                let (sender, receiver) = oneshot::channel();
                state.dispose_waiters.push(sender);
                Some(receiver)
            } else {
                None
            }
        };
        async move {
            if let Some(receiver) = receiver {
                let _ = receiver.await;
            }
        }
        .boxed_local()
    }

    /// Synchronously prevents future publication when assembly fails before ownership installs.
    pub fn cancel(&self) {
        let mut state = self.state.borrow_mut();
        state.disposed = true;
        state.read_generation = state.read_generation.wrapping_add(1);
        state.write_generation = state.write_generation.wrapping_add(1);
    }

    fn write(
        &self,
        operation: ClientSettingsPathOperation,
    ) -> LocalBoxFuture<'static, Result<(), ClientSettingsOperationError>> {
        let generation = {
            let mut state = self.state.borrow_mut();
            state.read_generation = state.read_generation.wrapping_add(1);
            state.write_generation = state.write_generation.wrapping_add(1);
            state.write_generation
        };
        self.enqueue(QueuedOperationKind::Write {
            generation,
            operation,
        })
    }

    fn enqueue(
        &self,
        kind: QueuedOperationKind,
    ) -> LocalBoxFuture<'static, Result<(), ClientSettingsOperationError>> {
        if self.mode == ClientSettingsMode::Memory || self.state.borrow().disposed {
            return futures::future::ready(Ok(())).boxed_local();
        }
        let Some(owner) = self.weak_self.upgrade() else {
            return futures::future::ready(Err(ClientSettingsOperationError::new(
                "ui-settings: controller must remain Rc-owned while operations are queued",
            )))
            .boxed_local();
        };
        let (sender, receiver) = oneshot::channel();
        let should_spawn = {
            let mut state = self.state.borrow_mut();
            state.queue.push_back(QueuedOperation {
                kind,
                completion: sender,
            });
            if state.draining {
                false
            } else {
                state.draining = true;
                true
            }
        };
        if should_spawn {
            self.spawner
                .spawn(async move { owner.drain().await }.boxed_local());
        }
        async move { receiver.await.unwrap_or(Ok(())) }.boxed_local()
    }

    async fn drain(self: Rc<Self>) {
        loop {
            let next = {
                let mut state = self.state.borrow_mut();
                state.queue.pop_front().map(|operation| {
                    let skip = state.disposed;
                    (operation, skip)
                })
            };
            let Some((operation, skip)) = next else {
                let waiters = {
                    let mut state = self.state.borrow_mut();
                    state.draining = false;
                    std::mem::take(&mut state.dispose_waiters)
                };
                for waiter in waiters {
                    let _ = waiter.send(());
                }
                return;
            };
            let result = if skip {
                Ok(())
            } else {
                self.execute(operation.kind).await
            };
            let _ = operation.completion.send(result);
        }
    }

    async fn execute(
        self: &Rc<Self>,
        operation: QueuedOperationKind,
    ) -> Result<(), ClientSettingsOperationError> {
        match operation {
            QueuedOperationKind::Read { generation } => self.read(generation).await,
            QueuedOperationKind::Write {
                generation,
                operation,
            } => self.execute_write(generation, operation).await,
        }
    }

    async fn read(self: &Rc<Self>, generation: u64) -> Result<(), ClientSettingsOperationError> {
        let Ok(SettingsRpcResult::Success(description)) = self.transport.describe().await else {
            return Ok(());
        };
        if self.state.borrow().disposed {
            return Ok(());
        }
        let view = description
            .namespaces
            .into_iter()
            .find(|candidate| candidate.ns == self.spec.namespace.as_str());
        let publish = generation == self.state.borrow().read_generation;
        let Some(view) = view else {
            if publish {
                let mut next = self.snapshot().as_ref().clone();
                next.status = ClientSettingsStatus::Unavailable;
                next.writable = description.writable;
                self.publish(next)?;
            }
            return Ok(());
        };
        self.accept(view, publish, Some(description.writable))
    }

    async fn execute_write(
        self: &Rc<Self>,
        generation: u64,
        operation: ClientSettingsPathOperation,
    ) -> Result<(), ClientSettingsOperationError> {
        let request = ClientSettingsMutateRequest {
            ns: self.spec.namespace.as_str().to_owned(),
            ops: vec![operation],
            expected_revision: self.snapshot().revision,
        };
        let response = self.transport.mutate(request).await;
        let Ok(SettingsRpcResult::Success(view)) = response else {
            if self.latest_write(generation) {
                let read_generation = {
                    let mut state = self.state.borrow_mut();
                    state.read_generation = state.read_generation.wrapping_add(1);
                    state.read_generation
                };
                self.read(read_generation).await?;
            }
            return Ok(());
        };
        let publish = generation == self.state.borrow().write_generation;
        self.accept(view, publish, None)
    }

    fn latest_write(&self, generation: u64) -> bool {
        let state = self.state.borrow();
        !state.disposed && generation == state.write_generation
    }

    fn accept(
        &self,
        view: ClientSettingsNamespaceView,
        publish: bool,
        writable: Option<bool>,
    ) -> Result<(), ClientSettingsOperationError> {
        let decoded = if publish { self.decode(&view)? } else { None };
        let mut next = self.snapshot().as_ref().clone();
        next.revision = Some(view.revision);
        next.base = view.base;
        next.user = view.user;
        if let Some(writable) = writable {
            next.writable = writable;
        }
        if let Some(decoded) = decoded {
            next.status = ClientSettingsStatus::Ready;
            next.value = Some(Rc::new(decoded));
        }
        self.publish(next)
    }

    fn decode(
        &self,
        view: &ClientSettingsNamespaceView,
    ) -> Result<Option<Value>, ClientSettingsOperationError> {
        if let Some(decode) = &self.spec.decode {
            return decode(&view.value).map_err(ClientSettingsOperationError::new);
        }
        if !view.value.is_object() {
            return Ok(None);
        }
        let Ok(schema) = seekdeep_schemastery::Schema::from_json(&view.schema) else {
            return Ok(None);
        };
        Ok(schema
            .resolve(&view.value)
            .is_ok()
            .then(|| view.value.clone()))
    }

    fn publish(
        &self,
        snapshot: ClientSettingsScopeSnapshot<Value>,
    ) -> Result<(), ClientSettingsOperationError> {
        let listeners = {
            let mut state = self.state.borrow_mut();
            state.snapshot = Rc::new(snapshot);
            state.listeners.values().cloned().collect::<Vec<_>>()
        };
        for listener in listeners {
            listener()?;
        }
        Ok(())
    }
}

impl ClientSettingsScope<Value> for ClientSettingsScopeController {
    fn snapshot(&self) -> Rc<ClientSettingsScopeSnapshot<Value>> {
        ClientSettingsScopeController::snapshot(self)
    }

    fn subscribe(&self, listener: Rc<dyn Fn()>) -> ClientSettingsDisposer {
        self.subscribe_fallible(Rc::new(move || {
            listener();
            Ok(())
        }))
    }

    fn set(&self, field: String, value: Value) -> LocalBoxFuture<'static, Result<(), String>> {
        self.set_field(field, value)
            .map(|result| result.map_err(|error| error.to_string()))
            .boxed_local()
    }

    fn unset(&self, field: String) -> LocalBoxFuture<'static, Result<(), String>> {
        self.unset_field(field)
            .map(|result| result.map_err(|error| error.to_string()))
            .boxed_local()
    }
}
