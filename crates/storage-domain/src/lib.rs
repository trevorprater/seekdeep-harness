//! Schema-validated, change-emitting KV domains over named storage backends.

use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

use futures::{
    FutureExt as _, StreamExt as _, future::BoxFuture, future::join_all, stream::BoxStream,
};
use indexmap::IndexMap;
use parking_lot::{Mutex, ReentrantMutex};
use seekdeep_cordis::{
    Context, EventArgs, EventOptions, EventReply, Plugin, ServiceKey,
    fiber::{DisposeFuture, EffectHandle},
};
use seekdeep_invariants::{
    InvariantFailure, InvariantInstaller, InvariantRegistration, InvariantRegistry,
};
use seekdeep_storage::{
    FormMount, KvSnapshot, KvUnit, KvUnitDescriptor, STORAGE, Storage, StorageError, UNIT_NAME_RE,
    storage_backend_service_key,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::{Notify, mpsc, oneshot};

/// Typed Cordis slot corresponding to `ctx.storageDomain`.
pub const STORAGE_DOMAIN: ServiceKey<DomainFacility> = ServiceKey::new("storageDomain");

/// Cordis plugin name retained by loader-facing diagnostics.
pub const NAME: &str = "storage-domain";
/// The hub is the static dependency; backend seats are injected from config.
pub const INJECT: &[&str] = &["storage"];

/// Stable invariant companion name.
pub const INVARIANT_NAME: &str = "storage-domain-invariant";
const PACKAGE_NAME: &str = "@deepseek-ai/seekdeep-storage-domain";

/// Stable domain-layer failure vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DomainErrorCode {
    /// The domain name is reserved by an open or in-flight open.
    AlreadyOpen,
    /// The routed backend does not serve KV.
    FacetUnsupported,
    /// Stored data fails its declared schema.
    InvalidRecord,
    /// An update targeted an absent record.
    MissingKey,
    /// An operation targeted a closed domain.
    Closed,
}

impl DomainErrorCode {
    /// Exact source discriminant.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AlreadyOpen => "already-open",
            Self::FacetUnsupported => "facet-unsupported",
            Self::InvalidRecord => "invalid-record",
            Self::MissingKey => "missing-key",
            Self::Closed => "closed",
        }
    }
}

impl fmt::Display for DomainErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Location of one invalid durable record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidRecordDetail {
    /// Table name, empty for global.
    pub table: String,
    /// Record key, empty for global.
    pub key: String,
}

/// Typed domain-layer failure.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct DomainError {
    /// Stable failure discriminant.
    pub code: DomainErrorCode,
    /// Diagnostic prose.
    pub message: String,
    /// Invalid durable slot exactly for `invalid-record`.
    pub detail: Option<InvalidRecordDetail>,
    cause: Option<Arc<anyhow::Error>>,
}

impl DomainError {
    /// Creates a domain failure.
    #[must_use]
    pub fn new(code: DomainErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: None,
            cause: None,
        }
    }

    fn invalid(domain: &str, table: &str, key: &str, cause: anyhow::Error) -> Self {
        let slot = if table.is_empty() {
            "global".to_owned()
        } else {
            format!("record '{key}' in table '{table}'")
        };
        Self {
            code: DomainErrorCode::InvalidRecord,
            message: format!("domain '{domain}': stored {slot} does not match its schema"),
            detail: Some(InvalidRecordDetail {
                table: table.to_owned(),
                key: key.to_owned(),
            }),
            cause: Some(Arc::new(cause)),
        }
    }

    /// Original validation failure, when retained.
    #[must_use]
    pub fn cause(&self) -> Option<&anyhow::Error> {
        self.cause.as_deref()
    }
}

/// Runtime JSON validator/parser corresponding to a source Zod schema.
type ValueParser = dyn Fn(&Value) -> anyhow::Result<Value> + Send + Sync;

/// Cloneable durable-boundary JSON parser and normalizer.
#[derive(Clone)]
pub struct ValueSchema {
    parse: Arc<ValueParser>,
}

impl fmt::Debug for ValueSchema {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ValueSchema(..)")
    }
}

impl ValueSchema {
    /// Creates a schema from one pure parser/normalizer.
    #[must_use]
    pub fn new(parse: impl Fn(&Value) -> anyhow::Result<Value> + Send + Sync + 'static) -> Self {
        Self {
            parse: Arc::new(parse),
        }
    }

