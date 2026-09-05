#![allow(dead_code)]

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use futures::{FutureExt as _, future::BoxFuture};
use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_core::session::{SessionEvent, SessionHeader, SessionId};
use seekdeep_llm::AbortSignal;
use seekdeep_session_persistence::{
    SessionInspection, SessionLocation, SessionPersistence, SessionPersistenceSnapshot,
};
use seekdeep_storage::{
    KvFacet, KvSnapshot, KvUnit, KvUnitDescriptor, StorageBackend, StorageError, StorageErrorCode,
};
use serde_json::{Map, Value};

#[derive(Clone, Debug)]
pub(crate) struct Medium {
    pub(crate) version: u64,
    pub(crate) tables: IndexMap<String, Map<String, Value>>,
    pub(crate) global: Value,
}

#[derive(Debug, Default)]
pub(crate) struct Pool {
    pub(crate) media: Mutex<HashMap<String, Medium>>,
    open: Mutex<HashSet<String>>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct FailAt {
    pub(crate) put: Option<usize>,
    pub(crate) delete: Option<usize>,
    pub(crate) global: Vec<usize>,
}

#[derive(Clone, Debug)]
pub(crate) struct MemoryBackend {
    pub(crate) pool: Arc<Pool>,
    failures: FailAt,
    puts: Arc<AtomicUsize>,
    deletes: Arc<AtomicUsize>,
    globals: Arc<AtomicUsize>,
}

impl MemoryBackend {
    pub(crate) fn new(pool: Arc<Pool>) -> Arc<Self> {
        Self::failing(pool, FailAt::default())
    }

    pub(crate) fn failing(pool: Arc<Pool>, failures: FailAt) -> Arc<Self> {
        Arc::new(Self {
            pool,
            failures,
            puts: Arc::new(AtomicUsize::new(0)),
            deletes: Arc::new(AtomicUsize::new(0)),
            globals: Arc::new(AtomicUsize::new(0)),
        })
    }

    fn fails(counter: &AtomicUsize, selected: Option<usize>) -> bool {
        let call = counter.fetch_add(1, Ordering::AcqRel) + 1;
        selected == Some(call)
    }
}

impl StorageBackend for MemoryBackend {
    fn kv(&self) -> Option<Arc<dyn KvFacet>> {
        Some(Arc::new(MemoryFacet {
            backend: self.clone(),
        }))
    }

    fn close(&self) -> BoxFuture<'static, Result<(), StorageError>> {
        async { Ok(()) }.boxed()
    }
}

#[derive(Debug)]
struct MemoryFacet {
    backend: MemoryBackend,
}

impl KvFacet for MemoryFacet {
    fn open(
        &self,
        descriptor: KvUnitDescriptor,
    ) -> BoxFuture<'static, anyhow::Result<Arc<dyn KvUnit>>> {
        let backend = self.backend.clone();
        async move {
            descriptor.validate()?;
            let mut media = backend.pool.media.lock();
            if let Some(existing) = media.get(&descriptor.name) {
                if existing.version != descriptor.version {
                    return Err(StorageError::new(
                        StorageErrorCode::VersionMismatch,
                        "version mismatch",
                    )
                    .into());
                }
            } else {
                media.insert(
                    descriptor.name.clone(),
                    Medium {
                        version: descriptor.version,
                        tables: descriptor
                            .tables
                            .iter()
                            .map(|name| (name.clone(), Map::new()))
                            .collect(),
                        global: Value::Null,
                    },
                );
            }
            drop(media);
            if !backend.pool.open.lock().insert(descriptor.name.clone()) {
                return Err(StorageError::new(
                    StorageErrorCode::MalformedMedium,
                    "unit already open",
                )
                .into());
            }
            Ok(Arc::new(MemoryUnit {
                backend,
                name: descriptor.name,
                closed: Mutex::new(false),
            }) as Arc<dyn KvUnit>)
        }
        .boxed()
    }
}

#[derive(Debug)]
struct MemoryUnit {
    backend: MemoryBackend,
    name: String,
    closed: Mutex<bool>,
}

