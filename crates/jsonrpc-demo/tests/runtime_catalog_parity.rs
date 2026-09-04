//! Packaged runtime closure selection and real compiled plugin composition.

use std::collections::BTreeMap;

use seekdeep_sdk_jsonrpc_demo::runner::catalog;

#[test]
fn packaged_catalog_resolves_the_runtime_manifests_concrete_plugins() {
    let root = tempfile::tempdir().unwrap();
    let catalog = catalog(root.path(), &BTreeMap::new(), Some(root.path())).unwrap();
    for name in [
        "cordis-plugin-timer",
        "seekdeep-code-runtime-worker-thread",
        "seekdeep-workflow-worker-thread",
        "seekdeep-tool-cordis",
        "seekdeep-tool-workflow",
        "seekdeep-goal",
        "seekdeep-goal-round-driver",
        "seekdeep-command-goal",
        "seekdeep-session-persistence-sqlite",
        "seekdeep-session-query-sqlite",
        "seekdeep-skill",
        "seekdeep-skill-filesystem",
        "seekdeep-tool-skill",
        "seekdeep-web-fetch-http",
        "seekdeep-web-search-exa",
        "seekdeep-web-search-perplexity",
    ] {
        catalog
            .preflight_yaml(&format!("- name: '@seekdeep-ai/{name}'\n"))
            .unwrap_or_else(|error| panic!("packaged manifest plugin {name}: {error}"));
    }
}

#[test]
fn development_replay_is_not_silently_added_to_the_packaged_closure() {
    let root = tempfile::tempdir().unwrap();
    let replay = "- name: '@seekdeep-ai/seekdeep-llm-replay'\n";
    let development = catalog(root.path(), &BTreeMap::new(), None).unwrap();
    development.preflight_yaml(replay).unwrap();
    let packaged = catalog(root.path(), &BTreeMap::new(), Some(root.path())).unwrap();
    assert!(packaged.preflight_yaml(replay).is_err());
}

#[tokio::test]
async fn ambient_node_modules_cannot_expand_the_packaged_plugin_set() {
    let root = tempfile::tempdir().unwrap();
    let package = root.path().join("node_modules/ambient-plugin");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("package.json"),
        r#"{"name":"ambient-plugin","type":"module","main":"index.mjs"}"#,
    )
    .unwrap();
    std::fs::write(
        package.join("index.mjs"),
        "export const name = 'ambient-plugin'; export function apply() {}\n",
    )
    .unwrap();
    let config_path = root.path().join("cordis.yml");
    let source = "- name: ambient-plugin\n";
    let development = catalog(root.path(), &BTreeMap::new(), None).unwrap();
    let context = seekdeep_cordis::Context::new();
    development
        .load_yaml_at(&context, source, &config_path)
        .await
        .unwrap()
        .dispose()
        .await
        .unwrap();
    let packaged = catalog(root.path(), &BTreeMap::new(), Some(root.path())).unwrap();
    let context = seekdeep_cordis::Context::new();
    assert!(
        packaged
            .load_yaml_at(&context, source, &config_path)
            .await
            .is_err()
    );
    let explicit = "- name: './node_modules/ambient-plugin/index.mjs'\n";
    packaged
        .load_yaml_at(&context, explicit, &config_path)
        .await
        .unwrap()
        .dispose()
        .await
        .unwrap();
}

#[tokio::test]
async fn packaged_sqlite_composition_persists_and_reopens_the_exact_session() {
    use seekdeep_core::{
        session::{AppendOptions, SessionId},
        session_store::{CreateSessionOptions, SESSIONS},
    };
    use seekdeep_session_persistence::SESSION_PERSISTENCE;

    let root = tempfile::tempdir().unwrap();
    let catalog = catalog(root.path(), &BTreeMap::new(), Some(root.path())).unwrap();
    let source = serde_json::json!([
        {"name":"@seekdeep-ai/seekdeep-session"},
        {"name":"@seekdeep-ai/seekdeep-session-persistence-sqlite", "config":{"path":root.path().join("sessions.sqlite")}},
        {"name":"@seekdeep-ai/seekdeep-session-query-sqlite", "config":{"path":root.path().join("query.sqlite")}},
        {"name":"@seekdeep-ai/seekdeep-invariants", "config":{"enabled":false}}
    ]).to_string();
    let context = seekdeep_cordis::Context::new();
    let composition = catalog.load_yaml(&context, &source).await.unwrap();
    let sessions = context.get(SESSIONS).unwrap();
    let id = SessionId::new("packaged-sqlite");
    let session = sessions
        .create(&context, Some(id.clone()), CreateSessionOptions::default())
        .unwrap();
    let event = session
        .append(
            "approval/policy",
            serde_json::json!({"policy":"never"}),
            AppendOptions::default(),
        )
        .unwrap();
    assert!(sessions.flush(&session).await.unwrap());
    let stored = context
        .get(SESSION_PERSISTENCE)
        .unwrap()
        .persistence()
        .inspect(&id, None)
        .await
        .unwrap();
    assert_eq!(stored.meta.id, id);
    assert_eq!(stored.events.as_slice(), std::slice::from_ref(&event));
    composition.dispose().await.unwrap();
    assert!(context.get(SESSION_PERSISTENCE).is_none());

    let reopened = seekdeep_cordis::Context::new();
    let composition = catalog.load_yaml(&reopened, &source).await.unwrap();
    let stored = reopened
        .get(SESSION_PERSISTENCE)
        .unwrap()
        .persistence()
        .inspect(&id, None)
        .await
        .unwrap();
    assert_eq!(stored.events, [event]);
    composition.dispose().await.unwrap();
}

#[tokio::test]
async fn relocated_packaged_binary_runs_its_own_workflow_worker_without_node_or_siblings() {
    use std::{process::Stdio, time::Duration};
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

    let root = tempfile::tempdir().unwrap();
    let binary = root.path().join(if cfg!(windows) {
        "runtime.exe"
    } else {
        "runtime"
    });
    std::fs::copy(
        env!("CARGO_BIN_EXE_seekdeep-jsonrpc-agent-packaged"),
        &binary,
    )
    .unwrap();
    let mut child = tokio::process::Command::new(binary)
        .current_dir(root.path())
        .env_clear()
        .env("SEEKDEEP_INTERNAL_WORKFLOW_WORKER", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let init = serde_json::json!({
        "meta": {"name":"packaged-worker", "description":"relocated native worker"},
        "body": "return { answer: 42, processType: typeof process };",
        "limits": {"maxConcurrentAgents":1,"maxTotalAgents":1,"maxItemsPerCall":1,"syncTimeoutMs":5000}
    });
    stdin
        .write_all(format!("{init}\n").as_bytes())
        .await
        .unwrap();
    stdin.flush().await.unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let ready = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ready.as_deref(), Some("{\"type\":\"ready\"}"));
    stdin.write_all(b"{\"type\":\"go\"}\n").await.unwrap();
    stdin.flush().await.unwrap();
    let result = loop {
        let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .expect("terminal worker result");
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        if value["type"] == "result" {
            break value["result"].clone();
        }
    };
    assert_eq!(result["stopReason"], "completed");
    assert_eq!(
        result["value"],
        serde_json::json!({"answer":42,"processType":"undefined"})
    );
    drop(stdin);
    assert!(
        tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}