    /// Creates a strict Serde round-trip schema.
    #[must_use]
    pub fn serde<T>() -> Self
    where
        T: DeserializeOwned + Serialize + Send + Sync + 'static,
    {
        Self::new(|value| {
            let parsed: T = serde_json::from_value(value.clone())?;
            Ok(serde_json::to_value(parsed)?)
        })
    }

    /// Parses and normalizes one JSON value.
    ///
    /// # Errors
    ///
    /// Returns the schema-owned validation failure.
    pub fn parse(&self, value: &Value) -> anyhow::Result<Value> {
        (self.parse)(value)
    }
}

/// Global singleton declaration.
#[derive(Clone, Debug)]
pub struct DomainGlobalSpec {
    /// Durable-boundary schema.
    pub schema: ValueSchema,
    /// Value served before the first write.
    pub initial: Value,
}

/// One record-table declaration.
#[derive(Clone, Debug)]
pub struct DomainTableSpec {
    /// Durable-boundary record schema.
    pub value_schema: ValueSchema,
}

/// Static declaration of one domain.
#[derive(Clone, Debug)]
pub struct DomainSpec {
    /// Unit/domain name.
    pub name: String,
    /// Durable format version.
    pub version: u64,
    /// Optional global singleton.
    pub global: Option<DomainGlobalSpec>,
    /// Table declarations in stable insertion order.
    pub tables: IndexMap<String, DomainTableSpec>,
}

/// Declares one table.
#[must_use]
pub fn domain_table(schema: ValueSchema) -> DomainTableSpec {
    DomainTableSpec {
        value_schema: schema,
    }
}

/// Validates one domain declaration before any medium is touched.
///
/// # Errors
///
/// Returns an error for invalid names or a nullable global schema.
pub fn define_domain(spec: DomainSpec) -> anyhow::Result<DomainSpec> {
    anyhow::ensure!(
        UNIT_NAME_RE.is_match(&spec.name),
        "domain name '{}' must match {}",
        spec.name,
        UNIT_NAME_RE.as_str()
    );
    for table in spec.tables.keys() {
        anyhow::ensure!(
            UNIT_NAME_RE.is_match(table),
            "domain '{}' table name '{}' must match {}",
            spec.name,
            table,
            UNIT_NAME_RE.as_str()
        );
    }
    if let Some(global) = &spec.global {
        anyhow::ensure!(
            global.schema.parse(&Value::Null).is_err(),
            "domain '{}' global schema must not accept null: null is the medium's \"never written\" sentinel, so a stored null could not round-trip",
            spec.name
        );
    }
    Ok(spec)
}

/// Projects one domain spec onto the backend unit descriptor.
#[must_use]
pub fn descriptor_of(spec: &DomainSpec) -> KvUnitDescriptor {
    KvUnitDescriptor {
        name: spec.name.clone(),
        version: spec.version,
        tables: spec.tables.keys().cloned().collect(),
        has_global: spec.global.is_some(),
    }
}

/// One post-durability domain change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation")]
pub enum DomainChanged {
    /// A record or global singleton was inserted/replaced.
    #[serde(rename = "put")]
    Put {
        /// Domain name.
        domain: String,
        /// Table name, empty for global.
        table: String,
        /// Key, empty for global.
        key: String,
        /// New complete snapshot.
        value: Value,
    },
    /// A record was deleted.
    #[serde(rename = "deleted")]
    Deleted {
        /// Domain name.
        domain: String,
        /// Table name.
        table: String,
        /// Deleted key.
        key: String,
    },
}

impl DomainChanged {
    /// Owning domain.
    #[must_use]
    pub fn domain(&self) -> &str {
        match self {
            Self::Put { domain, .. } | Self::Deleted { domain, .. } => domain,
        }
    }

    /// Table name, empty for global.
    #[must_use]
    pub fn table(&self) -> &str {
        match self {
            Self::Put { table, .. } | Self::Deleted { table, .. } => table,
        }
    }

    /// Record key, empty for global.
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            Self::Put { key, .. } | Self::Deleted { key, .. } => key,
        }
    }
}

/// Backend routing configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainConfig {
    /// Default backend name.
    pub backend: String,
    /// Per-domain backend overrides.
    #[serde(default)]
    pub routes: HashMap<String, String>,
}

