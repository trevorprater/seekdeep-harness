//! Domain data-form behavior ported from the pinned source specifications.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use futures::{FutureExt as _, StreamExt as _, future::BoxFuture};
use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventArgs, EventOptions, FiberState};
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_storage::{
    KvFacet, KvSnapshot, KvUnit, KvUnitDescriptor, Storage, StorageBackend, StorageError,
    StorageErrorCode, storage_backend_service_key,
};
use seekdeep_storage_domain::{
    DomainChanged, DomainConfig, DomainError, DomainErrorCode, DomainGlobalSpec, DomainSpec,
    DomainTableSpec, ValueSchema, define_domain, descriptor_of, domain_table, register_invariant,
};
use serde_json::{Map, Value, json};

#[derive(Clone, Debug)]
struct Medium {
    version: u64,
    tables: IndexMap<String, Map<String, Value>>,
    global: Value,
}

#[derive(Debug, Default)]
struct PoolState {
    media: HashMap<String, Medium>,
    open: HashSet<String>,
}

#[derive(Debug, Default)]
struct MemoryPool {
    state: Mutex<PoolState>,
    fail_next_writes: AtomicUsize,
}

impl MemoryPool {
    fn fail_writes(&self, count: usize) {
        self.fail_next_writes.store(count, Ordering::Release);
    }

    fn maybe_fail(&self) -> Result<(), StorageError> {
        let remaining = self.fail_next_writes.load(Ordering::Acquire);
        if remaining == 0 {
            return Ok(());
        }
        self.fail_next_writes.fetch_sub(1, Ordering::AcqRel);
        Err(StorageError::new(
            StorageErrorCode::MalformedMedium,
            "injected write failure",
        ))
    }
}

#[derive(Debug)]
struct MemoryBackend {
    pool: Arc<MemoryPool>,
    closed: AtomicBool,
}

#[derive(Debug)]
struct NoKv;

impl StorageBackend for NoKv {
    fn close(&self) -> BoxFuture<'static, Result<(), StorageError>> {
        async { Ok(()) }.boxed()
    }
}

impl MemoryBackend {
    fn new(pool: Arc<MemoryPool>) -> Arc<Self> {
        Arc::new(Self {
            pool,
            closed: AtomicBool::new(false),
        })
    }
}

impl StorageBackend for MemoryBackend {
    fn kv(&self) -> Option<Arc<dyn KvFacet>> {
        Some(Arc::new(MemoryFacet {
            pool: self.pool.clone(),
        }))
    }

    fn close(&self) -> BoxFuture<'static, Result<(), StorageError>> {
        self.closed.store(true, Ordering::Release);
        async { Ok(()) }.boxed()
    }
}

#[derive(Debug)]
struct MemoryFacet {
    pool: Arc<MemoryPool>,
}

impl KvFacet for MemoryFacet {
    fn open(
        &self,
        descriptor: KvUnitDescriptor,
    ) -> BoxFuture<'static, anyhow::Result<Arc<dyn KvUnit>>> {
        let pool = self.pool.clone();
        async move {
            descriptor.validate()?;
            let mut state = pool.state.lock();
            if state.open.contains(&descriptor.name) {
                return Err(StorageError::new(
                    StorageErrorCode::MalformedMedium,
                    format!("unit '{}' is already open", descriptor.name),
                )
                .into());
            }
            if let Some(medium) = state.media.get(&descriptor.name) {
                if medium.version != descriptor.version {
                    return Err(StorageError::new(
                        StorageErrorCode::VersionMismatch,
                        format!(
                            "unit '{}' version {} does not match requested {}",
                            descriptor.name, medium.version, descriptor.version
                        ),
                    )
                    .into());
                }
            } else {
                state.media.insert(
                    descriptor.name.clone(),
                    Medium {
                        version: descriptor.version,
                        tables: descriptor
                            .tables
                            .iter()
                            .map(|table| (table.clone(), Map::new()))
                            .collect(),
                        global: Value::Null,
                    },
                );
            }
            state.open.insert(descriptor.name.clone());
            drop(state);
            Ok(Arc::new(MemoryUnit {
                pool,
                descriptor,
                closed: AtomicBool::new(false),
            }) as Arc<dyn KvUnit>)
        }
        .boxed()
    }
}

