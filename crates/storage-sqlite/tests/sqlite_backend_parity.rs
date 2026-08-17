//! `SQLite` backend source specifics plus the shared KV contract.

use std::{path::Path, sync::Arc};

use rusqlite::{Connection, OptionalExtension as _};
use seekdeep_cordis::Context;
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_storage::{
    KvFacet, KvUnit, KvUnitDescriptor, STORAGE, Storage, StorageBackend, StorageError,
    StorageErrorCode, storage_backend_service_key,
};
use seekdeep_storage_sqlite::{
    JournalMode, STORAGE_SQLITE_SCHEMA_VERSION, SqliteStorageBackend, SqliteStorageConfig, plugin,
    register_invariant,
};
use serde_json::json;
use tempfile::TempDir;

fn config(path: impl AsRef<Path>) -> SqliteStorageConfig {
    SqliteStorageConfig {
        path: path.as_ref().to_string_lossy().into_owned(),
        journal_mode: JournalMode::Wal,
    }
}

fn memory_config() -> SqliteStorageConfig {
    SqliteStorageConfig {
        path: ":memory:".to_owned(),
        journal_mode: JournalMode::Wal,
    }
}

fn descriptor() -> KvUnitDescriptor {
    KvUnitDescriptor {
        name: "specimen".to_owned(),
        version: 1,
        tables: vec!["records".to_owned(), "other".to_owned()],
        has_global: true,
    }
}

fn kv(backend: &SqliteStorageBackend) -> Arc<dyn KvFacet> {
    backend.kv().unwrap()
}

fn storage_error(error: &anyhow::Error) -> &StorageError {
    error.downcast_ref().expect("typed StorageError")
}

fn open_error(result: anyhow::Result<Arc<dyn KvUnit>>) -> anyhow::Error {
    match result {
        Ok(_) => panic!("unit open unexpectedly succeeded"),
        Err(error) => error,
    }
}

#[tokio::test]
async fn memory_and_file_media_obey_the_full_round_trip_contract() {
    let memory = SqliteStorageBackend::new(memory_config());
    let unit = kv(&memory).open(descriptor()).await.unwrap();
    unit.put_record("records".to_owned(), "k".to_owned(), json!({ "n": 1 }))
        .await
        .unwrap();
    assert_eq!(
        unit.load_all().await.unwrap().tables["records"]["k"],
        json!({ "n": 1 })
    );
    memory.close().await.unwrap();

    let root = TempDir::new().unwrap();
    let path = root.path().join("storage.db");
    let backend = SqliteStorageBackend::new(config(&path));
    let unit = kv(&backend).open(descriptor()).await.unwrap();
    let empty = unit.load_all().await.unwrap();
    assert!(empty.tables["records"].is_empty());
    assert!(empty.tables["other"].is_empty());
    assert!(empty.global.is_null());
    unit.put_record("records".to_owned(), "k".to_owned(), json!({ "v": "old" }))
        .await
        .unwrap();
    unit.put_record("records".to_owned(), "k".to_owned(), json!({ "v": "new" }))
        .await
        .unwrap();
    unit.put_record(
        "other".to_owned(),
        "weird key / with:stuff".to_owned(),
        json!([1, 2, 3]),
    )
    .await
    .unwrap();
    unit.set_global(json!({ "counter": 7 })).await.unwrap();
    unit.delete_record("records".to_owned(), "absent".to_owned())
        .await
        .unwrap();
    backend.close().await.unwrap();
    backend.close().await.unwrap();

    let reopened = SqliteStorageBackend::new(config(&path));
    let unit = kv(&reopened).open(descriptor()).await.unwrap();
    let snapshot = unit.load_all().await.unwrap();
    assert_eq!(snapshot.tables["records"]["k"], json!({ "v": "new" }));
    assert_eq!(
        snapshot.tables["other"]["weird key / with:stuff"],
        json!([1, 2, 3])
    );
    assert_eq!(snapshot.global, json!({ "counter": 7 }));
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn schema_is_strict_stamped_and_unit_versions_are_independent() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("storage.db");
    let backend = SqliteStorageBackend::new(config(&path));
    kv(&backend).open(descriptor()).await.unwrap();
    backend.close().await.unwrap();

    let database = Connection::open(&path).unwrap();
    let schema_version: u32 = database
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(schema_version, STORAGE_SQLITE_SCHEMA_VERSION);
    let sql: String = database
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'u_specimen_records'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(sql.contains("STRICT"));
    let unit_version: i64 = database
        .query_row(
            "SELECT version FROM units WHERE name = 'specimen'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(unit_version, 1);
    drop(database);

    let backend = SqliteStorageBackend::new(config(&path));
    let mut wrong = descriptor();
    wrong.version = 2;
    let error = open_error(kv(&backend).open(wrong).await);
    assert_eq!(
        storage_error(&error).code,
        StorageErrorCode::VersionMismatch
    );
    backend.close().await.unwrap();
}

#[tokio::test]
async fn incompatible_schema_rejects_and_failed_materialization_stays_unstamped() {
    let root = TempDir::new().unwrap();
    let wrong_path = root.path().join("wrong.db");
    let wrong = Connection::open(&wrong_path).unwrap();
    wrong.pragma_update(None, "user_version", 999).unwrap();
    drop(wrong);
    let backend = SqliteStorageBackend::new(config(&wrong_path));
    let error = open_error(kv(&backend).open(descriptor()).await);
    assert_eq!(
        storage_error(&error).code,
        StorageErrorCode::VersionMismatch
    );
    backend.close().await.unwrap();

    let obstructed_path = root.path().join("obstructed.db");
    let obstructed = Connection::open(&obstructed_path).unwrap();
    obstructed
        .execute_batch("CREATE TABLE squatter (x TEXT); CREATE INDEX unit_globals ON squatter(x);")
        .unwrap();
    drop(obstructed);
    let backend = SqliteStorageBackend::new(config(&obstructed_path));
    assert!(kv(&backend).open(descriptor()).await.is_err());
    backend.close().await.unwrap();
    let repair = Connection::open(&obstructed_path).unwrap();
    let version: u32 = repair
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 0);
    repair.execute_batch("DROP INDEX unit_globals").unwrap();
    drop(repair);
    let backend = SqliteStorageBackend::new(config(&obstructed_path));
    kv(&backend).open(descriptor()).await.unwrap();
    backend.close().await.unwrap();
}