/// Builds the dependency-driven Cordis plugin from the source package.
///
/// The outer plugin waits for `storage`; its inner injection follows every
/// configured backend lifecycle seat. A backend disappearance therefore
/// withdraws the service, drains all still-open domains, and unmounts the form.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: DomainConfig = serde_json::from_value(config)?;
            let backend_services = std::iter::once(config.backend.as_str())
                .chain(config.routes.values().map(String::as_str))
                .map(storage_backend_service_key)
                .collect::<HashSet<_>>();
            let inner_config = config.clone();
            let inner = Plugin::new(
                "storage-domain:backends",
                backend_services,
                move |domain_context, _| {
                    let config = inner_config.clone();
                    Box::pin(async move { activate_plugin_epoch(&domain_context, config) })
                },
            );
            context.plugin(inner, Value::Null)?;
            Ok(())
        })
    })
    .with_config_validator(|value| {
        let config: DomainConfig = serde_json::from_value(value.clone())?;
        anyhow::ensure!(!config.backend.is_empty(), "backend must not be empty");
        Ok(serde_json::to_value(config)?)
    })
}

fn activate_plugin_epoch(context: &Context, config: DomainConfig) -> anyhow::Result<()> {
    let storage = context
        .get(STORAGE)
        .ok_or_else(|| anyhow::anyhow!("storage-domain lost required storage service"))?;
    let facility = DomainFacility::new(context.clone(), storage.clone(), config);
    let mount = storage.mount("domain", facility.clone())?;
    let cleanup_facility = facility.clone();
    let cleanup_mount = mount.clone();
    let cleanup = EffectHandle::new("storage-domain form", move || -> DisposeFuture {
        Box::pin(async move {
            cleanup_facility.close_all().await?;
            cleanup_mount.dispose();
            Ok(())
        })
    });
    if let Err(error) = context.own(cleanup) {
        mount.dispose();
        return Err(error.into());
    }
    context.provide(STORAGE_DOMAIN, facility)?;
    Ok(())
}

enum QueueMessage {
    Job(BoxFuture<'static, ()>),
    Close,
}

#[derive(Debug, Default)]
struct CloseState {
    result: Mutex<Option<Result<(), StorageError>>>,
    notify: Notify,
}

#[derive(Debug, Default)]
struct ChangeHubState {
    sequence: u64,
    subscribers: Vec<mpsc::UnboundedSender<(u64, DomainChanged)>>,
}

#[derive(Debug, Default)]
struct ChangeHub {
    state: Mutex<ChangeHubState>,
}

impl ChangeHub {
    fn send(&self, change: &DomainChanged) {
        let mut state = self.state.lock();
        state.sequence = state.sequence.saturating_add(1);
        let sequence = state.sequence;
        state
            .subscribers
            .retain(|subscriber| subscriber.send((sequence, change.clone())).is_ok());
    }

    fn subscribe(&self) -> mpsc::UnboundedReceiver<(u64, DomainChanged)> {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.state.lock().subscribers.push(sender);
        receiver
    }

    fn sequence(&self) -> u64 {
        self.state.lock().sequence
    }
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

/// Mounted domain facility.
pub struct DomainFacility {
    context: Context,
    storage: Arc<Storage>,
    config: DomainConfig,
    domains: Mutex<HashMap<String, Arc<Domain>>>,
    reserved: Mutex<HashSet<String>>,
    changes: Arc<ChangeHub>,
}

impl fmt::Debug for DomainFacility {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DomainFacility")
            .field("config", &self.config)
            .field("domains", &self.domains.lock().keys().collect::<Vec<_>>())
            .field("reserved", &self.reserved.lock())
            .finish_non_exhaustive()
    }
}

impl DomainFacility {
    /// Creates an unmounted facility over one storage hub.
    #[must_use]
    pub fn new(context: Context, storage: Arc<Storage>, config: DomainConfig) -> Arc<Self> {
        Arc::new(Self {
            context,
            storage,
            config,
            domains: Mutex::new(HashMap::new()),
            reserved: Mutex::new(HashSet::new()),
            changes: Arc::new(ChangeHub::default()),
        })
    }

    /// Opens one declared domain.
    pub fn open(
        self: &Arc<Self>,
        spec: DomainSpec,
    ) -> BoxFuture<'static, anyhow::Result<Arc<Domain>>> {
        {
            let mut reserved = self.reserved.lock();
            if !reserved.insert(spec.name.clone()) {
                let name = spec.name;
                return async move {
                    Err(DomainError::new(
                        DomainErrorCode::AlreadyOpen,
                        format!("domain '{name}' is already open"),
                    )
                    .into())
                }
                .boxed();
            }
        }
        let facility = self.clone();
        async move { facility.finish_open(spec).await }.boxed()
    }