#[derive(Debug)]
struct MemoryUnit {
    pool: Arc<MemoryPool>,
    descriptor: KvUnitDescriptor,
    closed: AtomicBool,
}

impl MemoryUnit {
    fn check(&self) -> Result<(), StorageError> {
        if self.closed.load(Ordering::Acquire) {
            Err(StorageError::new(
                StorageErrorCode::Closed,
                format!("unit '{}' is closed", self.descriptor.name),
            ))
        } else {
            Ok(())
        }
    }
}

impl KvUnit for MemoryUnit {
    fn load_all(&self) -> BoxFuture<'static, anyhow::Result<KvSnapshot>> {
        let checked = self.check();
        let pool = self.pool.clone();
        let descriptor = self.descriptor.clone();
        async move {
            checked?;
            let state = pool.state.lock();
            let medium = state.media.get(&descriptor.name).unwrap();
            Ok(KvSnapshot {
                tables: medium.tables.clone(),
                global: medium.global.clone(),
            })
        }
        .boxed()
    }

    fn put_record(
        &self,
        table: String,
        key: String,
        value: Value,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        let checked = self.check();
        let pool = self.pool.clone();
        let name = self.descriptor.name.clone();
        async move {
            checked?;
            pool.maybe_fail()?;
            pool.state
                .lock()
                .media
                .get_mut(&name)
                .unwrap()
                .tables
                .get_mut(&table)
                .expect("declared table")
                .insert(key, value);
            Ok(())
        }
        .boxed()
    }

    fn delete_record(&self, table: String, key: String) -> BoxFuture<'static, anyhow::Result<()>> {
        let checked = self.check();
        let pool = self.pool.clone();
        let name = self.descriptor.name.clone();
        async move {
            checked?;
            pool.maybe_fail()?;
            pool.state
                .lock()
                .media
                .get_mut(&name)
                .unwrap()
                .tables
                .get_mut(&table)
                .expect("declared table")
                .remove(&key);
            Ok(())
        }
        .boxed()
    }

    fn set_global(&self, value: Value) -> BoxFuture<'static, anyhow::Result<()>> {
        let checked = self.check();
        let pool = self.pool.clone();
        let name = self.descriptor.name.clone();
        async move {
            checked?;
            pool.maybe_fail()?;
            pool.state.lock().media.get_mut(&name).unwrap().global = value;
            Ok(())
        }
        .boxed()
    }

    fn close(&self) -> BoxFuture<'static, Result<(), StorageError>> {
        let first = !self.closed.swap(true, Ordering::AcqRel);
        let pool = self.pool.clone();
        let name = self.descriptor.name.clone();
        async move {
            if first {
                pool.state.lock().open.remove(&name);
            }
            Ok(())
        }
        .boxed()
    }
}

fn item_schema() -> ValueSchema {
    ValueSchema::new(|value| {
        let object = value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("expected object"))?;
        anyhow::ensure!(object.get("label").is_some_and(Value::is_string));
        anyhow::ensure!(object.get("count").is_some_and(Value::is_i64));
        Ok(json!({
            "label": object["label"].clone(),
            "count": object["count"].clone(),
        }))
    })
}

fn settings_schema() -> ValueSchema {
    ValueSchema::new(|value| {
        let object = value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("expected object"))?;
        anyhow::ensure!(object.get("theme").is_some_and(Value::is_string));
        Ok(json!({ "theme": object["theme"].clone() }))
    })
}

fn spec() -> DomainSpec {
    define_domain(DomainSpec {
        name: "demo".to_owned(),
        version: 1,
        global: Some(DomainGlobalSpec {
            schema: settings_schema(),
            initial: json!({ "theme": "plain" }),
        }),
        tables: IndexMap::from([("items".to_owned(), domain_table(item_schema()))]),
    })
    .unwrap()
}