#[tokio::test]
async fn validates_names_reservations_reopen_and_closed_precedence() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("never-created.db");
    for invalid in [
        KvUnitDescriptor {
            name: "Bad-Name".to_owned(),
            ..descriptor()
        },
        KvUnitDescriptor {
            tables: vec!["ok".to_owned(), "1bad".to_owned()],
            ..descriptor()
        },
    ] {
        let backend = SqliteStorageBackend::new(config(&path));
        assert!(
            open_error(kv(&backend).open(invalid).await)
                .to_string()
                .contains("violates")
        );
        backend.close().await.unwrap();
    }

    let backend = SqliteStorageBackend::new(memory_config());
    let first = kv(&backend).open(descriptor()).await.unwrap();
    assert!(
        open_error(kv(&backend).open(descriptor()).await)
            .to_string()
            .contains("already open")
    );
    first.close().await.unwrap();
    let second = kv(&backend).open(descriptor()).await.unwrap();
    second.close().await.unwrap();
    backend.close().await.unwrap();
    let error = open_error(kv(&backend).open(descriptor()).await);
    assert_eq!(storage_error(&error).code, StorageErrorCode::Closed);
    let error = second
        .put_record("undeclared".to_owned(), "k".to_owned(), json!(1))
        .await
        .unwrap_err();
    assert_eq!(storage_error(&error).code, StorageErrorCode::Closed);
}

#[tokio::test]
async fn arbitrary_record_keys_and_undeclared_slots_are_exact() {
    let backend = SqliteStorageBackend::new(memory_config());
    let unit = kv(&backend).open(descriptor()).await.unwrap();
    unit.put_record(
        "records".to_owned(),
        "__proto__".to_owned(),
        json!({ "evil": true }),
    )
    .await
    .unwrap();
    unit.put_record("records".to_owned(), "constructor".to_owned(), json!(1))
        .await
        .unwrap();
    let records = unit.load_all().await.unwrap().tables["records"].clone();
    assert_eq!(records["__proto__"], json!({ "evil": true }));
    assert_eq!(records["constructor"], json!(1));
    assert!(
        unit.put_record("undeclared".to_owned(), "k".to_owned(), json!(1))
            .await
            .unwrap_err()
            .to_string()
            .contains("declared no table")
    );
    backend.close().await.unwrap();

    let backend = SqliteStorageBackend::new(memory_config());
    let mut no_global = descriptor();
    no_global.has_global = false;
    let unit = kv(&backend).open(no_global).await.unwrap();
    assert!(unit.load_all().await.unwrap().global.is_null());
    assert!(
        unit.set_global(json!(1))
            .await
            .unwrap_err()
            .to_string()
            .contains("declared no global slot")
    );
    backend.close().await.unwrap();
}