    async fn finish_open(self: &Arc<Self>, spec: DomainSpec) -> anyhow::Result<Arc<Domain>> {
        let result = self.try_open(&spec).await;
        if result.is_err() {
            self.reserved.lock().remove(&spec.name);
        }
        result
    }

    async fn try_open(self: &Arc<Self>, spec: &DomainSpec) -> anyhow::Result<Arc<Domain>> {
        let backend_name = self
            .config
            .routes
            .get(&spec.name)
            .unwrap_or(&self.config.backend)
            .clone();
        let backend = self.storage.backend.get(&backend_name)?;
        let facet = backend.kv().ok_or_else(|| {
            DomainError::new(
                DomainErrorCode::FacetUnsupported,
                format!(
                    "backend '{backend_name}' routed for domain '{}' has no kv facet",
                    spec.name
                ),
            )
        })?;
        let unit = facet.open(descriptor_of(spec)).await?;
        let loaded = unit.load_all().await;
        let snapshot = match loaded {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let _ = unit.close().await;
                return Err(error);
            }
        };
        let prepared = prepare_snapshot(spec, &snapshot);
        let (tables, global) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                let _ = unit.close().await;
                return Err(error);
            }
        };
        let domain = Domain::new(
            self.context.clone(),
            spec.clone(),
            unit,
            tables,
            global,
            Arc::downgrade(self),
            self.changes.clone(),
        );
        self.domains
            .lock()
            .insert(spec.name.clone(), domain.clone());
        Ok(domain)
    }

    /// Looks up one open domain.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<Domain>> {
        self.domains.lock().get(name).cloned()
    }

    fn closed(&self, name: &str) {
        self.domains.lock().remove(name);
        self.reserved.lock().remove(name);
    }

    /// Subscribes to all committed domain changes.
    #[must_use]
    pub fn subscribe(&self) -> BoxStream<'static, anyhow::Result<DomainChanged>> {
        let mut receiver = self.changes.subscribe();
        async_stream::stream! {
            while let Some((_, change)) = receiver.recv().await {
                yield Ok(change);
            }
        }
        .boxed()
    }

    /// Subscribes without loss and retains a monotonic facility-wide commit sequence.
    #[must_use]
    pub fn subscribe_sequenced(&self) -> BoxStream<'static, anyhow::Result<(u64, DomainChanged)>> {
        let mut receiver = self.changes.subscribe();
        async_stream::stream! {
            while let Some(change) = receiver.recv().await {
                yield Ok(change);
            }
        }
        .boxed()
    }

    /// Latest committed change sequence.
    #[must_use]
    pub fn change_sequence(&self) -> u64 {
        self.changes.sequence()
    }

    /// Closes every still-open domain.
    ///
    /// # Errors
    ///
    /// Returns an aggregate after attempting every unit close when one or
    /// more backends reject teardown.
    pub async fn close_all(&self) -> anyhow::Result<()> {
        let domains = self.domains.lock().values().cloned().collect::<Vec<_>>();
        let results = join_all(domains.into_iter().map(|domain| domain.close())).await;
        let errors = results
            .into_iter()
            .filter_map(Result::err)
            .map(|error| format!("{error:#}"))
            .collect::<Vec<_>>();
        if errors.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(errors.join("\n"))
        }
    }

    /// Provides and mounts this facility for its Cordis lifetime.
    ///
    /// # Errors
    ///
    /// Returns ordinary service or storage mount failures.
    pub fn mount(
        self: &Arc<Self>,
        context: &Context,
    ) -> Result<(EffectHandle, FormMount), anyhow::Error> {
        let mount = self.storage.mount("domain", self.clone())?;
        match context.provide(STORAGE_DOMAIN, self.clone()) {
            Ok(service) => Ok((service, mount)),
            Err(error) => {
                mount.dispose();
                Err(error.into())
            }
        }
    }
}

type RecordMap = IndexMap<String, Value>;
type DomainTables = IndexMap<String, RecordMap>;
type PreparedSnapshot = (DomainTables, Option<Value>);

fn prepare_snapshot(spec: &DomainSpec, snapshot: &KvSnapshot) -> anyhow::Result<PreparedSnapshot> {
    let mut tables = IndexMap::new();
    for (table, table_spec) in &spec.tables {
        let mut records = IndexMap::new();
        if let Some(stored) = snapshot.tables.get(table) {
            for (key, raw) in stored {
                let parsed = table_spec
                    .value_schema
                    .parse(raw)
                    .map_err(|error| DomainError::invalid(&spec.name, table, key, error))?;
                records.insert(key.clone(), parsed);
            }
        }
        tables.insert(table.clone(), records);
    }
    let global = match &spec.global {
        None => None,
        Some(global) if snapshot.global.is_null() => Some(global.initial.clone()),
        Some(global) => Some(
            global
                .schema
                .parse(&snapshot.global)
                .map_err(|error| DomainError::invalid(&spec.name, "", "", error))?,
        ),
    };
    Ok((tables, global))
}

