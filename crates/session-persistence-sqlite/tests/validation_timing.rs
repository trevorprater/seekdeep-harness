//! Source-derived append/read phase and lossless storage contract for both backends.

use std::{path::Path, sync::Arc};

use seekdeep_cordis::{Context, PluginFiber};
use seekdeep_core::{
    session::{SessionEvent, SessionHeader, SessionId},
    session_store::SessionStore,
};
use seekdeep_session_persistence::{
    SESSION_PERSISTENCE, SessionFormatUnsupportedError, SessionPersistence,
    SessionPersistenceCorruptionError,
};
use seekdeep_session_persistence_jsonl::{JsonlCompression, JsonlConfig};
use seekdeep_session_persistence_sqlite::SqliteConfig;
use serde_json::{Value, json};

async fn mount(
    root: &Path,
    mode: &str,
) -> (Context, Arc<PluginFiber>, Arc<dyn SessionPersistence>) {
    let context = Context::new();
    SessionStore::install(&context).unwrap();
    let fiber = if mode == "sqlite" {
        seekdeep_session_persistence_sqlite::install(
            &context,
            SqliteConfig::new(root.join("sessions.db")),
        )
        .unwrap()
    } else {
        let mut config = JsonlConfig::new(root);
        config.compression = if mode == "jsonl-none" {
            JsonlCompression::None
        } else {
            JsonlCompression::Zstd
        };
        seekdeep_session_persistence_jsonl::install(&context, config).unwrap()
    };
    fiber.await_settled().await.unwrap();
    let persistence = context.get(SESSION_PERSISTENCE).unwrap().persistence();
    (context, fiber, persistence)
}

fn sqlite_raw(root: &Path, id: &SessionId) -> Value {
    let connection = rusqlite::Connection::open_with_flags(
        root.join("sessions.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut query = connection.prepare("SELECT type, seq, time, data, source_event_seqs, surface_op, ignorable FROM events WHERE session_id = ? ORDER BY seq").unwrap();
    let rows = query.query_map([id.as_str()], |row| {
        let mut event = json!({"type":row.get::<_,String>(0)?,"seq":row.get::<_,i64>(1)?,"time":row.get::<_,i64>(2)?,"data":serde_json::from_str::<Value>(&row.get::<_,String>(3)?).unwrap()});
        for (column, key) in [(4, "sourceEventSeqs"), (5, "surfaceOp")] {
            if let Some(value) = row.get::<_,Option<String>>(column)? { event[key] = serde_json::from_str(&value).unwrap(); }
        }
        if let Some(value) = row.get::<_,Option<i64>>(6)? { event["ignorable"] = json!(value != 0); }
        Ok(event)
    }).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
    json!(rows)
}

#[tokio::test]
async fn append_and_cold_read_match_the_source_matrix() {
    let expected: Vec<Value> = serde_json::from_str(include_str!(
        "../../session-persistence/tests/fixtures/validation-timing.expected.json"
    ))
    .unwrap();
    run_contract(&expected).await;
}

#[tokio::test]
#[ignore = "requires the pinned source checkout and its Node dependencies"]
async fn append_and_cold_read_match_a_fresh_source_run() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let source = std::env::var_os("SEEKDEEP_PARITY_SOURCE").map_or_else(
        || std::path::PathBuf::from("/Users/trevor/ws/deepseek-harness"),
        Into::into,
    );
    let snapshot = std::fs::read_to_string(repository.join("SOURCE_SNAPSHOT")).unwrap();
    let pinned = snapshot
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .unwrap();
    let head = std::process::Command::new("git")
        .arg("-C")
        .arg(&source)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(head.status.success());
    assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), pinned);
    let temporary = tempfile::tempdir().unwrap();
    let probe_path = temporary.path().join("oracle.spec.ts");
    let result_path = temporary.path().join("source.json");
    let config_path = temporary.path().join("vitest.config.mts");
    let quoted =
        |path: &Path| serde_json::to_string(&path.to_string_lossy().replace('\\', "/")).unwrap();
    let mut probe = include_str!("support/validation_timing_oracle.ts.in").to_owned();
    for (marker, relative) in [
        ("__VITEST__", "node_modules/vitest/dist/index.js"),
        ("__CORDIS__", "vendor/cordis/src/index.ts"),
        ("__SESSION__", "packages/core/session/src/index.ts"),
        (
            "__JSONL__",
            "packages/session/session-persistence-jsonl/src/index.ts",
        ),
        (
            "__SQLITE__",
            "packages/session/session-persistence-sqlite/src/index.ts",
        ),
    ] {
        probe = probe.replace(marker, &quoted(&source.join(relative)));
    }
    probe = probe
        .replace(
            "__CASES__",
            &quoted(
                &repository
                    .join("crates/session-persistence/tests/fixtures/validation-timing.json"),
            ),
        )
        .replace("__OUTPUT__", &quoted(&result_path));
    std::fs::write(&probe_path, probe).unwrap();
    std::fs::write(&config_path, format!("import source from {};\nexport default {{...source,root:{},test:{{...source.test,include:[{}],coverage:{{enabled:false}}}}}};\n", quoted(&source.join("vitest.web.config.ts")), quoted(&source), quoted(&probe_path))).unwrap();
    let output = tokio::process::Command::new("node")
        .arg(source.join("node_modules/vitest/vitest.mjs"))
        .args(["run", "--config"])
        .arg(&config_path)
        .current_dir(&source)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let source_json = std::fs::read_to_string(result_path).unwrap();
    if let Some(path) = std::env::var_os("SEEKDEEP_VALIDATION_SOURCE_OUTPUT") {
        std::fs::write(path, &source_json).unwrap();
    }
    let expected: Vec<Value> = serde_json::from_str(&source_json).unwrap();
    run_contract(&expected).await;
}