#[tokio::test]
async fn unparsable_record_and_global_json_are_malformed_medium() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("storage.db");
    let backend = SqliteStorageBackend::new(config(&path));
    let unit = kv(&backend).open(descriptor()).await.unwrap();
    unit.put_record("records".to_owned(), "bad".to_owned(), json!(1))
        .await
        .unwrap();
    unit.set_global(json!(1)).await.unwrap();
    backend.close().await.unwrap();
    let database = Connection::open(&path).unwrap();
    database
        .execute(
            "UPDATE u_specimen_records SET value = '{not json' WHERE key = 'bad'",
            [],
        )
        .unwrap();
    drop(database);
    let backend = SqliteStorageBackend::new(config(&path));
    let unit = kv(&backend).open(descriptor()).await.unwrap();
    let error = unit.load_all().await.unwrap_err();
    assert_eq!(
        storage_error(&error).code,
        StorageErrorCode::MalformedMedium
    );
    backend.close().await.unwrap();

    let database = Connection::open(&path).unwrap();
    database
        .execute(
            "UPDATE u_specimen_records SET value = '1' WHERE key = 'bad'",
            [],
        )
        .unwrap();
    database
        .execute(
            "UPDATE unit_globals SET value = '][' WHERE unit = 'specimen'",
            [],
        )
        .unwrap();
    drop(database);
    let backend = SqliteStorageBackend::new(config(&path));
    let unit = kv(&backend).open(descriptor()).await.unwrap();
    let error = unit.load_all().await.unwrap_err();
    assert_eq!(
        storage_error(&error).code,
        StorageErrorCode::MalformedMedium
    );
    backend.close().await.unwrap();
}

#[tokio::test]
async fn close_drains_a_pending_failed_open() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("storage.db");
    let first = SqliteStorageBackend::new(config(&path));
    kv(&first)
        .open(descriptor())
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    first.close().await.unwrap();

    let backend = SqliteStorageBackend::new(config(&path));
    let mut wrong = descriptor();
    wrong.version = 99;
    let pending = kv(&backend).open(wrong);
    let closing = backend.close();
    let error = open_error(pending.await);
    assert_eq!(
        storage_error(&error).code,
        StorageErrorCode::VersionMismatch
    );
    closing.await.unwrap();
}

#[tokio::test]
async fn plugin_registers_service_and_disposes_backend_in_source_order() {
    let context = Context::new();
    let storage = Storage::new();
    let _storage_effect = storage.provide(&context).unwrap();
    let mounted = context
        .plugin(plugin(), json!({ "path": ":memory:" }))
        .unwrap();
    mounted.await_settled().await.unwrap();
    let backend = context
        .get_named::<SqliteStorageBackend>(&storage_backend_service_key("sqlite"))
        .unwrap();
    assert!(storage.backend.get("sqlite").is_ok());
    let unit = kv(&backend).open(descriptor()).await.unwrap();
    unit.put_record("records".to_owned(), "k".to_owned(), json!(1))
        .await
        .unwrap();
    mounted.dispose().await.unwrap();
    assert!(context.get(STORAGE).is_some());
    assert!(storage.backend.get("sqlite").is_err());
    assert!(
        context
            .get_named::<SqliteStorageBackend>(&storage_backend_service_key("sqlite"))
            .is_none()
    );
    let error = unit.load_all().await.unwrap_err();
    assert_eq!(storage_error(&error).code, StorageErrorCode::Closed);
}

#[tokio::test]
async fn filesystem_modes_invalid_paths_and_invariant_registration_match_contract() {
    let root = TempDir::new().unwrap();
    let nested = root.path().join("private").join("storage.db");
    let backend = SqliteStorageBackend::new(config(&nested));
    kv(&backend).open(descriptor()).await.unwrap();
    backend.close().await.unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        assert_eq!(
            std::fs::metadata(nested.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&nested).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o644)).unwrap();
        let reopened = SqliteStorageBackend::new(config(&nested));
        kv(&reopened).open(descriptor()).await.unwrap();
        reopened.close().await.unwrap();
        assert_eq!(
            std::fs::metadata(&nested).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    let invalid = SqliteStorageBackend::new(SqliteStorageConfig {
        path: format!("{}\0invalid", nested.display()),
        journal_mode: JournalMode::Wal,
    });
    assert!(kv(&invalid).open(descriptor()).await.is_err());
    invalid.close().await.unwrap();

    let context = Context::new();
    let invariants = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
    let registration = register_invariant(&invariants).unwrap();
    registration.await_ready().await.unwrap();
    registration.dispose().await.unwrap();
    register_invariant(&invariants).unwrap();
}

#[test]
fn journal_config_defaults_and_unknown_values_are_rejected() {
    let parsed: SqliteStorageConfig =
        serde_json::from_value(json!({ "path": ":memory:" })).unwrap();
    assert_eq!(parsed.journal_mode, JournalMode::Wal);
    assert!(
        serde_json::from_value::<SqliteStorageConfig>(
            json!({ "path": ":memory:", "journalMode": "off" })
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<SqliteStorageConfig>(json!({
            "path": ":memory:",
            "extra": true
        }))
        .is_err()
    );
    let database = Connection::open_in_memory().unwrap();
    let absent: Option<i64> = database
        .query_row("SELECT 1 WHERE 0", [], |row| row.get(0))
        .optional()
        .unwrap();
    assert!(absent.is_none());
}