/// One open domain with authoritative in-memory state and a single write queue.
pub struct Domain {
    name: String,
    context: Context,
    unit: Arc<dyn KvUnit>,
    tables: HashMap<String, Arc<Mutex<RecordMap>>>,
    table_handles: Mutex<HashMap<String, Weak<KvTable>>>,
    global: Option<Arc<Mutex<Value>>>,
    sender: mpsc::UnboundedSender<QueueMessage>,
    disposing: AtomicBool,
    closed: AtomicBool,
    close_state: Arc<CloseState>,
    facility: Weak<DomainFacility>,
    changes: Arc<ChangeHub>,
    commit_lock: Arc<ReentrantMutex<()>>,
    self_weak: Weak<Domain>,
}

impl fmt::Debug for Domain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Domain")
            .field("name", &self.name)
            .field("tables", &self.tables.keys().collect::<Vec<_>>())
            .field("disposing", &self.disposing.load(Ordering::Acquire))
            .field("closed", &self.closed.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl Domain {
    #[allow(clippy::too_many_arguments)]
    fn new(
        context: Context,
        spec: DomainSpec,
        unit: Arc<dyn KvUnit>,
        records: IndexMap<String, IndexMap<String, Value>>,
        global: Option<Value>,
        facility: Weak<DomainFacility>,
        changes: Arc<ChangeHub>,
    ) -> Arc<Self> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let domain = Arc::new_cyclic(|weak| {
            let tables = records
                .into_iter()
                .map(|(name, records)| (name, Arc::new(Mutex::new(records))))
                .collect();
            Self {
                name: spec.name,
                context,
                unit,
                tables,
                table_handles: Mutex::new(HashMap::new()),
                global: global.map(|value| Arc::new(Mutex::new(value))),
                sender,
                disposing: AtomicBool::new(false),
                closed: AtomicBool::new(false),
                close_state: Arc::new(CloseState::default()),
                facility,
                changes,
                commit_lock: Arc::new(ReentrantMutex::new(())),
                self_weak: weak.clone(),
            }
        });
        tokio::spawn(run_queue(Arc::downgrade(&domain), receiver));
        domain
    }

    /// Domain name from the spec.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Lock spanning authoritative memory mutation, commit callbacks, and change emission.
    #[must_use]
    pub fn commit_lock(&self) -> Arc<ReentrantMutex<()>> {
        self.commit_lock.clone()
    }

    /// Resolves one stable declared table handle.
    ///
    /// # Errors
    ///
    /// Returns an error for an undeclared table.
    pub fn table(&self, name: &str) -> anyhow::Result<Arc<KvTable>> {
        let records =
            self.tables.get(name).cloned().ok_or_else(|| {
                anyhow::anyhow!("domain '{}' declares no table '{name}'", self.name)
            })?;
        let mut handles = self.table_handles.lock();
        if let Some(table) = handles.get(name).and_then(Weak::upgrade) {
            return Ok(table);
        }
        let domain = self
            .self_weak
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("domain '{}' lost its owner", self.name))?;
        let table = Arc::new(KvTable {
            domain,
            name: name.to_owned(),
            records,
        });
        handles.insert(name.to_owned(), Arc::downgrade(&table));
        Ok(table)
    }

    /// Returns the current global singleton.
    ///
    /// # Errors
    ///
    /// Returns an error when closed or when no global is declared.
    pub fn global_get(&self) -> anyhow::Result<Value> {
        self.assert_readable()?;
        self.global
            .as_ref()
            .map(|global| global.lock().clone())
            .ok_or_else(|| anyhow::anyhow!("domain '{}' declares no global", self.name))
    }

    /// Replaces the global durably through the write queue.
    pub fn global_set(&self, value: Value) -> BoxFuture<'static, anyhow::Result<()>> {
        self.global_set_with_commit(value, |_| {})
    }

    /// Replaces the global and invokes one callback after memory changes but before its event.
    pub fn global_set_with_commit<C>(
        &self,
        value: Value,
        on_commit: C,
    ) -> BoxFuture<'static, anyhow::Result<()>>
    where
        C: FnOnce(&Value) + Send + 'static,
    {
        let Some(global) = self.global.clone() else {
            let name = self.name.clone();
            return async move { anyhow::bail!("domain '{name}' declares no global") }.boxed();
        };
        let unit = self.unit.clone();
        let name = self.name.clone();
        let domain = self.weak_self();
        self.enqueue(async move {
            unit.set_global(value.clone()).await?;
            if let Some(domain) = domain.upgrade() {
                let _commit = domain.commit_lock.lock();
                *global.lock() = value.clone();
                on_commit(&value);
                domain.emit(DomainChanged::Put {
                    domain: name,
                    table: String::new(),
                    key: String::new(),
                    value,
                });
            } else {
                *global.lock() = value;
            }
            Ok(())
        })
    }

    fn weak_self(&self) -> Weak<Self> {
        self.self_weak.clone()
    }

    fn enqueue<T, F>(&self, future: F) -> BoxFuture<'static, anyhow::Result<T>>
    where
        T: Send + 'static,
        F: std::future::Future<Output = anyhow::Result<T>> + Send + 'static,
    {
        if self.disposing.load(Ordering::Acquire) {
            let name = self.name.clone();
            return async move {
                Err(DomainError::new(
                    DomainErrorCode::Closed,
                    format!("domain '{name}' is closed"),
                )
                .into())
            }
            .boxed();
        }
        let (send, receive) = oneshot::channel();
        let job = async move {
            let _ = send.send(future.await);
        }
        .boxed();
        if self.sender.send(QueueMessage::Job(job)).is_err() {
            let name = self.name.clone();
            return async move {
                Err(DomainError::new(
                    DomainErrorCode::Closed,
                    format!("domain '{name}' is closed"),
                )
                .into())
            }
            .boxed();
        }
        async move {
            receive
                .await
                .map_err(|_| anyhow::anyhow!("domain write queue stopped"))?
        }
        .boxed()
    }

    fn assert_readable(&self) -> anyhow::Result<()> {
        if self.closed.load(Ordering::Acquire) {
            Err(DomainError::new(
                DomainErrorCode::Closed,
                format!("domain '{}' is closed", self.name),
            )
            .into())
        } else {
            Ok(())
        }
    }

    fn emit(&self, change: DomainChanged) {
        self.changes.send(&change);
        if let Err(error) =
            self.context
                .events()
                .emit(&self.context, "domain/changed", &EventArgs::one(change))
        {
            tracing::warn!(domain = %self.name, error = %error, "domain/changed listener failed");
        }
    }

    /// Rejects new writes, drains accepted writes, then closes the unit.
    pub fn close(&self) -> BoxFuture<'static, anyhow::Result<()>> {
        if !self.disposing.swap(true, Ordering::AcqRel) {
            let _ = self.sender.send(QueueMessage::Close);
        }
        let state = self.close_state.clone();
        async move { state.wait().await.map_err(anyhow::Error::new) }.boxed()
    }
}