fn bare_spec() -> DomainSpec {
    define_domain(DomainSpec {
        name: "bare".to_owned(),
        version: 1,
        global: None,
        tables: IndexMap::from([("rows".to_owned(), domain_table(item_schema()))]),
    })
    .unwrap()
}

struct Harness {
    context: Context,
    storage: Arc<Storage>,
    facility: Arc<seekdeep_storage_domain::DomainFacility>,
    pool: Arc<MemoryPool>,
    _storage_effect: seekdeep_cordis::fiber::EffectHandle,
    _backend_registration: seekdeep_storage::BackendRegistration,
    _facility_effect: seekdeep_cordis::fiber::EffectHandle,
    _mount: seekdeep_storage::FormMount,
}

fn harness(config: Option<DomainConfig>, pool: Option<Arc<MemoryPool>>) -> Harness {
    let context = Context::new();
    let storage = Storage::new();
    let storage_effect = storage.provide(&context).unwrap();
    let pool = pool.unwrap_or_default();
    let backend: Arc<dyn StorageBackend> = MemoryBackend::new(pool.clone());
    let backend_registration = storage.backend.register("memory", backend).unwrap();
    let facility = seekdeep_storage_domain::DomainFacility::new(
        context.clone(),
        storage.clone(),
        config.unwrap_or_else(|| DomainConfig {
            backend: "memory".to_owned(),
            routes: HashMap::new(),
        }),
    );
    let (facility_effect, mount) = facility.mount(&context).unwrap();
    Harness {
        context,
        storage,
        facility,
        pool,
        _storage_effect: storage_effect,
        _backend_registration: backend_registration,
        _facility_effect: facility_effect,
        _mount: mount,
    }
}

fn domain_error(error: &anyhow::Error) -> &DomainError {
    error.downcast_ref().expect("DomainError")
}

fn storage_error(error: &anyhow::Error) -> &StorageError {
    error.downcast_ref().expect("StorageError")
}

#[test]
fn define_domain_rejects_invalid_names_and_nullable_global() {
    let invalid = define_domain(DomainSpec {
        name: "Bad-Name".to_owned(),
        version: 1,
        global: None,
        tables: IndexMap::new(),
    })
    .unwrap_err();
    assert!(invalid.to_string().contains("must match"));

    let bad_table = define_domain(DomainSpec {
        name: "ok".to_owned(),
        version: 1,
        global: None,
        tables: IndexMap::from([(
            "Bad Table".to_owned(),
            DomainTableSpec {
                value_schema: item_schema(),
            },
        )]),
    })
    .unwrap_err();
    assert!(bad_table.to_string().contains("table name"));

    let nullable = define_domain(DomainSpec {
        name: "ok".to_owned(),
        version: 1,
        global: Some(DomainGlobalSpec {
            schema: ValueSchema::new(|value| Ok(value.clone())),
            initial: Value::Null,
        }),
        tables: IndexMap::new(),
    })
    .unwrap_err();
    assert!(nullable.to_string().contains("must not accept null"));
}

#[tokio::test]
async fn opens_routes_validates_and_releases_failed_or_closed_names() {
    let healthy = harness(None, None);
    let domain = healthy.facility.open(spec()).await.unwrap();
    domain
        .table("items")
        .unwrap()
        .put("a".to_owned(), json!({ "label": "first", "count": 1 }))
        .await
        .unwrap();
    let duplicate = healthy.facility.open(spec()).await.unwrap_err();
    assert_eq!(domain_error(&duplicate).code, DomainErrorCode::AlreadyOpen);
    assert_eq!(
        domain.table("items").unwrap().get("a").unwrap(),
        Some(json!({ "label": "first", "count": 1 }))
    );
    domain.close().await.unwrap();
    assert!(healthy.facility.open(spec()).await.is_ok());

    let routed = harness(
        Some(DomainConfig {
            backend: "memory".to_owned(),
            routes: HashMap::from([("demo".to_owned(), "missing".to_owned())]),
        }),
        None,
    );
    let missing = routed.facility.open(spec()).await.unwrap_err();
    assert_eq!(
        storage_error(&missing).code,
        StorageErrorCode::BackendNotFound
    );

    let no_kv = harness(None, None);
    let _registration = no_kv
        .storage
        .backend
        .register("nokv", Arc::new(NoKv))
        .unwrap();
    let facility = seekdeep_storage_domain::DomainFacility::new(
        no_kv.context.clone(),
        no_kv.storage.clone(),
        DomainConfig {
            backend: "nokv".to_owned(),
            routes: HashMap::new(),
        },
    );
    let unsupported = facility.open(spec()).await.unwrap_err();
    assert_eq!(
        domain_error(&unsupported).code,
        DomainErrorCode::FacetUnsupported
    );
}

