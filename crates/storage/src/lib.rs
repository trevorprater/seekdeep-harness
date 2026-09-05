//! Named storage-backend hub and backend-facing KV contract.
//!
//! The hub performs no I/O. Backends own media and optional data facets;
//! mounted data-form facilities own schemas, events, and domain semantics.

use std::{
    any::Any,
    fmt,
    sync::{Arc, LazyLock, Weak},
};

use futures::future::BoxFuture;
use indexmap::IndexMap;
use parking_lot::Mutex;
use regex::Regex;
use seekdeep_cordis::{Context, CordisError, Plugin, ServiceKey, fiber::EffectHandle};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

/// Allowed unit and table name format.
pub static UNIT_NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9_]*$").expect("static unit-name regex"));

/// Typed Cordis slot corresponding to `ctx.storage`.
pub const STORAGE: ServiceKey<Storage> = ServiceKey::new("storage");

/// Cordis service-plugin name.
pub const NAME: &str = "storage";
/// The root hub has no dependencies.
pub const INJECT: &[&str] = &[];

/// Stable invariant companion name.
pub const INVARIANT_NAME: &str = "storage-invariant";
const PACKAGE_NAME: &str = "@deepseek-ai/seekdeep-storage";

/// Stable storage failure vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageErrorCode {
    /// A named backend is absent.
    BackendNotFound,
    /// A data form is absent.
    FormNotMounted,
    /// A backend name is occupied.
    DuplicateBackend,
    /// A data-form name is occupied.
    DuplicateMount,
    /// A durable unit carries another version.
    VersionMismatch,
    /// A durable medium cannot be decoded as its declared unit.
    MalformedMedium,
    /// An operation targeted a closed unit or backend.
    Closed,
}

impl StorageErrorCode {
    /// Exact source discriminant.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BackendNotFound => "backend-not-found",
            Self::FormNotMounted => "form-not-mounted",
            Self::DuplicateBackend => "duplicate-backend",
            Self::DuplicateMount => "duplicate-mount",
            Self::VersionMismatch => "version-mismatch",
            Self::MalformedMedium => "malformed-medium",
            Self::Closed => "closed",
        }
    }
}

impl fmt::Display for StorageErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Typed hub/backend failure.
#[derive(Clone, Debug, Error)]
#[error("{message}")]
pub struct StorageError {
    /// Stable failure discriminant.
    pub code: StorageErrorCode,
    /// Diagnostic prose.
    pub message: String,
    /// Optional original failure.
    cause: Option<Arc<anyhow::Error>>,
}

impl StorageError {
    /// Creates a storage failure without a source error.
    #[must_use]
    pub fn new(code: StorageErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            cause: None,
        }
    }

    /// Creates a storage failure retaining its original error.
    #[must_use]
    pub fn with_source(
        code: StorageErrorCode,
        message: impl Into<String>,
        source: anyhow::Error,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            cause: Some(Arc::new(source)),
        }
    }

    /// Original failure retained for diagnostics, when present.
    #[must_use]
    pub fn cause(&self) -> Option<&anyhow::Error> {
        self.cause.as_deref()
    }
}

/// Static identity and shape of one KV unit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KvUnitDescriptor {
    /// Unit/file/table-prefix name.
    pub name: String,
    /// Non-negative format version.
    pub version: u64,
    /// Declared record-table names.
    pub tables: Vec<String>,
    /// Whether the unit carries a global singleton.
    pub has_global: bool,
}

impl KvUnitDescriptor {
    /// Validates unit and table identifiers at the earliest shared boundary.
    ///
    /// # Errors
    ///
    /// Returns a malformed-medium-class error for an invalid static descriptor.
    pub fn validate(&self) -> Result<(), StorageError> {
        if !UNIT_NAME_RE.is_match(&self.name) {
            return Err(StorageError::new(
                StorageErrorCode::MalformedMedium,
                format!("invalid KV unit name '{}'", self.name),
            ));
        }
        if let Some(table) = self
            .tables
            .iter()
            .find(|table| !UNIT_NAME_RE.is_match(table))
        {
            return Err(StorageError::new(
                StorageErrorCode::MalformedMedium,
                format!("invalid KV table name '{table}' in unit '{}'", self.name),
            ));
        }
        Ok(())
    }
}

/// Full current snapshot of one KV unit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KvSnapshot {
    /// Every declared table's records.
    pub tables: IndexMap<String, Map<String, Value>>,
    /// Global singleton, null when never written or undeclared.
    pub global: Value,
}

