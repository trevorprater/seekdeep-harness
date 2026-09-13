//! `SQLite` storage backend: one database hosts every routed KV unit.

mod invariant;
mod schema;
mod unit;

pub use invariant::{INVARIANT_NAME, register_invariant};
pub use schema::{JournalMode, STORAGE_SQLITE_SCHEMA_VERSION, open_database, record_table_name};

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

use futures::{FutureExt as _, future::BoxFuture};
use parking_lot::Mutex;
use rusqlite::OptionalExtension as _;
use seekdeep_cordis::{
    Context, Plugin,
    fiber::{DisposeFuture, EffectHandle},
};
use seekdeep_storage::{
    BackendRegistration, KvFacet, KvUnit, KvUnitDescriptor, STORAGE, StorageBackend, StorageError,
    StorageErrorCode, UNIT_NAME_RE, storage_backend_service_key,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Notify, OnceCell};

use unit::{SharedDatabase, SqliteKvUnit};

/// Stable backend registration name.
pub const BACKEND_NAME: &str = "sqlite";
/// Cordis plugin name.
pub const NAME: &str = "storage-sqlite";
/// The backend registers on the storage hub.
pub const INJECT: &[&str] = &["storage"];

/// `SQLite` backend plugin configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SqliteStorageConfig {
    /// Database filename, or `:memory:`.
    pub path: String,
    /// Durable journal strategy.
    #[serde(default)]
    pub journal_mode: JournalMode,
}

#[derive(Debug)]
enum InitFailure {
    Storage(StorageError),
    Io {
        kind: std::io::ErrorKind,
        raw: Option<i32>,
        message: String,
    },
    Other(String),
}

impl InitFailure {
    fn capture(error: &anyhow::Error) -> Self {
        if let Some(storage) = error.downcast_ref::<StorageError>() {
            Self::Storage(storage.clone())
        } else if let Some(io) = error.downcast_ref::<std::io::Error>() {
            Self::Io {
                kind: io.kind(),
                raw: io.raw_os_error(),
                message: io.to_string(),
            }
        } else {
            Self::Other(format!("{error:#}"))
        }
    }

    fn error(&self) -> anyhow::Error {
        match self {
            Self::Storage(error) => error.clone().into(),
            Self::Io {
                kind: _,
                raw: Some(raw),
                ..
            } => std::io::Error::from_raw_os_error(*raw).into(),
            Self::Io { kind, message, .. } => std::io::Error::new(*kind, message.clone()).into(),
            Self::Other(message) => anyhow::anyhow!(message.clone()),
        }
    }
}

#[derive(Debug, Default)]
struct BackendState {
    opening: HashSet<String>,
    units: HashMap<String, Arc<SqliteKvUnit>>,
}

#[derive(Debug, Default)]
struct CloseState {
    result: Mutex<Option<Result<(), StorageError>>>,
    notify: Notify,
}

impl CloseState {
    fn complete(&self, result: Result<(), StorageError>) {
        *self.result.lock() = Some(result);
        self.notify.notify_waiters();
    }

    async fn wait(&self) -> Result<(), StorageError> {
        loop {
            let notified = self.notify.notified();
            if let Some(result) = self.result.lock().clone() {
                return result;
            }
            notified.await;
        }
    }
}

#[derive(Debug)]
pub(crate) struct BackendInner {
    config: SqliteStorageConfig,
    database: OnceCell<Result<SharedDatabase, Arc<InitFailure>>>,
    state: Mutex<BackendState>,
    changed: Notify,
    closed: AtomicBool,
    close_state: Arc<CloseState>,
}

/// `SQLite` backend owning one connection and one-open-handle per unit name.
#[derive(Clone)]
pub struct SqliteStorageBackend {
    inner: Arc<BackendInner>,
}

