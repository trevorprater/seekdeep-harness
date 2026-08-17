//! One eagerly publishing JSON KV unit.

use std::{
    path::PathBuf,
    sync::{Arc, Weak},
};

use futures::{FutureExt as _, future::BoxFuture};
use parking_lot::Mutex;
use seekdeep_storage::{KvSnapshot, KvUnit, KvUnitDescriptor, StorageError, StorageErrorCode};
use serde_json::Value;
use tokio::sync::{Notify, oneshot};

use crate::{BackendInner, UnitState, parse, release_unit, serialize, write_atomic};

#[derive(Debug, Default)]
struct Lifecycle {
    closed: bool,
    in_flight: usize,
    released: bool,
}

/// Open JSON unit; public through the backend-neutral [`KvUnit`] trait.
#[derive(Debug)]
pub(crate) struct JsonKvUnit {
    descriptor: KvUnitDescriptor,
    path: PathBuf,
    state: Arc<Mutex<UnitState>>,
    lifecycle: Arc<Mutex<Lifecycle>>,
    changed: Arc<Notify>,
    backend: Weak<BackendInner>,
}

pub(crate) async fn open_json_unit(
    descriptor: KvUnitDescriptor,
    path: PathBuf,
    backend: Weak<BackendInner>,
) -> anyhow::Result<Arc<JsonKvUnit>> {
    let state = match tokio::fs::read_to_string(&path).await {
        Ok(text) => parse(&text, &descriptor)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => UnitState {
            version: descriptor.version,
            global: Value::Null,
            tables: descriptor
                .tables
                .iter()
                .map(|table| (table.clone(), serde_json::Map::new()))
                .collect(),
        },
        Err(error) => return Err(error.into()),
    };
    Ok(Arc::new(JsonKvUnit {
        descriptor,
        path,
        state: Arc::new(Mutex::new(state)),
        lifecycle: Arc::new(Mutex::new(Lifecycle::default())),
        changed: Arc::new(Notify::new()),
        backend,
    }))
}

impl JsonKvUnit {
    fn closed_error(&self) -> StorageError {
        StorageError::new(
            StorageErrorCode::Closed,
            format!("unit '{}' is closed", self.descriptor.name),
        )
    }

    fn publish(
        &self,
        data: String,
        rollback: impl FnOnce(&mut UnitState) + Send + 'static,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        let path = self.path.clone();
        let state = self.state.clone();
        let lifecycle = self.lifecycle.clone();
        let changed = self.changed.clone();
        let (send, receive) = oneshot::channel();
        tokio::spawn(async move {
            let result = write_atomic(&path, &data).await.map_err(anyhow::Error::new);
            if result.is_err() {
                rollback(&mut state.lock());
            }
            lifecycle.lock().in_flight -= 1;
            changed.notify_waiters();
            let _ = send.send(result);
        });
        async move {
            receive
                .await
                .map_err(|_| anyhow::anyhow!("JSON unit publish task stopped"))?
        }
        .boxed()
    }

    fn begin_write<R>(
        &self,
        mutate: impl FnOnce(&mut UnitState) -> anyhow::Result<R>,
        rollback: impl FnOnce(&mut UnitState, R) + Send + 'static,
    ) -> BoxFuture<'static, anyhow::Result<()>>
    where
        R: Send + 'static,
    {
        let mut lifecycle = self.lifecycle.lock();
        if lifecycle.closed {
            let error = self.closed_error();
            return async move { Err(error.into()) }.boxed();
        }
        let (previous, data) = {
            let mut state = self.state.lock();
            let previous = match mutate(&mut state) {
                Ok(previous) => previous,
                Err(error) => return async move { Err(error) }.boxed(),
            };
            (previous, serialize(&self.descriptor.name, &state))
        };
        lifecycle.in_flight += 1;
        drop(lifecycle);
        self.publish(data, move |state| rollback(state, previous))
    }

    async fn finish_close(
        descriptor_name: String,
        backend: Weak<BackendInner>,
        lifecycle: Arc<Mutex<Lifecycle>>,
        changed: Arc<Notify>,
    ) {
        loop {
            let notified = changed.notified();
            let release = {
                let mut lifecycle = lifecycle.lock();
                if lifecycle.in_flight == 0 && !lifecycle.released {
                    lifecycle.released = true;
                    true
                } else if lifecycle.released {
                    return;
                } else {
                    false
                }
            };
            if release {
                release_unit(&backend, &descriptor_name);
                changed.notify_waiters();
                return;
            }
            notified.await;
        }
    }
}

impl KvUnit for JsonKvUnit {
    fn load_all(&self) -> BoxFuture<'static, anyhow::Result<KvSnapshot>> {
        if self.lifecycle.lock().closed {
            let error = self.closed_error();
            return async move { Err(error.into()) }.boxed();
        }
        let state = self.state.lock().clone();
        async move {
            Ok(KvSnapshot {
                tables: state.tables,
                global: state.global,
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
        let unit = self.descriptor.name.clone();
        let rollback_table = table.clone();
        let rollback_key = key.clone();
        self.begin_write(
            move |state| {
                let records = state.tables.get_mut(&table).ok_or_else(|| {
                    anyhow::anyhow!("unit '{unit}' does not declare table '{table}'")
                })?;
                let previous = records.insert(key, value);
                Ok(previous)
            },
            move |state, previous| {
                let records = state
                    .tables
                    .get_mut(&rollback_table)
                    .expect("validated table remains declared");
                if let Some(previous) = previous {
                    records.insert(rollback_key, previous);
                } else {
                    records.remove(&rollback_key);
                }
            },
        )
    }

    fn delete_record(&self, table: String, key: String) -> BoxFuture<'static, anyhow::Result<()>> {
        let mut lifecycle = self.lifecycle.lock();
        if lifecycle.closed {
            let error = self.closed_error();
            return async move { Err(error.into()) }.boxed();
        }
        let (previous, data) = {
            let mut state = self.state.lock();
            let Some(records) = state.tables.get_mut(&table) else {
                let unit = self.descriptor.name.clone();
                return async move {
                    anyhow::bail!("unit '{unit}' does not declare table '{table}'")
                }
                .boxed();
            };
            let Some(previous) = records.remove(&key) else {
                return async { Ok(()) }.boxed();
            };
            (previous, serialize(&self.descriptor.name, &state))
        };
        lifecycle.in_flight += 1;
        drop(lifecycle);
        self.publish(data, move |state| {
            state
                .tables
                .get_mut(&table)
                .expect("validated table remains declared")
                .insert(key, previous);
        })
    }

    fn set_global(&self, value: Value) -> BoxFuture<'static, anyhow::Result<()>> {
        let has_global = self.descriptor.has_global;
        let unit = self.descriptor.name.clone();
        self.begin_write(
            move |state| {
                anyhow::ensure!(has_global, "unit '{unit}' does not declare a global slot");
                Ok(std::mem::replace(&mut state.global, value))
            },
            |state, previous| state.global = previous,
        )
    }

    fn close(&self) -> BoxFuture<'static, Result<(), StorageError>> {
        self.lifecycle.lock().closed = true;
        let name = self.descriptor.name.clone();
        let backend = self.backend.clone();
        let lifecycle = self.lifecycle.clone();
        let changed = self.changed.clone();
        let (send, receive) = oneshot::channel();
        tokio::spawn(async move {
            Self::finish_close(name, backend, lifecycle, changed).await;
            let _ = send.send(());
        });
        async move {
            receive.await.map_err(|_| {
                StorageError::new(StorageErrorCode::Closed, "JSON unit close task stopped")
            })
        }
        .boxed()
    }
}