/// One opened KV unit.
pub trait KvUnit: Send + Sync + 'static {
    /// Reads the full current snapshot.
    fn load_all(&self) -> BoxFuture<'static, anyhow::Result<KvSnapshot>>;

    /// Atomically and durably upserts one record.
    fn put_record(
        &self,
        table: String,
        key: String,
        value: Value,
    ) -> BoxFuture<'static, anyhow::Result<()>>;

    /// Atomically and durably deletes one record; a miss is a no-op.
    fn delete_record(&self, table: String, key: String) -> BoxFuture<'static, anyhow::Result<()>>;

    /// Atomically and durably writes the global singleton.
    fn set_global(&self, value: Value) -> BoxFuture<'static, anyhow::Result<()>>;

    /// Drains writes and idempotently releases this unit.
    fn close(&self) -> BoxFuture<'static, Result<(), StorageError>>;
}

/// Optional KV facet of a storage backend.
pub trait KvFacet: Send + Sync + 'static {
    /// Opens or creates one unit.
    fn open(
        &self,
        descriptor: KvUnitDescriptor,
    ) -> BoxFuture<'static, anyhow::Result<Arc<dyn KvUnit>>>;
}

/// One registered backend owning exactly one durable medium.
pub trait StorageBackend: Send + Sync + 'static {
    /// KV operations when this backend serves the form.
    fn kv(&self) -> Option<Arc<dyn KvFacet>> {
        None
    }

    /// Drains all units and idempotently releases the medium.
    fn close(&self) -> BoxFuture<'static, Result<(), StorageError>>;
}

#[derive(Default)]
struct BackendRegistryInner {
    backends: Mutex<IndexMap<String, Arc<dyn StorageBackend>>>,
}

/// Mutable name-to-backend table preserving registration order for diagnostics.
#[derive(Clone, Default)]
pub struct BackendRegistry {
    inner: Arc<BackendRegistryInner>,
}

impl fmt::Debug for BackendRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BackendRegistry")
            .field("names", &self.names())
            .finish()
    }
}

impl BackendRegistry {
    /// Registers one named backend until the returned registration is disposed.
    ///
    /// # Errors
    ///
    /// Returns `duplicate-backend` when the name is occupied.
    pub fn register(
        &self,
        name: impl Into<String>,
        backend: Arc<dyn StorageBackend>,
    ) -> Result<BackendRegistration, StorageError> {
        let name = name.into();
        let mut backends = self.inner.backends.lock();
        if backends.contains_key(&name) {
            return Err(StorageError::new(
                StorageErrorCode::DuplicateBackend,
                format!("storage backend '{name}' is already registered"),
            ));
        }
        backends.insert(name.clone(), backend.clone());
        drop(backends);
        Ok(BackendRegistration {
            inner: Arc::new(RegistrationState {
                registry: Arc::downgrade(&self.inner),
                name,
                backend,
                disposed: Mutex::new(false),
            }),
        })
    }

    /// Resolves one backend.
    ///
    /// # Errors
    ///
    /// Returns `backend-not-found` with the registered-name diagnostic on a miss.
    pub fn get(&self, name: &str) -> Result<Arc<dyn StorageBackend>, StorageError> {
        let backends = self.inner.backends.lock();
        backends.get(name).cloned().ok_or_else(|| {
            let registered = if backends.is_empty() {
                "none".to_owned()
            } else {
                backends.keys().cloned().collect::<Vec<_>>().join(", ")
            };
            StorageError::new(
                StorageErrorCode::BackendNotFound,
                format!("storage backend '{name}' is not registered (registered: {registered})"),
            )
        })
    }

    /// Returns a registration-order name snapshot.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.inner.backends.lock().keys().cloned().collect()
    }
}

struct RegistrationState {
    registry: Weak<BackendRegistryInner>,
    name: String,
    backend: Arc<dyn StorageBackend>,
    disposed: Mutex<bool>,
}

/// Reversible named-backend registration.
#[derive(Clone)]
pub struct BackendRegistration {
    inner: Arc<RegistrationState>,
}

impl fmt::Debug for BackendRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BackendRegistration")
            .field("name", &self.inner.name)
            .field("disposed", &self.inner.disposed.lock())
            .finish_non_exhaustive()
    }
}