#[tokio::test]
async fn durable_validation_names_record_and_global_slots_and_passes_version_mismatch() {
    let pool = Arc::new(MemoryPool::default());
    pool.state.lock().media.insert(
        "demo".to_owned(),
        Medium {
            version: 1,
            tables: IndexMap::from([(
                "items".to_owned(),
                Map::from_iter([("bad".to_owned(), json!({ "label": "x", "count": "NaN" }))]),
            )]),
            global: Value::Null,
        },
    );
    let invalid = harness(None, Some(pool.clone()))
        .facility
        .open(spec())
        .await
        .unwrap_err();
    let invalid = domain_error(&invalid);
    assert_eq!(invalid.code, DomainErrorCode::InvalidRecord);
    assert_eq!(
        invalid.detail,
        Some(seekdeep_storage_domain::InvalidRecordDetail {
            table: "items".to_owned(),
            key: "bad".to_owned(),
        })
    );

    pool.state.lock().media.get_mut("demo").unwrap().tables = IndexMap::new();
    pool.state.lock().media.get_mut("demo").unwrap().global = json!({ "theme": 42 });
    let invalid_global = harness(None, Some(pool.clone()))
        .facility
        .open(spec())
        .await
        .unwrap_err();
    assert_eq!(
        domain_error(&invalid_global).detail,
        Some(seekdeep_storage_domain::InvalidRecordDetail {
            table: String::new(),
            key: String::new(),
        })
    );

    pool.state.lock().media.get_mut("demo").unwrap().version = 7;
    let mismatch = harness(None, Some(pool))
        .facility
        .open(spec())
        .await
        .unwrap_err();
    assert_eq!(
        storage_error(&mismatch).code,
        StorageErrorCode::VersionMismatch
    );
}

#[tokio::test]
async fn table_reads_are_snapshots_and_concurrent_updates_are_lossless() {
    let harness = harness(None, None);
    let domain = harness.facility.open(spec()).await.unwrap();
    let table = domain.table("items").unwrap();
    table
        .put("counter".to_owned(), json!({ "label": "c", "count": 0 }))
        .await
        .unwrap();
    let updates = (0..50).map(|_| {
        table.update("counter".to_owned(), |current| {
            Ok(json!({
                "label": current["label"].clone(),
                "count": current["count"].as_i64().unwrap() + 1,
            }))
        })
    });
    futures::future::try_join_all(updates).await.unwrap();
    assert_eq!(table.get("counter").unwrap().unwrap()["count"], 50);
    let entries = table.entries().unwrap();
    let keys = table.keys().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(keys, ["counter"]);
    table
        .put("later".to_owned(), json!({ "label": "l", "count": 1 }))
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(table.len().unwrap(), 2);
    assert!(domain.table("nope").is_err());

    let missing = table
        .update("ghost".to_owned(), |value| Ok(value.clone()))
        .await
        .unwrap_err();
    assert_eq!(domain_error(&missing).code, DomainErrorCode::MissingKey);
    assert!(table.delete("later".to_owned()).await.unwrap());
    assert!(!table.delete("later".to_owned()).await.unwrap());
}

