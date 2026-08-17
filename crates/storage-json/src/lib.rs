//! Atomic, human-readable, one-JSON-file-per-unit storage backend.

mod atomic;
mod format;
mod unit;

pub use atomic::write_atomic;
pub use format::{UnitState, parse, serialize};

use std::{
    collections::{HashMap, HashSet},
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, Weak},
};

use futures::{FutureExt as _, future::BoxFuture, future::join_all};
use parking_lot::Mutex;
use seekdeep_cordis::{
    Context, Plugin,
    fiber::{DisposeFuture, EffectHandle},
};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_storage::{
    BackendRegistration, KvFacet, KvUnit, KvUnitDescriptor, STORAGE, StorageBackend, StorageError,
    StorageErrorCode, UNIT_NAME_RE, storage_backend_service_key,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Notify, oneshot};

/// Stable backend name.
pub const BACKEND_NAME: &str = "json";
/// Cordis plugin name.
pub const NAME: &str = "storage-json";
/// The hub must exist before backend registration.
pub const INJECT: &[&str] = &["storage"];
/// Stable invariant companion name.
pub const INVARIANT_NAME: &str = "storage-json-invariant";
const PACKAGE_NAME: &str = "@deepseek-ai/seekdeep-storage-json";

/// Explicit file-tree root configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonStorageConfig {
    /// Directory holding one `<unit>.json` file per unit.
    pub root: PathBuf,
}

#[derive(Debug, Default)]
struct BackendState {
    closed: bool,
    opening: HashSet<String>,
    open: HashMap<String, Weak<unit::JsonKvUnit>>,
}

#[derive(Debug)]
pub(crate) struct BackendInner {
    root: PathBuf,
    state: Mutex<BackendState>,
    changed: Notify,
}

/// JSON backend rooted at one explicit directory.
#[derive(Clone)]
pub struct JsonStorageBackend {
    inner: Arc<BackendInner>,
}

impl fmt::Debug for JsonStorageBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.inner.state.lock();
        formatter
            .debug_struct("JsonStorageBackend")
            .field("root", &self.inner.root)
            .field("closed", &state.closed)
            .field("opening", &state.opening)
            .field("open", &state.open.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl JsonStorageBackend {
    /// Creates an unmounted backend over an explicit file-tree root.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(BackendInner {
                root: root.into(),
                state: Mutex::new(BackendState::default()),
                changed: Notify::new(),
            }),
        })
    }

    fn open(
        &self,
        descriptor: KvUnitDescriptor,
    ) -> BoxFuture<'static, anyhow::Result<Arc<dyn KvUnit>>> {
        if let Err(error) = validate_descriptor(&descriptor) {
            return async move { Err(error.into()) }.boxed();
        }
        {
            let mut state = self.inner.state.lock();
            if state.closed {
                return async {
                    Err(
                        StorageError::new(StorageErrorCode::Closed, "json backend is closed")
                            .into(),
                    )
                }
                .boxed();
            }
            if state.open.contains_key(&descriptor.name)
                || !state.opening.insert(descriptor.name.clone())
            {
                let name = descriptor.name;
                return async move {
                    anyhow::bail!(
                        "unit '{name}' is already open; a unit has exactly one live handle"
                    )
                }
                .boxed();
            }
        }
        let inner = self.inner.clone();
        eager(async move { finish_open(inner, descriptor).await })
    }

    /// Marks the backend closed synchronously, then drains opens and units eagerly.
    #[must_use]
    pub fn close_eager(&self) -> BoxFuture<'static, Result<(), StorageError>> {
        self.inner.state.lock().closed = true;
        let inner = self.inner.clone();
        let (send, receive) = oneshot::channel();
        tokio::spawn(async move {
            close_inner(&inner).await;
            let _ = send.send(());
        });
        async move {
            receive.await.map_err(|_| {
                StorageError::new(StorageErrorCode::Closed, "json backend close task stopped")
            })
        }
        .boxed()
    }
}

impl KvFacet for JsonStorageBackend {
    fn open(
        &self,
        descriptor: KvUnitDescriptor,
    ) -> BoxFuture<'static, anyhow::Result<Arc<dyn KvUnit>>> {
        self.open(descriptor)
    }
}

impl StorageBackend for JsonStorageBackend {
    fn kv(&self) -> Option<Arc<dyn KvFacet>> {
        Some(Arc::new(self.clone()))
    }

    fn close(&self) -> BoxFuture<'static, Result<(), StorageError>> {
        self.close_eager()
    }
}

async fn finish_open(
    inner: Arc<BackendInner>,
    descriptor: KvUnitDescriptor,
) -> anyhow::Result<Arc<dyn KvUnit>> {
    let name = descriptor.name.clone();
    let result = async {
        create_root(&inner.root).await?;
        let path = inner.root.join(format!("{name}.json"));
        let backend = Arc::downgrade(&inner);
        let unit = unit::open_json_unit(descriptor, path, backend).await?;
        let closed = {
            let mut state = inner.state.lock();
            if !state.closed {
                state.open.insert(name.clone(), Arc::downgrade(&unit));
            }
            state.closed
        };
        if closed {
            unit.close().await?;
            return Err(
                StorageError::new(StorageErrorCode::Closed, "json backend is closed").into(),
            );
        }
        Ok(unit as Arc<dyn KvUnit>)
    }
    .await;
    inner.state.lock().opening.remove(&name);
    inner.changed.notify_waiters();
    result
}