impl KvUnit for MemoryUnit {
    fn load_all(&self) -> BoxFuture<'static, anyhow::Result<KvSnapshot>> {
        let medium = self.backend.pool.media.lock()[&self.name].clone();
        async move {
            Ok(KvSnapshot {
                tables: medium.tables,
                global: medium.global,
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
        let backend = self.backend.clone();
        let name = self.name.clone();
        async move {
            if MemoryBackend::fails(&backend.puts, backend.failures.put) {
                return Err(StorageError::new(
                    StorageErrorCode::MalformedMedium,
                    "selected bootstrap put failure",
                )
                .into());
            }
            backend.pool.media.lock().get_mut(&name).unwrap().tables[&table].insert(key, value);
            Ok(())
        }
        .boxed()
    }

    fn delete_record(&self, table: String, key: String) -> BoxFuture<'static, anyhow::Result<()>> {
        let backend = self.backend.clone();
        let name = self.name.clone();
        async move {
            if MemoryBackend::fails(&backend.deletes, backend.failures.delete) {
                return Err(StorageError::new(
                    StorageErrorCode::MalformedMedium,
                    "selected rollback delete failure",
                )
                .into());
            }
            backend.pool.media.lock().get_mut(&name).unwrap().tables[&table].remove(&key);
            Ok(())
        }
        .boxed()
    }

    fn set_global(&self, value: Value) -> BoxFuture<'static, anyhow::Result<()>> {
        let backend = self.backend.clone();
        let name = self.name.clone();
        async move {
            let call = backend.globals.fetch_add(1, Ordering::AcqRel) + 1;
            if backend.failures.global.contains(&call) {
                return Err(StorageError::new(
                    StorageErrorCode::MalformedMedium,
                    "selected bootstrap marker failure",
                )
                .into());
            }
            backend.pool.media.lock().get_mut(&name).unwrap().global = value;
            Ok(())
        }
        .boxed()
    }

    fn close(&self) -> BoxFuture<'static, Result<(), StorageError>> {
        let first = !std::mem::replace(&mut *self.closed.lock(), true);
        let pool = self.backend.pool.clone();
        let name = self.name.clone();
        async move {
            if first {
                pool.open.lock().remove(&name);
            }
            Ok(())
        }
        .boxed()
    }
}

#[derive(Debug, Default)]
pub(crate) struct Headers {
    pub(crate) values: Mutex<Vec<SessionHeader>>,
    pub(crate) list_calls: AtomicUsize,
    pub(crate) fail_list: Mutex<Option<String>>,
}

#[async_trait]
impl SessionPersistence for Headers {
    fn locate(&self, _meta: &SessionHeader) -> Option<SessionLocation> {
        None
    }

    fn supports_raw_artifacts(&self) -> bool {
        false
    }

    async fn create(&self, _meta: &SessionHeader) -> anyhow::Result<()> {
        anyhow::bail!("event bodies must not be created")
    }

    async fn append(&self, _id: &SessionId, _events: &[SessionEvent]) -> anyhow::Result<()> {
        anyhow::bail!("event bodies must not be appended")
    }

    async fn load(&self, _id: &SessionId) -> anyhow::Result<SessionInspection> {
        anyhow::bail!("event bodies must not be loaded")
    }

    async fn inspect(
        &self,
        _id: &SessionId,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionInspection> {
        anyhow::bail!("event bodies must not be inspected")
    }

    async fn read_from(
        &self,
        _id: &SessionId,
        _from_seq: u64,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionInspection> {
        anyhow::bail!("event bodies must not be read")
    }

    async fn list(&self, _signal: Option<AbortSignal>) -> anyhow::Result<Vec<SessionHeader>> {
        self.list_calls.fetch_add(1, Ordering::AcqRel);
        if let Some(message) = self.fail_list.lock().clone() {
            anyhow::bail!(message);
        }
        Ok(self.values.lock().clone())
    }

    async fn list_snapshots(
        &self,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<Vec<SessionPersistenceSnapshot>> {
        anyhow::bail!("snapshots must not be listed")
    }
}

pub(crate) fn header(id: &str, cwd: Option<&str>, created_at: u64) -> SessionHeader {
    let mut value = SessionHeader::new(SessionId::new(id));
    value.cwd = cwd.map(ToOwned::to_owned);
    value.created_at = created_at;
    value
}

pub(crate) fn stored_workspace(pool: &Arc<Pool>, entries: Vec<(String, Value)>, global: Value) {
    pool.media.lock().insert(
        "workspace".to_owned(),
        Medium {
            version: 2,
            tables: IndexMap::from([("workspaces".to_owned(), entries.into_iter().collect())]),
            global,
        },
    );
}