impl BackendRegistration {
    /// Idempotently unregisters only this contribution.
    pub fn dispose(&self) {
        let mut disposed = self.inner.disposed.lock();
        if *disposed {
            return;
        }
        *disposed = true;
        let Some(registry) = self.inner.registry.upgrade() else {
            return;
        };
        let mut backends = registry.backends.lock();
        if backends
            .get(&self.inner.name)
            .is_some_and(|current| Arc::ptr_eq(current, &self.inner.backend))
        {
            backends.shift_remove(&self.inner.name);
        }
    }
}

#[derive(Default)]
struct StorageInner {
    forms: Mutex<IndexMap<String, Arc<dyn Any + Send + Sync>>>,
}

/// Storage hub service.
#[derive(Default)]
pub struct Storage {
    /// Named backend table.
    pub backend: BackendRegistry,
    inner: Arc<StorageInner>,
}

impl fmt::Debug for Storage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Storage")
            .field("backend", &self.backend)
            .field(
                "forms",
                &self.inner.forms.lock().keys().cloned().collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl Storage {
    /// Creates an empty hub.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Mounts one typed data-form facility.
    ///
    /// # Errors
    ///
    /// Returns `duplicate-mount` when the name is occupied.
    pub fn mount<T>(
        &self,
        form: impl Into<String>,
        facility: Arc<T>,
    ) -> Result<FormMount, StorageError>
    where
        T: Any + Send + Sync,
    {
        let form = form.into();
        let facility: Arc<dyn Any + Send + Sync> = facility;
        let mut forms = self.inner.forms.lock();
        if forms.contains_key(&form) {
            return Err(StorageError::new(
                StorageErrorCode::DuplicateMount,
                format!("storage form '{form}' is already mounted"),
            ));
        }
        forms.insert(form.clone(), facility.clone());
        drop(forms);
        Ok(FormMount {
            inner: Arc::new(FormMountState {
                storage: Arc::downgrade(&self.inner),
                form,
                facility,
                disposed: Mutex::new(false),
            }),
        })
    }

    /// Resolves one typed data form.
    ///
    /// # Errors
    ///
    /// Returns `form-not-mounted` on a miss or type mismatch.
    pub fn form<T>(&self, form: &str) -> Result<Arc<T>, StorageError>
    where
        T: Any + Send + Sync,
    {
        let forms = self.inner.forms.lock();
        let value = forms.get(form).cloned().ok_or_else(|| {
            StorageError::new(
                StorageErrorCode::FormNotMounted,
                format!("storage form '{form}' is not mounted"),
            )
        })?;
        Arc::downcast(value).map_err(|_| {
            StorageError::new(
                StorageErrorCode::FormNotMounted,
                format!("storage form '{form}' is mounted with another Rust type"),
            )
        })
    }

    /// Provides this hub on the Cordis `storage` seat.
    ///
    /// # Errors
    ///
    /// Returns ordinary inactive-fiber or duplicate-service failures.
    pub fn provide(self: &Arc<Self>, context: &Context) -> Result<EffectHandle, CordisError> {
        context.provide(STORAGE, self.clone())
    }
}

/// Builds the source-compatible Storage service plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, _config| {
        Box::pin(async move {
            Storage::new().provide(&context)?;
            Ok(())
        })
    })
}

struct FormMountState {
    storage: Weak<StorageInner>,
    form: String,
    facility: Arc<dyn Any + Send + Sync>,
    disposed: Mutex<bool>,
}

/// Reversible data-form mount.
#[derive(Clone)]
pub struct FormMount {
    inner: Arc<FormMountState>,
}

impl fmt::Debug for FormMount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FormMount")
            .field("form", &self.inner.form)
            .field("disposed", &self.inner.disposed.lock())
            .finish_non_exhaustive()
    }
}

impl FormMount {
    /// Idempotently unmounts only this contribution.
    pub fn dispose(&self) {
        let mut disposed = self.inner.disposed.lock();
        if *disposed {
            return;
        }
        *disposed = true;
        let Some(storage) = self.inner.storage.upgrade() else {
            return;
        };
        let mut forms = storage.forms.lock();
        if forms
            .get(&self.inner.form)
            .is_some_and(|current| Arc::ptr_eq(current, &self.inner.facility))
        {
            forms.shift_remove(&self.inner.form);
        }
    }
}

/// Derives the lifecycle-only Cordis service key for a backend name.
#[must_use]
pub fn storage_backend_service_key(name: &str) -> String {
    format!("storage.backend.{name}")
}