async fn run_queue(domain: Weak<Domain>, mut receiver: mpsc::UnboundedReceiver<QueueMessage>) {
    while let Some(message) = receiver.recv().await {
        match message {
            QueueMessage::Job(job) => job.await,
            QueueMessage::Close => {
                let Some(domain) = domain.upgrade() else {
                    return;
                };
                let result = domain.unit.close().await;
                domain.closed.store(true, Ordering::Release);
                if let Some(facility) = domain.facility.upgrade() {
                    facility.closed(&domain.name);
                }
                domain.close_state.complete(result);
                return;
            }
        }
    }
}

/// Stable handle on one declared table.
pub struct KvTable {
    domain: Arc<Domain>,
    name: String,
    records: Arc<Mutex<IndexMap<String, Value>>>,
}

struct UpdateJob {
    domain: Arc<Domain>,
    unit: Arc<dyn KvUnit>,
    records: Arc<Mutex<IndexMap<String, Value>>>,
    table: String,
    domain_name: String,
    key: String,
    committed_domain: Arc<Domain>,
}

impl fmt::Debug for KvTable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("KvTable")
            .field("name", &self.name)
            .field("size", &self.records.lock().len())
            .finish_non_exhaustive()
    }
}

impl KvTable {
    /// Reads one record synchronously.
    ///
    /// # Errors
    ///
    /// Returns `closed` after domain teardown or an ownership error if the
    /// domain was dropped before this handle.
    pub fn get(&self, key: &str) -> anyhow::Result<Option<Value>> {
        self.domain.assert_readable()?;
        Ok(self.records.lock().get(key).cloned())
    }