#[tokio::test]
async fn events_follow_durability_and_failures_leave_memory_untouched() {
    let harness = harness(None, None);
    let mut events = harness.facility.subscribe();
    let domain = harness.facility.open(spec()).await.unwrap();
    let table = domain.table("items").unwrap();
    table
        .put("a".to_owned(), json!({ "label": "x", "count": 1 }))
        .await
        .unwrap();
    table
        .update("a".to_owned(), |current| {
            Ok(json!({ "label": "x", "count": current["count"].as_i64().unwrap() + 1 }))
        })
        .await
        .unwrap();
    assert!(table.delete("a".to_owned()).await.unwrap());
    assert!(!table.delete("a".to_owned()).await.unwrap());
    domain.global_set(json!({ "theme": "dark" })).await.unwrap();
    let mut changes = Vec::new();
    for _ in 0..4 {
        changes.push(events.next().await.unwrap().unwrap());
    }
    assert_eq!(
        changes,
        vec![
            DomainChanged::Put {
                domain: "demo".to_owned(),
                table: "items".to_owned(),
                key: "a".to_owned(),
                value: json!({ "label": "x", "count": 1 }),
            },
            DomainChanged::Put {
                domain: "demo".to_owned(),
                table: "items".to_owned(),
                key: "a".to_owned(),
                value: json!({ "label": "x", "count": 2 }),
            },
            DomainChanged::Deleted {
                domain: "demo".to_owned(),
                table: "items".to_owned(),
                key: "a".to_owned(),
            },
            DomainChanged::Put {
                domain: "demo".to_owned(),
                table: String::new(),
                key: String::new(),
                value: json!({ "theme": "dark" }),
            },
        ]
    );

    table
        .put("safe".to_owned(), json!({ "label": "x", "count": 1 }))
        .await
        .unwrap();
    let _ = events.next().await;
    harness.pool.fail_writes(4);
    assert!(
        table
            .put("safe".to_owned(), json!({ "label": "x", "count": 9 }))
            .await
            .is_err()
    );
    assert!(
        table
            .update("safe".to_owned(), |value| Ok(value.clone()))
            .await
            .is_err()
    );
    assert!(table.delete("safe".to_owned()).await.is_err());
    assert!(domain.global_set(json!({ "theme": "lost" })).await.is_err());
    assert_eq!(
        table.get("safe").unwrap(),
        Some(json!({ "label": "x", "count": 1 }))
    );
    assert_eq!(domain.global_get().unwrap(), json!({ "theme": "dark" }));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), events.next())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn invariant_accepts_real_writes_and_rejects_each_stale_known_shape() {
    let harness = harness(None, None);
    let registry = InvariantRegistry::install(&harness.context, &InvariantConfig::default())
        .expect("invariant registry");
    let registration = register_invariant(&registry).expect("domain invariant");
    registration.await_ready().await.expect("invariant ready");

    let domain = harness.facility.open(spec()).await.expect("open domain");
    let table = domain.table("items").expect("items table");
    table
        .put("a".to_owned(), json!({"label": "x", "count": 1}))
        .await
        .expect("real put");
    table
        .update("a".to_owned(), |current| {
            Ok(json!({"label": "x", "count": current["count"].as_i64().unwrap() + 1}))
        })
        .await
        .expect("real update");
    domain
        .global_set(json!({"theme": "dark"}))
        .await
        .expect("real global set");

    for (change, diagnostic) in [
        (
            DomainChanged::Put {
                domain: "ghost".to_owned(),
                table: "items".to_owned(),
                key: "a".to_owned(),
                value: json!({"label": "x", "count": 2}),
            },
            "not open",
        ),
        (
            DomainChanged::Put {
                domain: "demo".to_owned(),
                table: "items".to_owned(),
                key: "a".to_owned(),
                value: json!({"label": "x", "count": 999}),
            },
            "differs from the in-memory record",
        ),
        (
            DomainChanged::Deleted {
                domain: "demo".to_owned(),
                table: "items".to_owned(),
                key: "a".to_owned(),
            },
            "still in memory",
        ),
        (
            DomainChanged::Put {
                domain: "demo".to_owned(),
                table: String::new(),
                key: String::new(),
                value: json!({"theme": "wrong"}),
            },
            "differs from the in-memory global",
        ),
    ] {
        let error = harness
            .context
            .events()
            .emit(&harness.context, "domain/changed", &EventArgs::one(change))
            .expect_err("stale change must violate the invariant");
        assert!(error.to_string().contains(diagnostic), "{error:#}");
        assert!(
            error
                .to_string()
                .contains("@deepseek-ai/seekdeep-storage-domain"),
            "{error:#}"
        );
    }
}