impl std::fmt::Debug for SqliteStorageBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.inner.state.lock();
        formatter
            .debug_struct("SqliteStorageBackend")
            .field("config", &self.inner.config)
            .field("opening", &state.opening)
            .field("units", &state.units.keys().collect::<Vec<_>>())
            .field("closed", &self.inner.closed.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl SqliteStorageBackend {
    /// Starts one backend over the configured medium.
    #[must_use]
    pub fn new(config: SqliteStorageConfig) -> Arc<Self> {
        let backend = Arc::new(Self {
            inner: Arc::new(BackendInner {
                config,
                database: OnceCell::new(),
                state: Mutex::new(BackendState::default()),
                changed: Notify::new(),
                closed: AtomicBool::new(false),
                close_state: Arc::new(CloseState::default()),
            }),
        });
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let inner = backend.inner.clone();
            runtime.spawn(async move {
                // Every primitive re-awaits the same cell. Observing the
                // result here only mirrors the source's handled ready promise.
                let _ = database(&inner).await;
            });
        }
        backend
    }

    fn open(
        &self,
        descriptor: KvUnitDescriptor,
    ) -> BoxFuture<'static, anyhow::Result<Arc<dyn KvUnit>>> {
        if self.inner.closed.load(Ordering::Acquire) {
            return futures::future::ready(Err(StorageError::new(
                StorageErrorCode::Closed,
                "sqlite storage backend is closed",
            )
            .into()))
            .boxed();
        }
        if !UNIT_NAME_RE.is_match(&descriptor.name) {
            return futures::future::ready(Err(anyhow::anyhow!(
                "kv unit name '{}' violates {}",
                descriptor.name,
                UNIT_NAME_RE.as_str()
            )))
            .boxed();
        }
        if let Some(table) = descriptor
            .tables
            .iter()
            .find(|table| !UNIT_NAME_RE.is_match(table))
        {
            return futures::future::ready(Err(anyhow::anyhow!(
                "kv table name '{table}' in unit '{}' violates {}",
                descriptor.name,
                UNIT_NAME_RE.as_str()
            )))
            .boxed();
        }
        {
            let mut state = self.inner.state.lock();
            if state.units.contains_key(&descriptor.name)
                || !state.opening.insert(descriptor.name.clone())
            {
                return futures::future::ready(Err(anyhow::anyhow!(
                    "kv unit '{}' is already open (double-open is a caller bug)",
                    descriptor.name
                )))
                .boxed();
            }
        }
        let inner = self.inner.clone();
        eager(async move { finish_open(inner, descriptor).await })
    }

    /// Marks the backend closed synchronously and eagerly drains pending opens.
    #[must_use]
    pub fn close_eager(&self) -> BoxFuture<'static, Result<(), StorageError>> {
        if !self.inner.closed.swap(true, Ordering::AcqRel) {
            let inner = self.inner.clone();
            tokio::spawn(async move {
                let result = close_inner(&inner).await;
                inner.close_state.complete(result);
            });
        }
        let close_state = self.inner.close_state.clone();
        async move { close_state.wait().await }.boxed()
    }
}

impl KvFacet for SqliteStorageBackend {
    fn open(
        &self,
        descriptor: KvUnitDescriptor,
    ) -> BoxFuture<'static, anyhow::Result<Arc<dyn KvUnit>>> {
        self.open(descriptor)
    }
}

impl StorageBackend for SqliteStorageBackend {
    fn kv(&self) -> Option<Arc<dyn KvFacet>> {
        Some(Arc::new(self.clone()))
    }

    fn close(&self) -> BoxFuture<'static, Result<(), StorageError>> {
        self.close_eager()
    }
}

async fn database(inner: &Arc<BackendInner>) -> anyhow::Result<SharedDatabase> {
    let config = inner.config.clone();
    let result = inner
        .database
        .get_or_init(|| async move {
            tokio::task::spawn_blocking(move || {
                open_database(&config.path, config.journal_mode)
                    .map(|database| Arc::new(Mutex::new(Some(database))))
                    .map_err(|error| Arc::new(InitFailure::capture(&error)))
            })
            .await
            .map_err(|error| Arc::new(InitFailure::Other(error.to_string())))?
        })
        .await;
    result
        .as_ref()
        .map(Arc::clone)
        .map_err(|failure| failure.error())
}

async fn finish_open(
    inner: Arc<BackendInner>,
    descriptor: KvUnitDescriptor,
) -> anyhow::Result<Arc<dyn KvUnit>> {
    let name = descriptor.name.clone();
    let result = async {
        let database = database(&inner).await?;
        materialize_unit(&database, &descriptor)?;
        let unit = SqliteKvUnit::new(database, descriptor, Arc::downgrade(&inner));
        inner.state.lock().units.insert(name.clone(), unit.clone());
        Ok(unit as Arc<dyn KvUnit>)
    }
    .await;
    inner.state.lock().opening.remove(&name);
    inner.changed.notify_waiters();
    result
}