async fn run_contract(expected: &[Value]) {
    let cases: Vec<Value> = serde_json::from_str(include_str!(
        "../../session-persistence/tests/fixtures/validation-timing.json"
    ))
    .unwrap();
    let mut actual = Vec::new();
    let mut mismatches = Vec::new();
    for mode in ["jsonl-none", "jsonl-zstd", "sqlite"] {
        for case in &cases {
            let name = case["name"].as_str().unwrap();
            let temporary = tempfile::tempdir().unwrap();
            let (context, fiber, writer) = mount(temporary.path(), mode).await;
            let id = SessionId::new(name);
            let mut header = SessionHeader::new(id.clone());
            header.created_at = 1000;
            let events: Vec<SessionEvent> = serde_json::from_value(case["events"].clone()).unwrap();
            writer.create(&header).await.unwrap();
            let append = writer.append(&id, &events).await;
            let mut result = json!({"mode":mode,"name":name,"append":append.is_ok(),"read":false,"load":false,"rawEvents":null,"readEvents":null,"loadEvents":null,"appendError":append.err().map(|error|error.to_string()),"readError":null,"readKind":null,"loadError":null,"loadKind":null});
            if result["append"] == true && !events.is_empty() {
                result["rawEvents"] = if mode == "sqlite" {
                    sqlite_raw(temporary.path(), &id)
                } else {
                    let raw = writer.read_raw(&id, None).await.unwrap().unwrap();
                    json!(
                        raw.content
                            .lines()
                            .skip(1)
                            .map(|line| serde_json::from_str::<Value>(line).unwrap())
                            .collect::<Vec<_>>()
                    )
                };
            }
            fiber.dispose().await.unwrap();
            context.fiber().dispose().await.unwrap();
            drop(writer);
            let (context, fiber, reader) = mount(temporary.path(), mode).await;
            if result["append"] == true && !events.is_empty() {
                match reader.inspect(&id, None).await {
                    Ok(snapshot) => {
                        result["read"] = json!(true);
                        result["readEvents"] = serde_json::to_value(snapshot.events).unwrap();
                    }
                    Err(error) => {
                        result["readKind"] =
                            json!(if error.is::<SessionPersistenceCorruptionError>() {
                                "corruption"
                            } else if error.is::<SessionFormatUnsupportedError>() {
                                "unsupported"
                            } else {
                                "other"
                            });
                        result["readError"] = json!(error.to_string());
                    }
                }
                match reader.load(&id).await {
                    Ok(snapshot) => {
                        result["load"] = json!(true);
                        result["loadEvents"] = serde_json::to_value(snapshot.events).unwrap();
                    }
                    Err(error) => {
                        result["loadKind"] =
                            json!(if error.is::<SessionPersistenceCorruptionError>() {
                                "corruption"
                            } else if error.is::<SessionFormatUnsupportedError>() {
                                "unsupported"
                            } else {
                                "other"
                            });
                        result["loadError"] = json!(error.to_string());
                    }
                }
            }
            fiber.dispose().await.unwrap();
            context.fiber().dispose().await.unwrap();
            drop(reader);
            let source = expected
                .iter()
                .find(|row| (row["mode"].is_null() || row["mode"] == mode) && row["name"] == name)
                .unwrap();
            compare_result(mode, name, &result, source, &mut mismatches);
            actual.push(result);
        }
    }
    if let Some(path) = std::env::var_os("SEEKDEEP_VALIDATION_ACTUAL") {
        std::fs::write(path, serde_json::to_vec_pretty(&actual).unwrap()).unwrap();
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

fn compare_result(
    mode: &str,
    name: &str,
    result: &Value,
    source: &Value,
    mismatches: &mut Vec<String>,
) {
    for field in [
        "append",
        "read",
        "rawEvents",
        "readEvents",
        "load",
        "loadEvents",
    ] {
        if result[field] != source[field] {
            mismatches.push(format!(
                "{mode}/{name}/{field}: actual {}, source {}",
                result[field], source[field]
            ));
        }
    }
    let append_error = source["appendError"].as_str().map(|error| {
        error
            .strip_prefix("Error: ")
            .or_else(|| error.strip_prefix("TypeError: "))
            .unwrap_or(error)
    });
    if result["appendError"].as_str() != append_error {
        mismatches.push(format!(
            "{mode}/{name}/appendError: actual {}, source {append_error:?}",
            result["appendError"]
        ));
    }
    let raw_error = source["readError"].as_str();
    let read_kind = source["readKind"].as_str().or_else(|| {
        raw_error.map(|error| {
            if error.starts_with("SessionPersistenceCorruptionError:") {
                "corruption"
            } else if error.starts_with("SessionFormatUnsupportedError:") {
                "unsupported"
            } else {
                "other"
            }
        })
    });
    if result["readKind"].as_str() != read_kind {
        mismatches.push(format!(
            "{mode}/{name}/readKind: actual {}, source {read_kind:?}",
            result["readKind"]
        ));
    }
    let source_error = raw_error.map(|error| {
        error
            .strip_prefix("SessionPersistenceCorruptionError: ")
            .or_else(|| error.strip_prefix("SessionFormatUnsupportedError: "))
            .unwrap_or(error)
            .split(" (raw log:")
            .next()
            .unwrap()
    });
    let actual_error = result["readError"]
        .as_str()
        .map(|error| error.split(" (raw log:").next().unwrap());
    if actual_error != source_error {
        mismatches.push(format!(
            "{mode}/{name}/readError: actual {actual_error:?}, source {source_error:?}"
        ));
    }
    let raw_error = source["loadError"].as_str();
    let kind = source["loadKind"].as_str().or_else(|| {
        raw_error.map(|error| {
            if error.starts_with("SessionPersistenceCorruptionError:") {
                "corruption"
            } else if error.starts_with("SessionFormatUnsupportedError:") {
                "unsupported"
            } else {
                "other"
            }
        })
    });
    if result["loadKind"].as_str() != kind {
        mismatches.push(format!(
            "{mode}/{name}/loadKind: actual {}, source {kind:?}",
            result["loadKind"]
        ));
    }
    let source_error = raw_error.map(|error| {
        error
            .strip_prefix("SessionPersistenceCorruptionError: ")
            .or_else(|| error.strip_prefix("SessionFormatUnsupportedError: "))
            .unwrap_or(error)
            .split(" (raw log:")
            .next()
            .unwrap()
    });
    let actual_error = result["loadError"]
        .as_str()
        .map(|error| error.split(" (raw log:").next().unwrap());
    if actual_error != source_error {
        mismatches.push(format!(
            "{mode}/{name}/loadError: actual {actual_error:?}, source {source_error:?}"
        ));
    }
}