#[tokio::test]
async fn invariant_tolerates_an_untyped_operation_outside_the_closed_union() {
    let harness = harness(None, None);
    let registry = InvariantRegistry::install(&harness.context, &InvariantConfig::default())
        .expect("invariant registry");
    let registration = register_invariant(&registry).expect("domain invariant");
    registration.await_ready().await.expect("invariant ready");
    let domain = harness.facility.open(spec()).await.expect("open domain");
    domain
        .table("items")
        .expect("items table")
        .put("a".to_owned(), json!({"label": "x", "count": 1}))
        .await
        .expect("seed record");

    harness
        .context
        .events()
        .emit(
            &harness.context,
            "domain/changed",
            &EventArgs::one(json!({
                "domain": "demo",
                "table": "items",
                "key": "a",
                "operation": "exotic",
            })),
        )
        .expect("unknown operation is ignored at the untyped event boundary");
}

#[tokio::test]
async fn global_initial_is_lazy_and_close_drains_rejects_and_frees_name() {
    let harness = harness(None, None);
    let domain = harness.facility.open(spec()).await.unwrap();
    assert_eq!(domain.global_get().unwrap(), json!({ "theme": "plain" }));
    assert!(harness.pool.state.lock().media["demo"].global.is_null());
    domain.global_set(json!({ "theme": "dark" })).await.unwrap();
    assert_eq!(
        harness.pool.state.lock().media["demo"].global,
        json!({ "theme": "dark" })
    );
    let table = domain.table("items").unwrap();
    let pending = table.put("a".to_owned(), json!({ "label": "x", "count": 1 }));
    let closing = domain.close();
    pending.await.unwrap();
    closing.await.unwrap();
    assert_eq!(
        domain_error(&table.get("a").unwrap_err()).code,
        DomainErrorCode::Closed
    );
    assert_eq!(
        domain_error(
            &table
                .put("b".to_owned(), json!({ "label": "y", "count": 2 }))
                .await
                .unwrap_err()
        )
        .code,
        DomainErrorCode::Closed
    );
    domain.close().await.unwrap();
    assert!(harness.facility.open(spec()).await.is_ok());

    let bare = harness.facility.open(bare_spec()).await.unwrap();
    assert!(
        bare.global_get()
            .unwrap_err()
            .to_string()
            .contains("declares no global")
    );
}

#[tokio::test]
async fn throwing_listener_is_contained_after_commit() {
    let harness = harness(None, None);
    harness
        .context
        .events()
        .on_sync(
            &harness.context,
            "domain/changed",
            |_context, _args| anyhow::bail!("observer exploded"),
            EventOptions::default(),
        )
        .unwrap();
    let domain = harness.facility.open(spec()).await.unwrap();
    let table = domain.table("items").unwrap();
    table
        .put("a".to_owned(), json!({ "label": "x", "count": 1 }))
        .await
        .unwrap();
    assert_eq!(
        table.get("a").unwrap(),
        Some(json!({ "label": "x", "count": 1 }))
    );
}

#[tokio::test]
async fn sequenced_subscribers_are_lossless_beyond_the_old_broadcast_capacity() {
    const CHANGE_COUNT: usize = 1_500;

    let harness = harness(None, None);
    let mut changes = harness.facility.subscribe_sequenced();
    let domain = harness.facility.open(spec()).await.unwrap();
    let table = domain.table("items").unwrap();

    for index in 0..CHANGE_COUNT {
        table
            .put(
                format!("item-{index}"),
                json!({
                    "label": "queued",
                    "count": i64::try_from(index).expect("bounded test index"),
                }),
            )
            .await
            .unwrap();
    }

    for expected in 1..=CHANGE_COUNT {
        let (sequence, change) = changes.next().await.unwrap().unwrap();
        assert_eq!(sequence, expected as u64);
        assert_eq!(change.key(), format!("item-{}", expected - 1));
    }
    assert_eq!(harness.facility.change_sequence(), CHANGE_COUNT as u64);
}

