//! Real-process initialized LSP instance lifecycle parity coverage.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use seekdeep_llm::AbortSignal;
use seekdeep_lsp::{
    LSP_DISPOSED, LspError, LspOperation, LspPosition, LspProviderQuery, LspQueryResult,
};
use seekdeep_lsp_stdio::{ConnectionWriter, HostSource, InstanceSpec, LspInstance, encode_message};
use seekdeep_subprocess_local::LocalSubprocessRuntime;
use serde_json::{Value, json};
use tempfile::TempDir;
use thiserror::Error;

struct TestInstance {
    instance: LspInstance,
    _runtime: Arc<LocalSubprocessRuntime>,
    _root: TempDir,
    workspace: PathBuf,
    workspace_uri: String,
}

impl TestInstance {
    fn source(&self, text: impl Into<String>) -> HostSource {
        HostSource {
            file_url: url::Url::from_file_path(self.workspace.join("a.ts"))
                .unwrap()
                .to_string(),
            text: text.into(),
        }
    }

    fn request(&self, operation: LspOperation) -> LspProviderQuery {
        LspProviderQuery {
            operation,
            file_path: "a.ts".to_owned(),
            position: LspPosition {
                line: 0.0,
                character: 6.0,
            },
            workspace_root: self.workspace.to_string_lossy().into_owned(),
            language_id: "typescript".to_owned(),
        }
    }

    async fn run(&self, operation: LspOperation) -> anyhow::Result<LspQueryResult> {
        self.instance
            .query(self.request(operation), self.source("const x = 1\n"), None)
            .await
    }

    async fn run_with_signal(
        &self,
        operation: LspOperation,
        signal: &AbortSignal,
    ) -> anyhow::Result<LspQueryResult> {
        self.instance
            .query(
                self.request(operation),
                self.source("const x = 1\n"),
                Some(signal),
            )
            .await
    }

    async fn finish(&self) {
        tokio::time::timeout(Duration::from_secs(5), self.instance.dispose())
            .await
            .expect("instance teardown timed out")
            .unwrap();
    }
}

async fn make_instance(
    env: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
    shutdown_timeout_ms: f64,
    kill_grace_ms: f64,
    writer: Option<ConnectionWriter>,
) -> TestInstance {
    let root = tempfile::tempdir().unwrap();
    let canonical_root = tokio::fs::canonicalize(root.path()).await.unwrap();
    let workspace = canonical_root.join("ws");
    tokio::fs::create_dir(&workspace).await.unwrap();
    tokio::fs::write(workspace.join("a.ts"), "const x = 1\n")
        .await
        .unwrap();
    let workspace_uri = url::Url::from_file_path(&workspace).unwrap().to_string();
    let runtime = Arc::new(LocalSubprocessRuntime::new());
    let spec = InstanceSpec {
        command: env!("CARGO_BIN_EXE_seekdeep-lsp-stdio-fixture").to_owned(),
        args: Vec::new(),
        cwd: workspace.clone(),
        env: env
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect::<BTreeMap<_, _>>(),
        max_message_bytes: 16_000_000,
        max_stderr_bytes: 100_000,
        kill_grace_ms,
        configuration: Some(json!({"setting": 42})),
        workspace_uri: workspace_uri.clone(),
        initialization_options: Some(json!({"init": true})),
        shutdown_timeout_ms,
    };
    let instance = LspInstance::new(spec, runtime.as_ref(), writer).unwrap();
    TestInstance {
        instance,
        _runtime: runtime,
        _root: root,
        workspace,
        workspace_uri,
    }
}

async fn standard_instance(
    env: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
) -> TestInstance {
    make_instance(env, 200.0, 200.0, None).await
}