fn materialize_unit(
    database: &SharedDatabase,
    descriptor: &KvUnitDescriptor,
) -> anyhow::Result<()> {
    let database = database.lock();
    let database = database.as_ref().ok_or_else(|| {
        StorageError::new(StorageErrorCode::Closed, "sqlite storage backend is closed")
    })?;
    let on_disk = database
        .query_row(
            "SELECT version FROM units WHERE name = ?1",
            [&descriptor.name],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let version = i64::try_from(descriptor.version).map_err(|_| {
        anyhow::anyhow!(
            "kv unit version {} exceeds SQLite INTEGER",
            descriptor.version
        )
    })?;
    match on_disk {
        None => {
            database.execute(
                "INSERT INTO units (name, version) VALUES (?1, ?2)",
                rusqlite::params![descriptor.name, version],
            )?;
        }
        Some(on_disk) if on_disk != version => {
            return Err(StorageError::new(
                StorageErrorCode::VersionMismatch,
                format!(
                    "kv unit '{}' is stamped version {on_disk} on the medium, incompatible with descriptor version {}",
                    descriptor.name, descriptor.version
                ),
            )
            .into());
        }
        Some(_) => {}
    }
    for table in &descriptor.tables {
        let physical = record_table_name(&descriptor.name, table);
        database.execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS \"{physical}\" (\
             key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT"
        ))?;
    }
    Ok(())
}

async fn close_inner(inner: &Arc<BackendInner>) -> Result<(), StorageError> {
    if database(inner).await.is_err() {
        return Ok(());
    }
    loop {
        let notified = inner.changed.notified();
        let units = {
            let state = inner.state.lock();
            state
                .opening
                .is_empty()
                .then(|| state.units.values().cloned().collect::<Vec<_>>())
        };
        if let Some(units) = units {
            for unit in units {
                unit.close().await?;
            }
            break;
        }
        notified.await;
    }
    if let Some(Ok(database)) = inner.database.get() {
        let connection = database.lock().take();
        if let Some(connection) = connection {
            connection.close().map_err(|(_, error)| {
                StorageError::with_source(
                    StorageErrorCode::Closed,
                    "failed to close sqlite storage backend",
                    error.into(),
                )
            })?;
        }
    }
    Ok(())
}

pub(crate) fn release_unit(backend: &Weak<BackendInner>, name: &str) {
    if let Some(backend) = backend.upgrade() {
        backend.state.lock().units.remove(name);
        backend.changed.notify_waiters();
    }
}

fn eager<T>(future: impl std::future::Future<Output = T> + Send + 'static) -> BoxFuture<'static, T>
where
    T: Send + 'static,
{
    let (send, receive) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _ = send.send(future.await);
    });
    async move { receive.await.expect("eager SQLite backend task stopped") }.boxed()
}

/// Reversible mounted `SQLite` backend contribution.
pub struct SqliteBackendMount {
    backend: Arc<SqliteStorageBackend>,
    registration: BackendRegistration,
    service: EffectHandle,
}

impl std::fmt::Debug for SqliteBackendMount {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SqliteBackendMount")
            .field("backend", &self.backend)
            .finish_non_exhaustive()
    }
}

impl SqliteBackendMount {
    /// Unregisters, closes, then withdraws the lifecycle seat.
    ///
    /// # Errors
    ///
    /// Returns close or service-disposal failures.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        self.registration.dispose();
        self.backend.close().await?;
        self.service.dispose().await
    }
}

/// Registers the backend directly on an active storage context.
///
/// # Errors
///
/// Returns missing-storage, duplicate-registration, or service failures.
pub fn mount(context: &Context, config: SqliteStorageConfig) -> anyhow::Result<SqliteBackendMount> {
    let storage = context
        .get(STORAGE)
        .ok_or_else(|| anyhow::anyhow!("storage service is required"))?;
    let backend = SqliteStorageBackend::new(config);
    let registration = storage
        .backend
        .register(BACKEND_NAME, backend.clone() as Arc<dyn StorageBackend>)?;
    match context.provide_named(&storage_backend_service_key(BACKEND_NAME), backend.clone()) {
        Ok(service) => Ok(SqliteBackendMount {
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

/// Builds the source-compatible storage-sqlite Cordis plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: SqliteStorageConfig = serde_json::from_value(config)?;
            let storage = context
                .get(STORAGE)
                .ok_or_else(|| anyhow::anyhow!("storage service is required"))?;
            let backend = SqliteStorageBackend::new(config);
            let registration = storage
                .backend
                .register(BACKEND_NAME, backend.clone() as Arc<dyn StorageBackend>)?;
            let cleanup_backend = backend.clone();
            let cleanup_registration = registration.clone();
            let cleanup = EffectHandle::new(
                "storage-sqlite.registerBackend",
                move || -> DisposeFuture {
                    Box::pin(async move {
                        cleanup_registration.dispose();
                        cleanup_backend.close().await?;
                        Ok(())
                    })
                },
            );
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
        let config: SqliteStorageConfig = serde_json::from_value(value.clone())?;
        anyhow::ensure!(!config.path.is_empty(), "path must not be empty");
        Ok(serde_json::to_value(config)?)
    })
}