/// Registers the package's explained-empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registry failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct FakeBackend;

    impl StorageBackend for FakeBackend {
        fn close(&self) -> BoxFuture<'static, Result<(), StorageError>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[test]
    fn registry_registers_resolves_disposes_and_rejects_duplicates() {
        let registry = BackendRegistry::default();
        let backend: Arc<dyn StorageBackend> = Arc::new(FakeBackend);
        let registration = registry.register("json", backend.clone()).unwrap();
        assert!(Arc::ptr_eq(&registry.get("json").unwrap(), &backend));
        assert_eq!(registry.names(), ["json"]);
        let duplicate = registry
            .register("json", Arc::new(FakeBackend))
            .unwrap_err();
        assert_eq!(duplicate.code, StorageErrorCode::DuplicateBackend);
        registration.dispose();
        assert!(registry.names().is_empty());
        let Err(error) = registry.get("json") else {
            panic!("disposed backend remained registered");
        };
        assert_eq!(error.code, StorageErrorCode::BackendNotFound);
    }

    #[test]
    fn stale_backend_registration_does_not_remove_successor() {
        let registry = BackendRegistry::default();
        let first = registry.register("json", Arc::new(FakeBackend)).unwrap();
        first.dispose();
        let second_backend: Arc<dyn StorageBackend> = Arc::new(FakeBackend);
        let _second = registry.register("json", second_backend.clone()).unwrap();
        first.dispose();
        assert!(Arc::ptr_eq(&registry.get("json").unwrap(), &second_backend));
    }

    #[tokio::test]
    async fn service_provision_and_form_mounts_are_reversible() {
        let context = Context::new();
        let fiber = seekdeep_cordis::Fiber::active_child("storage");
        let child = context.with_fiber(fiber.clone());
        let storage = Storage::new();
        storage.provide(&child).unwrap();
        assert!(Arc::ptr_eq(&context.get(STORAGE).unwrap(), &storage));

        let first = Arc::new(String::from("first"));
        let stale = storage.mount("domain", first.clone()).unwrap();
        assert!(Arc::ptr_eq(
            &storage.form::<String>("domain").unwrap(),
            &first
        ));
        assert_eq!(
            storage.mount("domain", first).unwrap_err().code,
            StorageErrorCode::DuplicateMount
        );
        stale.dispose();
        assert_eq!(
            storage.form::<String>("domain").unwrap_err().code,
            StorageErrorCode::FormNotMounted
        );
        let second = Arc::new(String::from("second"));
        let _current = storage.mount("domain", second.clone()).unwrap();
        stale.dispose();
        assert!(Arc::ptr_eq(
            &storage.form::<String>("domain").unwrap(),
            &second
        ));

        fiber.dispose().await.unwrap();
        assert!(context.get(STORAGE).is_none());
    }

    #[tokio::test]
    async fn plugin_publishes_and_withdraws_the_hub_service() {
        let context = Context::new();
        let mounted = context.plugin(plugin(), serde_json::Value::Null).unwrap();
        mounted.await_settled().await.unwrap();
        assert!(context.get(STORAGE).is_some());
        mounted.dispose().await.unwrap();
        assert!(context.get(STORAGE).is_none());
    }

    #[tokio::test]
    async fn explained_empty_invariant_reserves_and_releases_package_identity() {
        let context = Context::new();
        let registry =
            InvariantRegistry::install(&context, &seekdeep_invariants::InvariantConfig::default())
                .expect("invariant registry");
        let registration = register_invariant(&registry).expect("storage invariant");
        registration.await_ready().await.expect("invariant ready");
        assert!(register_invariant(&registry).is_err());
        registration.dispose().await.expect("dispose invariant");
        register_invariant(&registry)
            .expect("replacement invariant")
            .await_ready()
            .await
            .expect("replacement ready");
    }

    #[test]
    fn backend_service_keys_and_descriptor_validation_are_exact() {
        assert_eq!(storage_backend_service_key("json"), "storage.backend.json");
        assert_eq!(
            storage_backend_service_key("tenant-a"),
            "storage.backend.tenant-a"
        );
        let valid = KvUnitDescriptor {
            name: "contract_unit".to_owned(),
            version: 3,
            tables: vec!["alpha".to_owned(), "beta".to_owned()],
            has_global: true,
        };
        valid.validate().unwrap();
        let mut invalid = valid;
        invalid.tables.push("bad-name".to_owned());
        assert_eq!(
            invalid.validate().unwrap_err().code,
            StorageErrorCode::MalformedMedium
        );
    }
}