#[derive(Debug, Error)]
#[error("{0}")]
struct TestAbort(&'static str);

fn abort_with(signal: &AbortSignal, message: &'static str) {
    signal.abort_with_error(Arc::new(TestAbort(message)), json!({"message": message}));
}

fn failing_writer(method: &'static str) -> ConnectionWriter {
    Arc::new(move |stdin, message| {
        Box::pin(async move {
            if message.get("method").and_then(Value::as_str) == Some(method) {
                anyhow::bail!("fixture {method} failure");
            }
            stdin.write_all(&encode_message(&message)?).await?;
            Ok(())
        })
    })
}

#[tokio::test]
async fn server_requests_and_closed_query_results_are_exact() {
    for on_open in ["configuration", "lifecycle", "applyEdit", "unknown"] {
        let instance =
            standard_instance([("LSP_FAKE_ON_OPEN", on_open), ("LSP_FAKE_DEF", "null")]).await;
        assert_eq!(
            instance.run(LspOperation::GoToDefinition).await.unwrap(),
            LspQueryResult::Locations {
                locations: Vec::new(),
                resolved_workspace_uri: instance.workspace_uri.clone(),
            }
        );
        instance.finish().await;
    }

    let location_instance = standard_instance([(
        "LSP_FAKE_REFS".to_owned(),
        json!([{
            "uri": "file:///a.ts",
            "range": {
                "start": {"line": 0, "character": 0},
                "end": {"line": 0, "character": 3},
            }
        }])
        .to_string(),
    )])
    .await;
    let result = location_instance
        .run(LspOperation::FindReferences)
        .await
        .unwrap();
    assert!(matches!(
        result,
        LspQueryResult::Locations { ref locations, .. } if locations.len() == 1
    ));
    location_instance.finish().await;
}

#[tokio::test]
async fn preabort_ignored_cancel_and_acknowledged_cancel_have_source_boundaries() {
    let preaborted = standard_instance([("LSP_FAKE_DEF", "null")]).await;
    let signal = AbortSignal::default();
    abort_with(&signal, "pre-abort");
    let error = preaborted
        .run_with_signal(LspOperation::GoToDefinition, &signal)
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "pre-abort");
    preaborted.finish().await;

    let ignored = make_instance([("LSP_FAKE_HANG", "1")], 100.0, 100.0, None).await;
    let signal = AbortSignal::default();
    let query_instance = ignored.instance.clone();
    let request = ignored.request(LspOperation::GoToDefinition);
    let source = ignored.source("const x = 1\n");
    let query_signal = signal.clone();
    let pending = tokio::spawn(async move {
        query_instance
            .query(request, source, Some(&query_signal))
            .await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    abort_with(&signal, "mid-flight");
    assert_eq!(
        pending.await.unwrap().unwrap_err().to_string(),
        "mid-flight"
    );
    assert!(ignored.instance.dead());
    ignored.finish().await;

    let acknowledged = make_instance(
        [("LSP_FAKE_HANG", "1"), ("LSP_FAKE_CANCEL_ACK", "1")],
        200.0,
        2_000.0,
        None,
    )
    .await;
    let signal = AbortSignal::default();
    let query_instance = acknowledged.instance.clone();
    let request = acknowledged.request(LspOperation::GoToDefinition);
    let source = acknowledged.source("const x = 1\n");
    let query_signal = signal.clone();
    let pending = tokio::spawn(async move {
        query_instance
            .query(request, source, Some(&query_signal))
            .await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    abort_with(&signal, "acknowledged-abort");
    assert_eq!(
        pending.await.unwrap().unwrap_err().to_string(),
        "acknowledged-abort"
    );
    assert!(!acknowledged.instance.dead());
    acknowledged.finish().await;
}

#[tokio::test]
async fn handshake_abort_and_non_error_abort_reason_never_hang() {
    let handshake = make_instance([("LSP_FAKE_HANG_INITIALIZE", "1")], 100.0, 100.0, None).await;
    let signal = AbortSignal::default();
    let query_instance = handshake.instance.clone();
    let request = handshake.request(LspOperation::GoToDefinition);
    let source = handshake.source("const x = 1\n");
    let query_signal = signal.clone();
    let pending = tokio::spawn(async move {
        query_instance
            .query(request, source, Some(&query_signal))
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    abort_with(&signal, "handshake-abort");
    assert_eq!(
        pending.await.unwrap().unwrap_err().to_string(),
        "handshake-abort"
    );
    assert!(handshake.instance.dead());
    handshake.finish().await;

    let generic = make_instance([("LSP_FAKE_HANG", "1")], 100.0, 100.0, None).await;
    let signal = AbortSignal::default();
    let query_instance = generic.instance.clone();
    let request = generic.request(LspOperation::GoToDefinition);
    let source = generic.source("const x = 1\n");
    let query_signal = signal.clone();
    let pending = tokio::spawn(async move {
        query_instance
            .query(request, source, Some(&query_signal))
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    signal.abort_with_reason(json!("a string reason, not an Error"));
    assert!(
        pending
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("aborted")
    );
    generic.finish().await;
}

#[tokio::test]
async fn aborting_a_backpressured_did_open_write_forces_bounded_teardown() {
    let marker_root = tempfile::tempdir().unwrap();
    let marker = marker_root.path().join("initialized.log");
    let instance = make_instance(
        [
            (
                "LSP_FAKE_INITIALIZED_MARKER".to_owned(),
                marker.to_string_lossy().into_owned(),
            ),
            (
                "LSP_FAKE_PAUSE_STDIN_AFTER_INITIALIZED".to_owned(),
                "1".to_owned(),
            ),
        ],
        100.0,
        100.0,
        None,
    )
    .await;
    let signal = AbortSignal::default();
    let query_instance = instance.instance.clone();
    let request = instance.request(LspOperation::GoToDefinition);
    let source = instance.source("x".repeat(2_000_000));
    let query_signal = signal.clone();
    let pending = tokio::spawn(async move {
        query_instance
            .query(request, source, Some(&query_signal))
            .await
    });
    wait_for_file(&marker).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    abort_with(&signal, "didOpen-abort");
    assert_eq!(
        pending.await.unwrap().unwrap_err().to_string(),
        "didOpen-abort"
    );
    assert!(instance.instance.dead());
    instance.finish().await;
}

#[tokio::test]
async fn did_open_request_and_did_close_write_failures_own_teardown_exactly() {
    let did_open = make_instance(
        std::iter::empty::<(&str, &str)>(),
        100.0,
        100.0,
        Some(failing_writer("textDocument/didOpen")),
    )
    .await;
    assert!(
        did_open
            .run(LspOperation::GoToDefinition)
            .await
            .unwrap_err()
            .to_string()
            .contains("didOpen")
    );
    assert!(did_open.instance.dead());
    did_open.finish().await;

    let request = make_instance(
        std::iter::empty::<(&str, &str)>(),
        100.0,
        100.0,
        Some(failing_writer("textDocument/definition")),
    )
    .await;
    assert!(
        request
            .run(LspOperation::GoToDefinition)
            .await
            .unwrap_err()
            .to_string()
            .contains("textDocument/definition")
    );
    assert!(request.instance.dead());
    request.finish().await;

    let did_close = make_instance(
        [("LSP_FAKE_DEF", "null")],
        100.0,
        100.0,
        Some(failing_writer("textDocument/didClose")),
    )
    .await;
    assert_eq!(
        did_close.run(LspOperation::GoToDefinition).await.unwrap(),
        LspQueryResult::Locations {
            locations: Vec::new(),
            resolved_workspace_uri: did_close.workspace_uri.clone(),
        }
    );
    assert!(did_close.instance.dead());
    did_close.finish().await;
}

#[tokio::test]
async fn capability_and_server_failures_are_classified_without_false_abort() {
    let unsupported = standard_instance([
        ("LSP_FAKE_CAPS", r#"{"definitionProvider":false}"#),
        ("LSP_FAKE_DEF", "null"),
    ])
    .await;
    let error = unsupported
        .run(LspOperation::GoToDefinition)
        .await
        .unwrap_err();
    let lsp = error.downcast_ref::<LspError>().unwrap();
    assert_eq!(lsp.code(), "LSP_UNSUPPORTED_OPERATION");
    assert!(lsp.message().contains("goToDefinition"));
    unsupported.finish().await;

    let refused = standard_instance([("LSP_FAKE_ERROR", "1")]).await;
    let signal = AbortSignal::default();
    let error = refused
        .run_with_signal(LspOperation::GoToDefinition, &signal)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("server refused"));
    refused.finish().await;
}

#[tokio::test]
async fn graceful_idempotent_and_escalated_disposal_are_quiescent() {
    let marker_root = tempfile::tempdir().unwrap();
    let marker = marker_root.path().join("graceful-exit.log");
    let graceful = make_instance(
        [
            ("LSP_FAKE_DEF".to_owned(), "null".to_owned()),
            (
                "LSP_FAKE_EXIT_MARKER".to_owned(),
                marker.to_string_lossy().into_owned(),
            ),
            ("LSP_FAKE_EXIT_DELAY_MS".to_owned(), "75".to_owned()),
        ],
        500.0,
        200.0,
        None,
    )
    .await;
    graceful.run(LspOperation::GoToDefinition).await.unwrap();
    graceful.finish().await;
    assert_eq!(
        tokio::fs::read_to_string(&marker).await.unwrap(),
        "EXIT\nCLEAN\n"
    );

    let idempotent = standard_instance([("LSP_FAKE_DEF", "null")]).await;
    idempotent.run(LspOperation::GoToDefinition).await.unwrap();
    let (first, second) =
        tokio::join!(idempotent.instance.dispose(), idempotent.instance.dispose());
    first.unwrap();
    second.unwrap();
    assert!(idempotent.instance.dead());
    let error = idempotent
        .run(LspOperation::GoToDefinition)
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<LspError>().unwrap().code(),
        LSP_DISPOSED
    );

    let escalated = make_instance(
        [
            ("LSP_FAKE_DEF", "null"),
            ("LSP_FAKE_NO_SHUTDOWN", "1"),
            ("LSP_FAKE_IGNORE_SIGTERM", "1"),
        ],
        100.0,
        100.0,
        None,
    )
    .await;
    escalated.run(LspOperation::GoToDefinition).await.unwrap();
    escalated.finish().await;
    assert!(escalated.instance.dead());
}

#[cfg(unix)]
#[tokio::test]
async fn every_concurrent_disposer_awaits_a_surviving_helper_tree() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("helper.pid");
    let instance = make_instance(
        [
            ("LSP_FAKE_DEF".to_owned(), "null".to_owned()),
            (
                "LSP_FAKE_HELPER_PID_MARKER".to_owned(),
                marker.to_string_lossy().into_owned(),
            ),
        ],
        100.0,
        100.0,
        None,
    )
    .await;
    instance.run(LspOperation::GoToDefinition).await.unwrap();
    wait_for_file(&marker).await;
    let helper_pid = tokio::fs::read_to_string(&marker)
        .await
        .unwrap()
        .parse::<u32>()
        .unwrap();
    let (first, second) = tokio::join!(instance.instance.dispose(), instance.instance.dispose());
    first.unwrap();
    second.unwrap();
    wait_for_process_exit(helper_pid).await;
    assert!(!process_alive(helper_pid));
}

async fn wait_for_file(path: &Path) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if tokio::fs::metadata(path).await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture marker did not appear");
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    let status = std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if !status.is_ok_and(|status| status.success()) {
        return false;
    }
    #[cfg(target_os = "linux")]
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        let state = stat
            .rsplit_once(") ")
            .and_then(|(_, rest)| rest.split_whitespace().next());
        return !matches!(state, Some("Z" | "X" | "x"));
    }
    true
}

#[cfg(unix)]
async fn wait_for_process_exit(pid: u32) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while process_alive(pid) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("helper process did not exit");
}