#[test]
fn descriptor_projection_is_exact() {
    assert_eq!(
        descriptor_of(&spec()),
        KvUnitDescriptor {
            name: "demo".to_owned(),
            version: 1,
            tables: vec!["items".to_owned()],
            has_global: true,
        }
    );
}

#[tokio::test]
async fn table_handles_are_stable_and_keep_domain_machinery_owned() {
    let harness = harness(None, None);
    let domain = harness.facility.open(spec()).await.unwrap();
    let first = domain.table("items").unwrap();
    let second = domain.table("items").unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    drop(domain);
    first
        .put("retained".to_owned(), json!({ "label": "x", "count": 1 }))
        .await
        .unwrap();
    assert_eq!(
        second.get("retained").unwrap(),
        Some(json!({ "label": "x", "count": 1 }))
    );
    harness.facility.close_all().await.unwrap();
}

#[test]
fn failed_facility_service_publication_rolls_back_the_form_mount() {
    let context = Context::new();
    let storage = Storage::new();
    let _storage_effect = storage.provide(&context).unwrap();
    let facility = seekdeep_storage_domain::DomainFacility::new(
        context.clone(),
        storage.clone(),
        DomainConfig {
            backend: "memory".to_owned(),
            routes: HashMap::new(),
        },
    );
    let _occupied = context
        .provide(seekdeep_storage_domain::STORAGE_DOMAIN, facility.clone())
        .unwrap();
    assert!(facility.mount(&context).is_err());
    assert!(
        storage
            .form::<seekdeep_storage_domain::DomainFacility>("domain")
            .is_err()
    );
}

#[tokio::test]
async fn plugin_waits_for_backends_and_drains_before_unmounting() {
    let context = Context::new();
    let storage = Storage::new();
    let _storage_effect = storage.provide(&context).unwrap();
    let pool = Arc::new(MemoryPool::default());
    let backend = MemoryBackend::new(pool);
    let _registration = storage
        .backend
        .register("memory", backend.clone() as Arc<dyn StorageBackend>)
        .unwrap();
    let mounted = context
        .plugin(
            seekdeep_storage_domain::plugin(),
            json!({ "backend": "memory" }),
        )
        .unwrap();
    mounted.await_settled().await.unwrap();
    assert_eq!(mounted.fiber().state(), FiberState::Active);
    assert!(
        context
            .get(seekdeep_storage_domain::STORAGE_DOMAIN)
            .is_none()
    );
    assert!(
        storage
            .form::<seekdeep_storage_domain::DomainFacility>("domain")
            .is_err()
    );

    let backend_service = context
        .provide_named(&storage_backend_service_key("memory"), backend)
        .unwrap();
    let facility = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if let Some(facility) = context.get(seekdeep_storage_domain::STORAGE_DOMAIN) {
                break facility;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("configured backend should activate the domain form");
    assert!(Arc::ptr_eq(
        &storage
            .form::<seekdeep_storage_domain::DomainFacility>("domain")
            .unwrap(),
        &facility
    ));
    let domain = facility.open(bare_spec()).await.unwrap();
    let table = domain.table("rows").unwrap();
    table
        .put("a".to_owned(), json!({ "label": "x", "count": 1 }))
        .await
        .unwrap();

    backend_service.dispose().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if context
                .get(seekdeep_storage_domain::STORAGE_DOMAIN)
                .is_none()
                && storage
                    .form::<seekdeep_storage_domain::DomainFacility>("domain")
                    .is_err()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("backend withdrawal should drain and unmount the form");
    assert_eq!(
        domain_error(
            &table
                .put("b".to_owned(), json!({ "label": "y", "count": 2 }))
                .await
                .unwrap_err()
        )
        .code,
        DomainErrorCode::Closed
    );
    mounted.dispose().await.unwrap();
}
