//! One opened `SQLite` KV unit.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Weak},
};

use futures::{FutureExt as _, future::BoxFuture};
use indexmap::IndexMap;
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension as _, params};
use seekdeep_storage::{KvSnapshot, KvUnit, KvUnitDescriptor, StorageError, StorageErrorCode};
use serde_json::{Map, Value};

use crate::{BackendInner, record_table_name, release_unit};

pub(crate) type SharedDatabase = Arc<Mutex<Option<Connection>>>;

/// Open handle over one descriptor's physical rows.
pub(crate) struct SqliteKvUnit {
    database: SharedDatabase,
    descriptor: KvUnitDescriptor,
    backend: Weak<BackendInner>,
    closed: AtomicBool,
}

impl std::fmt::Debug for SqliteKvUnit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SqliteKvUnit")
            .field("descriptor", &self.descriptor)
            .field("closed", &self.closed.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl SqliteKvUnit {
    pub(crate) fn new(
        database: SharedDatabase,
        descriptor: KvUnitDescriptor,
        backend: Weak<BackendInner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            database,
            descriptor,
            backend,
            closed: AtomicBool::new(false),
        })
    }

    fn ensure_open(&self) -> anyhow::Result<()> {
        if self.closed.load(Ordering::Acquire) {
            Err(StorageError::new(
                StorageErrorCode::Closed,
                format!("kv unit '{}' is closed", self.descriptor.name),
            )
            .into())
        } else {
            Ok(())
        }
    }

    fn database<T>(
        &self,
        operation: impl FnOnce(&Connection) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        self.ensure_open()?;
        let database = self.database.lock();
        let database = database.as_ref().ok_or_else(|| {
            StorageError::new(StorageErrorCode::Closed, "sqlite storage backend is closed")
        })?;
        operation(database)
    }

    fn table(&self, table: &str) -> anyhow::Result<String> {
        if self.descriptor.tables.iter().any(|item| item == table) {
            Ok(record_table_name(&self.descriptor.name, table))
        } else {
            anyhow::bail!(
                "kv unit '{}' declared no table '{table}'",
                self.descriptor.name
            )
        }
    }

    fn parse_value(&self, text: &str, slot: &str) -> anyhow::Result<Value> {
        serde_json::from_str(text).map_err(|error| {
            StorageError::with_source(
                StorageErrorCode::MalformedMedium,
                format!(
                    "kv unit '{}' holds unparsable JSON at {slot}",
                    self.descriptor.name
                ),
                error.into(),
            )
            .into()
        })
    }
}

impl KvUnit for SqliteKvUnit {
    fn load_all(&self) -> BoxFuture<'static, anyhow::Result<KvSnapshot>> {
        let result = self.database(|database| {
            let mut tables = IndexMap::new();
            for table in &self.descriptor.tables {
                let physical = record_table_name(&self.descriptor.name, table);
                let mut statement =
                    database.prepare(&format!("SELECT key, value FROM \"{physical}\""))?;
                let rows = statement.query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?;
                let mut records = Map::new();
                for row in rows {
                    let (key, text) = row?;
                    let value = self.parse_value(&text, &format!("table '{table}' key '{key}'"))?;
                    records.insert(key, value);
                }
                tables.insert(table.clone(), records);
            }
            let global = if self.descriptor.has_global {
                let text = database
                    .query_row(
                        "SELECT value FROM unit_globals WHERE unit = ?1",
                        [&self.descriptor.name],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?;
                text.map_or(Ok(Value::Null), |text| {
                    self.parse_value(&text, "global slot")
                })?
            } else {
                Value::Null
            };
            Ok(KvSnapshot { tables, global })
        });
        futures::future::ready(result).boxed()
    }

    fn put_record(
        &self,
        table: String,
        key: String,
        value: Value,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        let result = self.ensure_open().and_then(|()| {
            self.table(&table).and_then(|physical| {
                self.database(|database| {
                    let text = serde_json::to_string(&value)?;
                    database.execute(
                        &format!(
                            "INSERT INTO \"{physical}\" (key, value) VALUES (?1, ?2) \
                             ON CONFLICT(key) DO UPDATE SET value = excluded.value"
                        ),
                        params![key, text],
                    )?;
                    Ok(())
                })
            })
        });
        futures::future::ready(result).boxed()
    }

    fn delete_record(&self, table: String, key: String) -> BoxFuture<'static, anyhow::Result<()>> {
        let result = self.ensure_open().and_then(|()| {
            self.table(&table).and_then(|physical| {
                self.database(|database| {
                    database
                        .execute(&format!("DELETE FROM \"{physical}\" WHERE key = ?1"), [key])?;
                    Ok(())
                })
            })
        });
        futures::future::ready(result).boxed()
    }

    fn set_global(&self, value: Value) -> BoxFuture<'static, anyhow::Result<()>> {
        let result = self.ensure_open().and_then(|()| {
            if self.descriptor.has_global {
                self.database(|database| {
                    let text = serde_json::to_string(&value)?;
                    database.execute(
                        "INSERT INTO unit_globals (unit, value) VALUES (?1, ?2) \
                         ON CONFLICT(unit) DO UPDATE SET value = excluded.value",
                        params![self.descriptor.name, text],
                    )?;
                    Ok(())
                })
            } else {
                Err(anyhow::anyhow!(
                    "kv unit '{}' declared no global slot",
                    self.descriptor.name
                ))
            }
        });
        futures::future::ready(result).boxed()
    }

    fn close(&self) -> BoxFuture<'static, Result<(), StorageError>> {
        if !self.closed.swap(true, Ordering::AcqRel) {
            release_unit(&self.backend, &self.descriptor.name);
        }
        futures::future::ready(Ok(())).boxed()
    }
}
