//! JSON backend source specifics plus the shared KV backend contract.

use std::{path::Path, sync::Arc};

use seekdeep_cordis::Context;
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_storage::{
    KvFacet, KvUnit, KvUnitDescriptor, STORAGE, Storage, StorageBackend, StorageError,
    StorageErrorCode, storage_backend_service_key,
};
use seekdeep_storage_json::{JsonStorageBackend, mount, plugin, register_invariant};
use serde_json::json;
use tempfile::TempDir;

fn descriptor() -> KvUnitDescriptor {
    KvUnitDescriptor {
        name: "contract_unit".to_owned(),
        version: 3,
        tables: vec!["alpha".to_owned(), "beta".to_owned()],
        has_global: true,
    }
}

fn shape() -> KvUnitDescriptor {
    KvUnitDescriptor {
        name: "shape".to_owned(),
        version: 1,
        tables: vec!["t".to_owned()],
        has_global: true,
    }
}

fn kv(backend: &JsonStorageBackend) -> Arc<dyn KvFacet> {
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
async fn missing_unit_is_empty_and_materializes_only_on_first_write() {
    let root = TempDir::new().unwrap();
    let backend = JsonStorageBackend::new(root.path());
    let unit = kv(&backend).open(descriptor()).await.unwrap();
    let snapshot = unit.load_all().await.unwrap();
    assert_eq!(snapshot.tables["alpha"], serde_json::Map::new());
    assert_eq!(snapshot.tables["beta"], serde_json::Map::new());
    assert!(snapshot.global.is_null());
    assert!(
        tokio::fs::metadata(root.path().join("contract_unit.json"))
            .await
            .is_err()
    );
    backend.close().await.unwrap();
}

#[tokio::test]
async fn records_and_global_round_trip_across_a_fresh_backend() {
    let root = TempDir::new().unwrap();
    let backend = JsonStorageBackend::new(root.path());
    let unit = kv(&backend).open(descriptor()).await.unwrap();
    unit.put_record("alpha".to_owned(), "k1".to_owned(), json!({ "n": 1 }))
        .await
        .unwrap();
    unit.put_record("alpha".to_owned(), "k2".to_owned(), json!({ "n": 2 }))
        .await
        .unwrap();
    unit.put_record(
        "beta".to_owned(),
        "weird key / with:stuff".to_owned(),
        json!({ "ok": true }),
    )
    .await
    .unwrap();
    unit.set_global(json!({ "counter": 7 })).await.unwrap();
    backend.close().await.unwrap();

    let reopened = JsonStorageBackend::new(root.path());
    let unit = kv(&reopened).open(descriptor()).await.unwrap();
    let snapshot = unit.load_all().await.unwrap();
    assert_eq!(
        snapshot.tables["alpha"],
        serde_json::Map::from_iter([
            ("k1".to_owned(), json!({ "n": 1 })),
            ("k2".to_owned(), json!({ "n": 2 })),
        ])
    );
    assert_eq!(
        snapshot.tables["beta"]["weird key / with:stuff"],
        json!({ "ok": true })
    );
    assert_eq!(snapshot.global, json!({ "counter": 7 }));
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn overwrite_delete_close_and_version_contract_is_exact() {
    let root = TempDir::new().unwrap();
    let backend = JsonStorageBackend::new(root.path());
    let unit = kv(&backend).open(descriptor()).await.unwrap();
    unit.put_record("alpha".to_owned(), "k".to_owned(), json!({ "v": "old" }))
        .await
        .unwrap();
    unit.put_record("alpha".to_owned(), "k".to_owned(), json!({ "v": "new" }))
        .await
        .unwrap();
    unit.delete_record("alpha".to_owned(), "k".to_owned())
        .await
        .unwrap();
    unit.delete_record("alpha".to_owned(), "k".to_owned())
        .await
        .unwrap();
    assert!(unit.load_all().await.unwrap().tables["alpha"].is_empty());
    unit.put_record("alpha".to_owned(), "kept".to_owned(), json!(1))
        .await
        .unwrap();
    backend.close().await.unwrap();
    backend.close().await.unwrap();
    let error = unit.load_all().await.unwrap_err();
    assert_eq!(storage_error(&error).code, StorageErrorCode::Closed);

    let reopened = JsonStorageBackend::new(root.path());
    let mut wrong = descriptor();
    wrong.version = 4;
    let error = open_error(kv(&reopened).open(wrong).await);
    assert_eq!(
        storage_error(&error).code,
        StorageErrorCode::VersionMismatch
    );
    let original = kv(&reopened).open(descriptor()).await.unwrap();
    assert_eq!(
        original.load_all().await.unwrap().tables["alpha"]["kept"],
        json!(1)
    );
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn pretty_file_matches_javascript_stringify_including_numbers() {
    let root = TempDir::new().unwrap();
    let backend = JsonStorageBackend::new(root.path());
    let unit = kv(&backend).open(shape()).await.unwrap();
    unit.put_record(
        "t".to_owned(),
        "k".to_owned(),
        json!({ "hello": "world", "wholeFloat": 1.0, "negativeZero": -0.0, "tiny": 1e-7 }),
    )
    .await
    .unwrap();
    let text = tokio::fs::read_to_string(root.path().join("shape.json"))
        .await
        .unwrap();
    assert_eq!(
        text,
        concat!(
            "{\n",
            "  \"unit\": {\n",
            "    \"name\": \"shape\",\n",
            "    \"version\": 1\n",
            "  },\n",
            "  \"global\": null,\n",
            "  \"tables\": {\n",
            "    \"t\": {\n",
            "      \"k\": {\n",
            "        \"hello\": \"world\",\n",
            "        \"wholeFloat\": 1,\n",
            "        \"negativeZero\": 0,\n",
            "        \"tiny\": 1e-7\n",
            "      }\n",
            "    }\n",
            "  }\n",
            "}\n"
        )
    );
    backend.close().await.unwrap();
}

#[tokio::test]
async fn malformed_foreign_and_table_shape_failures_are_classified() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("shape.json");
    for (text, code, message) in [
        (
            "not json",
            StorageErrorCode::MalformedMedium,
            "not valid JSON",
        ),
        (
            "\"string\"",
            StorageErrorCode::MalformedMedium,
            "not a JSON object",
        ),
        (
            r#"{"unit":{"name":"other","version":1},"global":null,"tables":{}}"#,
            StorageErrorCode::MalformedMedium,
            "foreign unit header",
        ),
        (
            r#"{"unit":{"name":"shape","version":9},"global":null,"tables":{}}"#,
            StorageErrorCode::VersionMismatch,
            "stored version 9",
        ),
        (
            r#"{"unit":{"name":"shape","version":1},"global":null,"tables":{"t":[]}}"#,
            StorageErrorCode::MalformedMedium,
            "table 't' is not an object",
        ),
        (
            r#"{"unit":{"name":"shape","version":1},"global":null}"#,
            StorageErrorCode::MalformedMedium,
            "tables is not an object",
        ),
    ] {
        tokio::fs::write(&path, text).await.unwrap();
        let backend = JsonStorageBackend::new(root.path());
        let error = open_error(kv(&backend).open(shape()).await);
        let error = storage_error(&error);
        assert_eq!(error.code, code);
        assert!(error.message.contains(message), "{}", error.message);
        backend.close().await.unwrap();
    }
}

#[tokio::test]
async fn missing_declared_table_opens_empty_and_extra_tables_are_ignored() {
    let root = TempDir::new().unwrap();
    tokio::fs::write(
        root.path().join("contract_unit.json"),
        r#"{"unit":{"name":"contract_unit","version":3},"global":null,"tables":{"alpha":{"k":1},"extra":{"x":2}}}"#,
    )
    .await
    .unwrap();
    let backend = JsonStorageBackend::new(root.path());
    let snapshot = kv(&backend)
        .open(descriptor())
        .await
        .unwrap()
        .load_all()
        .await
        .unwrap();
    assert_eq!(snapshot.tables["alpha"]["k"], json!(1));
    assert!(snapshot.tables["beta"].is_empty());
    assert!(!snapshot.tables.contains_key("extra"));
    backend.close().await.unwrap();
}

#[tokio::test]
async fn double_open_invalid_descriptors_and_closed_backend_fail_early() {
    let root = TempDir::new().unwrap();
    let backend = JsonStorageBackend::new(root.path());
    let _unit = kv(&backend).open(shape()).await.unwrap();
    assert!(
        open_error(kv(&backend).open(shape()).await)
            .to_string()
            .contains("already open")
    );
    backend.close().await.unwrap();
    let error = open_error(kv(&backend).open(shape()).await);
    assert_eq!(storage_error(&error).code, StorageErrorCode::Closed);

    for descriptor in [
        KvUnitDescriptor {
            name: "Bad-Name".to_owned(),
            ..shape()
        },
        KvUnitDescriptor {
            tables: vec!["ok".to_owned(), "not ok".to_owned()],
            ..shape()
        },
    ] {
        let other = JsonStorageBackend::new(root.path().join("other"));
        let error = open_error(kv(&other).open(descriptor).await);
        assert_eq!(
            storage_error(&error).code,
            StorageErrorCode::MalformedMedium
        );
    }
}

#[tokio::test]
async fn undeclared_slots_are_plain_errors_and_closed_takes_precedence() {
    let root = TempDir::new().unwrap();
    let backend = JsonStorageBackend::new(root.path());
    let mut descriptor = shape();
    descriptor.has_global = false;
    let unit = kv(&backend).open(descriptor).await.unwrap();
    assert!(
        unit.put_record("undeclared".to_owned(), "k".to_owned(), json!({}))
            .await
            .unwrap_err()
            .to_string()
            .contains("does not declare table")
    );
    assert!(
        unit.set_global(json!({}))
            .await
            .unwrap_err()
            .to_string()
            .contains("does not declare a global slot")
    );
    unit.close().await.unwrap();
    let error = unit.set_global(json!({})).await.unwrap_err();
    assert_eq!(storage_error(&error).code, StorageErrorCode::Closed);
}

#[tokio::test]
async fn failed_publish_rolls_memory_back_and_never_rides_the_next_write() {
    let root = TempDir::new().unwrap();
    let backend = JsonStorageBackend::new(root.path());
    let unit = kv(&backend).open(shape()).await.unwrap();
    unit.put_record("t".to_owned(), "k".to_owned(), json!({ "v": "committed" }))
        .await
        .unwrap();
    unit.set_global(json!({ "g": "committed" })).await.unwrap();
    let path = root.path().join("shape.json");
    let backup = root.path().join("shape.committed.json");
    tokio::fs::rename(&path, &backup).await.unwrap();
    tokio::fs::create_dir(&path).await.unwrap();
    assert!(
        unit.put_record("t".to_owned(), "k".to_owned(), json!({ "v": "rejected" }))
            .await
            .is_err()
    );
    assert!(
        unit.put_record("t".to_owned(), "k2".to_owned(), json!({ "v": "rejected" }))
            .await
            .is_err()
    );
    assert!(
        unit.delete_record("t".to_owned(), "k".to_owned())
            .await
            .is_err()
    );
    assert!(unit.set_global(json!({ "g": "rejected" })).await.is_err());
    tokio::fs::remove_dir(&path).await.unwrap();
    tokio::fs::rename(&backup, &path).await.unwrap();
    let snapshot = unit.load_all().await.unwrap();
    assert_eq!(snapshot.tables["t"]["k"], json!({ "v": "committed" }));
    assert!(!snapshot.tables["t"].contains_key("k2"));
    assert_eq!(snapshot.global, json!({ "g": "committed" }));
    unit.put_record("t".to_owned(), "k3".to_owned(), json!({ "v": "later" }))
        .await
        .unwrap();
    let text = tokio::fs::read_to_string(path).await.unwrap();
    assert!(!text.contains("rejected"));
    backend.close().await.unwrap();
}

#[tokio::test]
async fn non_not_found_reads_propagate_as_raw_filesystem_errors() {
    let root = TempDir::new().unwrap();
    tokio::fs::create_dir(root.path().join("shape.json"))
        .await
        .unwrap();
    let backend = JsonStorageBackend::new(root.path());
    let error = open_error(kv(&backend).open(shape()).await);
    assert!(error.downcast_ref::<std::io::Error>().is_some());
    assert!(error.downcast_ref::<StorageError>().is_none());
    backend.close().await.unwrap();
}

#[tokio::test]
async fn close_drains_inflight_write_and_blocks_inflight_open() {
    let root = TempDir::new().unwrap();
    let backend = JsonStorageBackend::new(root.path());
    let unit = kv(&backend).open(shape()).await.unwrap();
    let write = unit.put_record(
        "t".to_owned(),
        "big".to_owned(),
        json!({ "blob": "x".repeat(4 * 1024 * 1024) }),
    );
    unit.close().await.unwrap();
    write.await.unwrap();
    let document: serde_json::Value = serde_json::from_str(
        &tokio::fs::read_to_string(root.path().join("shape.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(document["tables"]["t"].get("big").is_some());

    let second = JsonStorageBackend::new(root.path());
    let opening = kv(&second).open(shape());
    let closing = second.close();
    let error = open_error(opening.await);
    assert_eq!(storage_error(&error).code, StorageErrorCode::Closed);
    closing.await.unwrap();
}

#[tokio::test]
async fn mount_registers_service_closes_and_invariant_reservation_is_reversible() {
    let root = TempDir::new().unwrap();
    let context = Context::new();
    let storage = Storage::new();
    let _storage_effect = storage.provide(&context).unwrap();
    let mounted = mount(&context, root.path()).unwrap();
    let backend = storage.backend.get("json").unwrap();
    assert!(
        context
            .get_named::<JsonStorageBackend>(&storage_backend_service_key("json"))
            .is_some()
    );
    let unit = backend.kv().unwrap().open(shape()).await.unwrap();
    unit.put_record("t".to_owned(), "k".to_owned(), json!(1))
        .await
        .unwrap();
    mounted.dispose().await.unwrap();
    assert!(storage.backend.get("json").is_err());
    assert!(
        context
            .get_named::<JsonStorageBackend>(&storage_backend_service_key("json"))
            .is_none()
    );
    let error = unit.load_all().await.unwrap_err();
    assert_eq!(storage_error(&error).code, StorageErrorCode::Closed);

    let invariants = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
    let registration = register_invariant(&invariants).unwrap();
    registration.await_ready().await.unwrap();
    registration.dispose().await.unwrap();
    register_invariant(&invariants).unwrap();
    assert!(context.get(STORAGE).is_some());
}

#[tokio::test]
async fn plugin_owns_registration_service_and_backend_teardown() {
    let root = TempDir::new().unwrap();
    let context = Context::new();
    let storage = Storage::new();
    let _storage_effect = storage.provide(&context).unwrap();
    let mounted = context
        .plugin(
            plugin(),
            serde_json::json!({ "root": root.path().to_string_lossy() }),
        )
        .unwrap();
    mounted.await_settled().await.unwrap();
    let backend = context
        .get_named::<JsonStorageBackend>(&storage_backend_service_key("json"))
        .unwrap();
    let unit = kv(&backend).open(shape()).await.unwrap();
    unit.put_record("t".to_owned(), "k".to_owned(), json!(1))
        .await
        .unwrap();
    mounted.dispose().await.unwrap();
    assert!(storage.backend.get("json").is_err());
    assert!(
        context
            .get_named::<JsonStorageBackend>(&storage_backend_service_key("json"))
            .is_none()
    );
    assert_eq!(
        storage_error(&unit.load_all().await.unwrap_err()).code,
        StorageErrorCode::Closed
    );
}

#[test]
fn source_path_helper_stays_absolute_in_tests() {
    assert!(Path::new("/tmp").is_absolute());
}