    /// Returns a stable entry snapshot.
    ///
    /// # Errors
    ///
    /// Returns `closed` after domain teardown or an ownership error if the
    /// domain was dropped before this handle.
    pub fn entries(&self) -> anyhow::Result<Vec<(String, Value)>> {
        self.domain.assert_readable()?;
        Ok(self
            .records
            .lock()
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect())
    }

    /// Returns a stable key snapshot.
    ///
    /// # Errors
    ///
    /// Returns `closed` after domain teardown or an ownership error if the
    /// domain was dropped before this handle.
    pub fn keys(&self) -> anyhow::Result<Vec<String>> {
        self.domain.assert_readable()?;
        Ok(self.records.lock().keys().cloned().collect())
    }

    /// Current record count.
    ///
    /// # Errors
    ///
    /// Returns `closed` after domain teardown or an ownership error if the
    /// domain was dropped before this handle.
    pub fn len(&self) -> anyhow::Result<usize> {
        self.domain.assert_readable()?;
        Ok(self.records.lock().len())
    }

    /// Whether the table is empty.
    ///
    /// # Errors
    ///
    /// Returns `closed` after domain teardown or an ownership error if the
    /// domain was dropped before this handle.
    pub fn is_empty(&self) -> anyhow::Result<bool> {
        self.len().map(|length| length == 0)
    }

    /// Durably inserts or replaces one record.
    pub fn put(&self, key: String, value: Value) -> BoxFuture<'static, anyhow::Result<()>> {
        let domain = self.domain.clone();
        let unit = domain.unit.clone();
        let records = self.records.clone();
        let table = self.name.clone();
        let domain_name = domain.name.clone();
        let committed_domain = domain.clone();
        domain.enqueue(async move {
            unit.put_record(table.clone(), key.clone(), value.clone())
                .await?;
            let _commit = committed_domain.commit_lock.lock();
            records.lock().insert(key.clone(), value.clone());
            committed_domain.emit(DomainChanged::Put {
                domain: domain_name,
                table,
                key,
                value,
            });
            Ok(())
        })
    }

    /// Durably deletes one record; an absent key is a no-op.
    pub fn delete(&self, key: String) -> BoxFuture<'static, anyhow::Result<bool>> {
        let domain = self.domain.clone();
        let unit = domain.unit.clone();
        let records = self.records.clone();
        let table = self.name.clone();
        let domain_name = domain.name.clone();
        let committed_domain = domain.clone();
        domain.enqueue(async move {
            if !records.lock().contains_key(&key) {
                return Ok(false);
            }
            unit.delete_record(table.clone(), key.clone()).await?;
            let _commit = committed_domain.commit_lock.lock();
            records.lock().shift_remove(&key);
            committed_domain.emit(DomainChanged::Deleted {
                domain: domain_name,
                table,
                key,
            });
            Ok(true)
        })
    }

    /// Atomic queued read-modify-write.
    pub fn update<F>(&self, key: String, transform: F) -> BoxFuture<'static, anyhow::Result<Value>>
    where
        F: FnOnce(&Value) -> anyhow::Result<Value> + Send + 'static,
    {
        let domain = self.domain.clone();
        let unit = domain.unit.clone();
        let records = self.records.clone();
        let table = self.name.clone();
        let domain_name = domain.name.clone();
        let committed_domain = domain.clone();
        Self::update_job(
            UpdateJob {
                domain,
                unit,
                records,
                table,
                domain_name,
                key,
                committed_domain,
            },
            transform,
            move |_| {},
        )
    }

    /// Atomic queued read-modify-write with a pre-event commit callback.
    pub fn update_with_commit<F, C>(
        &self,
        key: String,
        transform: F,
        on_commit: C,
    ) -> BoxFuture<'static, anyhow::Result<Value>>
    where
        F: FnOnce(&Value) -> anyhow::Result<Value> + Send + 'static,
        C: FnOnce(&Value) + Send + 'static,
    {
        let domain = self.domain.clone();
        let unit = domain.unit.clone();
        let records = self.records.clone();
        let table = self.name.clone();
        let domain_name = domain.name.clone();
        let committed_domain = domain.clone();
        Self::update_job(
            UpdateJob {
                domain,
                unit,
                records,
                table,
                domain_name,
                key,
                committed_domain,
            },
            transform,
            on_commit,
        )
    }

    fn update_job<F, C>(
        job: UpdateJob,
        transform: F,
        on_commit: C,
    ) -> BoxFuture<'static, anyhow::Result<Value>>
    where
        F: FnOnce(&Value) -> anyhow::Result<Value> + Send + 'static,
        C: FnOnce(&Value) + Send + 'static,
    {
        job.domain.clone().enqueue(async move {
            let current = job.records.lock().get(&job.key).cloned().ok_or_else(|| {
                DomainError::new(
                    DomainErrorCode::MissingKey,
                    format!(
                        "domain '{}' table '{}' has no record '{}' to update",
                        job.domain_name, job.table, job.key
                    ),
                )
            })?;
            let next = transform(&current)?;
            job.unit
                .put_record(job.table.clone(), job.key.clone(), next.clone())
                .await?;
            let _commit = job.committed_domain.commit_lock.lock();
            job.records.lock().insert(job.key.clone(), next.clone());
            on_commit(&next);
            job.committed_domain.emit(DomainChanged::Put {
                domain: job.domain_name,
                table: job.table,
                key: job.key,
                value: next.clone(),
            });
            Ok(next)
        })
    }
}