async fn close_inner(inner: &Arc<BackendInner>) {
    loop {
        let notified = inner.changed.notified();
        let units = {
            let state = inner.state.lock();
            state.opening.is_empty().then(|| {
                state
                    .open
                    .values()
                    .filter_map(Weak::upgrade)
                    .collect::<Vec<_>>()
            })
        };
        if let Some(units) = units {
            let _ = join_all(units.into_iter().map(|unit| unit.close())).await;
            return;
        }
        notified.await;
    }
}

pub(crate) fn release_unit(backend: &Weak<BackendInner>, name: &str) {
    if let Some(backend) = backend.upgrade() {
        backend.state.lock().open.remove(name);
        backend.changed.notify_waiters();
    }
}

fn validate_descriptor(descriptor: &KvUnitDescriptor) -> Result<(), StorageError> {
    if !UNIT_NAME_RE.is_match(&descriptor.name) {
        return Err(StorageError::new(
            StorageErrorCode::MalformedMedium,
            format!("invalid unit name '{}'", descriptor.name),
        ));
    }
    if let Some(table) = descriptor
        .tables
        .iter()
        .find(|table| !UNIT_NAME_RE.is_match(table))
    {
        return Err(StorageError::new(
            StorageErrorCode::MalformedMedium,
            format!("invalid table name '{table}' in unit '{}'", descriptor.name),
        ));
    }
    Ok(())
}

async fn create_root(path: &Path) -> std::io::Result<()> {
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(path).await
}

fn eager<T>(future: impl std::future::Future<Output = T> + Send + 'static) -> BoxFuture<'static, T>
where
    T: Send + 'static,
{
    let (send, receive) = oneshot::channel();
    tokio::spawn(async move {
        let _ = send.send(future.await);
    });
    async move { receive.await.expect("eager JSON backend task stopped") }.boxed()
}

/// Reversible mounted backend contribution.
pub struct JsonBackendMount {
    backend: Arc<JsonStorageBackend>,
    registration: BackendRegistration,
    service: EffectHandle,
}

impl fmt::Debug for JsonBackendMount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JsonBackendMount")
            .field("backend", &self.backend)
            .finish_non_exhaustive()
    }
}

impl JsonBackendMount {
    /// Unregisters, closes, and removes the lifecycle service.
    ///
    /// # Errors
    ///
    /// Returns service-disposal failures after the backend has drained.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        self.registration.dispose();
        self.backend.close().await?;
        self.service.dispose().await
    }
}

/// Registers the `json` backend and lifecycle service atomically.
///
/// # Errors
///
/// Returns missing-storage, duplicate-backend, or duplicate-service failures.
pub fn mount(context: &Context, root: impl Into<PathBuf>) -> anyhow::Result<JsonBackendMount> {
    let storage = context
        .get(STORAGE)
        .ok_or_else(|| anyhow::anyhow!("storage service is required"))?;
    let backend = JsonStorageBackend::new(root);
    let registration = storage.backend.register(BACKEND_NAME, backend.clone())?;
    match context.provide_named(&storage_backend_service_key(BACKEND_NAME), backend.clone()) {
        Ok(service) => Ok(JsonBackendMount {
            backend,
            registration,
            service,
        }),
        Err(error) => {
            registration.dispose();
            Err(error.into())
        }
    }
}

/// Builds the source-compatible storage-json Cordis plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: JsonStorageConfig = serde_json::from_value(config)?;
            let storage = context
                .get(STORAGE)
                .ok_or_else(|| anyhow::anyhow!("storage service is required"))?;
            let backend = JsonStorageBackend::new(config.root);
            let registration = storage
                .backend
                .register(BACKEND_NAME, backend.clone() as Arc<dyn StorageBackend>)?;
            let cleanup_registration = registration.clone();
            let cleanup_backend = backend.clone();
            let cleanup =
                EffectHandle::new("storage-json.registerBackend", move || -> DisposeFuture {
                    Box::pin(async move {
                        cleanup_registration.dispose();
                        cleanup_backend.close().await?;
                        Ok(())
                    })
                });
            if let Err(error) = context.own(cleanup) {
                registration.dispose();
                let _ = backend.close().await;
                return Err(error.into());
            }
            context.provide_named(&storage_backend_service_key(BACKEND_NAME), backend)?;
            Ok(())
        })
    })
    .with_config_validator(|value: &Value| {
        let config: JsonStorageConfig = serde_json::from_value(value.clone())?;
        anyhow::ensure!(
            !config.root.as_os_str().is_empty(),
            "root must not be empty"
        );
        Ok(serde_json::to_value(config)?)
    })
}

/// Registers the explained-empty durability invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}