/// Registers the change-event-to-memory invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        PACKAGE_NAME,
        InvariantInstaller::new(["storage"], install_invariant),
    )
}

async fn install_invariant(context: Context, failure: InvariantFailure) -> anyhow::Result<()> {
    context.events().on_sync(
        &context,
        "domain/changed",
        move |context, args| {
            if let Some(change) = decode_change_argument(&args)? {
                validate_change(&context, &change, &failure)?;
            }
            Ok(EventReply::Undefined)
        },
        EventOptions {
            global: true,
            ..EventOptions::default()
        },
    )?;
    Ok(())
}

fn decode_change_argument(args: &EventArgs) -> anyhow::Result<Option<DomainChanged>> {
    if let Some(change) = args.get::<DomainChanged>(0) {
        return Ok(Some((*change).clone()));
    }
    let value = args
        .get::<Value>(0)
        .ok_or_else(|| anyhow::anyhow!("domain/changed lacks change payload"))?;
    let operation = value
        .as_object()
        .and_then(|object| object.get("operation"))
        .and_then(Value::as_str);
    match operation {
        Some("put" | "deleted") => Ok(Some(serde_json::from_value((*value).clone())?)),
        Some(_) => Ok(None),
        None => Err(anyhow::anyhow!("domain/changed payload lacks operation")),
    }
}

fn validate_change(
    context: &Context,
    change: &DomainChanged,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    let facility = context
        .get(STORAGE_DOMAIN)
        .ok_or_else(|| failure.fail("storageDomain service is absent"))?;
    let domain = facility.get(change.domain()).ok_or_else(|| {
        failure.fail(format!(
            "domain/changed for '{}' emitted while that domain is not open",
            change.domain()
        ))
    })?;
    if change.table().is_empty() {
        let DomainChanged::Put { value, .. } = change else {
            return Err(failure
                .fail("global domain/changed cannot be a deletion")
                .into());
        };
        if domain.global_get()? != *value {
            return Err(failure
                .fail(format!(
                    "domain/changed global value for '{}' differs from the in-memory global",
                    change.domain()
                ))
                .into());
        }
        return Ok(());
    }
    let current = domain.table(change.table())?.get(change.key())?;
    match change {
        DomainChanged::Deleted { .. } if current.is_some() => Err(failure
            .fail(format!(
                "domain/changed deletion of '{}'.'{}'['{}'] emitted while the record is still in memory",
                change.domain(),
                change.table(),
                change.key()
            ))
            .into()),
        DomainChanged::Put { value, .. } if current.as_ref() != Some(value) => Err(failure
            .fail(format!(
                "domain/changed value for '{}'.'{}'['{}'] differs from the in-memory record",
                change.domain(),
                change.table(),
                change.key()
            ))
            .into()),
        DomainChanged::Deleted { .. } | DomainChanged::Put { .. } => Ok(()),
    }
}

/// Resolves the storage hub and constructs a facility.
///
/// # Errors
///
/// Returns an error when the storage service is absent.
pub fn facility_from_context(
    context: &Context,
    config: DomainConfig,
) -> anyhow::Result<Arc<DomainFacility>> {
    let storage = context
        .get(STORAGE)
        .ok_or_else(|| anyhow::anyhow!("storage service is required"))?;
    Ok(DomainFacility::new(context.clone(), storage, config))
}
