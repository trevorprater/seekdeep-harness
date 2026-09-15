//! End-to-end provider pooling and lifecycle parity over the Rust fixture server.

use std::{path::Path, sync::Arc, time::Duration};

use seekdeep_cordis::Context;
use seekdeep_fs_local::{Config as FsConfig, LocalFileSystem};
use seekdeep_llm::AbortSignal;
use seekdeep_lsp::{Lsp, LspOperation, LspPosition, LspQueryRequest, LspQueryResult};
use seekdeep_lsp_stdio::plugin;
use seekdeep_subprocess_local::LocalSubprocessRuntime;
use seekdeep_util::timeout::deadline;
use serde_json::{Map, Value, json};
use tempfile::TempDir;
use thiserror::Error;

struct Mounted {
    context: Context,
    lsp: Arc<Lsp>,
}

impl Mounted {
    async fn finish(&self) {
        tokio::time::timeout(Duration::from_secs(5), self.context.fiber().restart())
            .await
            .expect("mounted tree teardown timed out")
            .unwrap();
    }
}

async fn workspace_fixture() -> (TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let canonical = tokio::fs::canonicalize(root.path()).await.unwrap();
    let workspace = canonical.join("ws");
    tokio::fs::create_dir(&workspace).await.unwrap();
    tokio::fs::write(workspace.join("a.ts"), "const x = 1\nconst y = x\n")
        .await
        .unwrap();
    (root, workspace)
}

fn fake_server(env: Value, overrides: Value) -> Value {
    let mut server = Map::from_iter([
        (
            "command".to_owned(),
            Value::String(env!("CARGO_BIN_EXE_seekdeep-lsp-stdio-fixture").to_owned()),
        ),
        ("args".to_owned(), json!([])),
        ("env".to_owned(), env),
        (
            "extensionToLanguage".to_owned(),
            json!({".ts": "typescript"}),
        ),
    ]);
    if let Value::Object(overrides) = overrides {
        server.extend(overrides);
    }
    Value::Object(server)
}

async fn mount(root: &Path, servers: Value) -> Mounted {
    let context = Context::new();
    let lsp = Arc::new(Lsp::new());
    lsp.provide(&context).unwrap();
    LocalSubprocessRuntime::install(&context).unwrap();
    LocalFileSystem::install(
        &context,
        FsConfig {
            cwd: Some(root.to_string_lossy().into_owned()),
            diff_basis_max_bytes: None,
        },
    )
    .unwrap();
    let fiber = context
        .plugin(plugin(), json!({"servers": servers}))
        .unwrap();
    fiber.await_settled().await.unwrap();
    Mounted { context, lsp }
}

fn query(workspace: &Path, operation: LspOperation, file_path: &str) -> LspQueryRequest {
    LspQueryRequest {
        operation,
        file_path: file_path.to_owned(),
        position: LspPosition {
            line: 0.0,
            character: 6.0,
        },
        workspace_root: workspace.to_string_lossy().into_owned(),
    }
}

fn workspace_uri(workspace: &Path) -> String {
    url::Url::from_file_path(workspace).unwrap().to_string()
}

fn location(workspace: &Path, line: u64) -> Value {
    json!({
        "uri": url::Url::from_file_path(workspace.join("a.ts")).unwrap().to_string(),
        "range": {
            "start": {"line": line, "character": 0},
            "end": {"line": line, "character": 3},
        }
    })
}

#[tokio::test]
async fn routes_extensions_to_independent_configured_servers() {
    let (root, workspace) = workspace_fixture().await;
    tokio::fs::write(workspace.join("a.py"), "x = 1\n")
        .await
        .unwrap();
    let routed = mount(
        root.path(),
        json!({
            "typescript": fake_server(
                json!({"LSP_FAKE_HOVER": json!({"contents": "ts"}).to_string()}),
                json!({}),
            ),
            "python": fake_server(
                json!({"LSP_FAKE_HOVER": json!({"contents": "py"}).to_string()}),
                json!({"extensionToLanguage": {".py": "python"}}),
            ),
        }),
    )
    .await;
    for (file, expected) in [("a.ts", "ts"), ("a.py", "py")] {
        assert_eq!(
            routed
                .lsp
                .query(query(&workspace, LspOperation::Hover, file), None)
                .await
                .unwrap(),
            LspQueryResult::Hover {
                hover: Some(seekdeep_lsp::LspHover {
                    contents: expected.to_owned(),
                    range: None,
                })
            }
        );
    }
    routed.finish().await;
}

#[tokio::test]
async fn normalizes_every_closed_query_result() {
    let (root, workspace) = workspace_fixture().await;
    let cases = [
        (
            "LSP_FAKE_DEF",
            location(&workspace, 0),
            LspOperation::GoToDefinition,
            1,
        ),
        (
            "LSP_FAKE_IMPL",
            json!([{
                "targetUri": url::Url::from_file_path(workspace.join("a.ts")).unwrap().to_string(),
                "targetSelectionRange": {
                    "start": {"line": 1, "character": 0},
                    "end": {"line": 1, "character": 2},
                }
            }]),
            LspOperation::GoToImplementation,
            1,
        ),
        (
            "LSP_FAKE_REFS",
            json!([location(&workspace, 0), location(&workspace, 1)]),
            LspOperation::FindReferences,
            2,
        ),
    ];
    for (variable, payload, operation, expected) in cases {
        let mounted = mount(
            root.path(),
            json!({"fake": fake_server(json!({(variable): payload.to_string()}), json!({}))}),
        )
        .await;
        let result = mounted
            .lsp
            .query(query(&workspace, operation, "a.ts"), None)
            .await
            .unwrap();
        assert!(matches!(
            result,
            LspQueryResult::Locations { ref locations, ref resolved_workspace_uri }
                if locations.len() == expected && resolved_workspace_uri == &workspace_uri(&workspace)
        ));
        mounted.finish().await;
    }

    let hover = mount(
        root.path(),
        json!({"fake": fake_server(
            json!({"LSP_FAKE_HOVER": json!({"contents": {"kind": "markdown", "value": "docs"}}).to_string()}),
            json!({}),
        )}),
    )
    .await;
    assert!(matches!(
        hover
            .lsp
            .query(query(&workspace, LspOperation::Hover, "a.ts"), None)
            .await
            .unwrap(),
        LspQueryResult::Hover { hover: Some(ref hover) } if hover.contents == "docs"
    ));
    hover.finish().await;

    for (variable, operation) in [
        ("LSP_FAKE_DEF", LspOperation::GoToDefinition),
        ("LSP_FAKE_HOVER", LspOperation::Hover),
    ] {
        let mounted = mount(
            root.path(),
            json!({"fake": fake_server(json!({(variable): "null"}), json!({}))}),
        )
        .await;
        let result = mounted
            .lsp
            .query(query(&workspace, operation, "a.ts"), None)
            .await
            .unwrap();
        assert!(match result {
            LspQueryResult::Locations { locations, .. } => locations.is_empty(),
            LspQueryResult::Hover { hover } => hover.is_none(),
        });
        mounted.finish().await;
    }
}

#[tokio::test]
async fn initialization_sync_and_capability_failures_never_poison_the_pool() {
    let (root, workspace) = workspace_fixture().await;
    let marker = root.path().join("initialize-rejection-exit.log");
    let encoding = mount(
        root.path(),
        json!({"fake": fake_server(
            json!({
                "LSP_FAKE_ENCODING": "utf-8",
                "LSP_FAKE_DEF": "null",
                "LSP_FAKE_EXIT_MARKER": marker,
            }),
            json!({}),
        )}),
    )
    .await;
    for _ in 0..2 {
        let error = encoding
            .lsp
            .query(
                query(&workspace, LspOperation::GoToDefinition, "a.ts"),
                None,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("unsupported position encoding"));
    }
    assert_eq!(
        tokio::fs::read_to_string(&marker).await.unwrap(),
        "EXIT\nCLEAN\nEXIT\nCLEAN\n"
    );
    encoding.finish().await;

    let unsupported_sync = mount(
        root.path(),
        json!({"fake": fake_server(
            json!({"LSP_FAKE_SYNC": "0", "LSP_FAKE_DEF": "null"}),
            json!({}),
        )}),
    )
    .await;
    assert!(
        unsupported_sync
            .lsp
            .query(
                query(&workspace, LspOperation::GoToDefinition, "a.ts"),
                None,
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("transient textDocument/didOpen")
    );
    unsupported_sync.finish().await;

    let options_sync = mount(
        root.path(),
        json!({"fake": fake_server(
            json!({"LSP_FAKE_SYNC": json!({"openClose": true, "change": 2}).to_string(), "LSP_FAKE_DEF": "null"}),
            json!({}),
        )}),
    )
    .await;
    assert!(
        options_sync
            .lsp
            .query(
                query(&workspace, LspOperation::GoToDefinition, "a.ts"),
                None,
            )
            .await
            .is_ok()
    );
    options_sync.finish().await;

    let unsupported_operation = mount(
        root.path(),
        json!({"fake": fake_server(
            json!({"LSP_FAKE_CAPS": json!({"hoverProvider": false}).to_string()}),
            json!({}),
        )}),
    )
    .await;
    assert!(
        unsupported_operation
            .lsp
            .query(query(&workspace, LspOperation::Hover, "a.ts"), None)
            .await
            .unwrap_err()
            .to_string()
            .contains("does not support hover")
    );
    unsupported_operation.finish().await;
}

#[tokio::test]
async fn workspace_queue_reads_current_bytes_and_serializes_one_instance() {
    let (root, workspace) = workspace_fixture().await;
    let outside = root.path().join("outside.ts");
    tokio::fs::write(&outside, "secret").await.unwrap();
    let marker = root.path().join("opened.jsonl");
    let mounted = mount(
        root.path(),
        json!({"fake": fake_server(
            json!({
                "LSP_FAKE_DEF": "null",
                "LSP_FAKE_REPLY_DELAY_MS": "150",
                "LSP_FAKE_OPEN_MARKER": marker,
            }),
            json!({}),
        )}),
    )
    .await;
    let error = mounted
        .lsp
        .query(
            query(
                &workspace,
                LspOperation::GoToDefinition,
                &outside.to_string_lossy(),
            ),
            None,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("outside the workspace"));

    let first_lsp = mounted.lsp.clone();
    let first_request = query(&workspace, LspOperation::GoToDefinition, "a.ts");
    let first = tokio::spawn(async move { first_lsp.query(first_request, None).await });
    wait_for_lines(&marker, 1).await;
    let second_lsp = mounted.lsp.clone();
    let second_request = query(&workspace, LspOperation::GoToDefinition, "a.ts");
    let second = tokio::spawn(async move { second_lsp.query(second_request, None).await });
    tokio::fs::write(workspace.join("a.ts"), "const changed = 2\n")
        .await
        .unwrap();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(
        marker_lines(&marker).await,
        [
            "const x = 1\nconst y = x\n".to_owned(),
            "const changed = 2\n".to_owned(),
        ]
    );

    let (one, two, three) = tokio::join!(
        mounted.lsp.query(
            query(&workspace, LspOperation::GoToDefinition, "a.ts"),
            None,
        ),
        mounted.lsp.query(
            query(&workspace, LspOperation::GoToDefinition, "a.ts"),
            None,
        ),
        mounted.lsp.query(
            query(&workspace, LspOperation::GoToDefinition, "a.ts"),
            None,
        ),
    );
    one.unwrap();
    two.unwrap();
    three.unwrap();
    mounted.finish().await;
}

#[derive(Debug, Error)]
#[error("{0}")]
struct CallerAbort(&'static str);

fn abort(signal: &AbortSignal, message: &'static str) {
    signal.abort_with_error(Arc::new(CallerAbort(message)), json!({"message": message}));
}

#[tokio::test]
async fn caller_abort_timeout_and_preabort_preserve_their_first_reason() {
    let (root, workspace) = workspace_fixture().await;
    let mounted = mount(
        root.path(),
        json!({"fake": fake_server(json!({"LSP_FAKE_HANG": "1"}), json!({"killGraceMs": 50}))}),
    )
    .await;
    let signal = AbortSignal::default();
    let lsp = mounted.lsp.clone();
    let request = query(&workspace, LspOperation::GoToDefinition, "a.ts");
    let query_signal = signal.clone();
    let pending = tokio::spawn(async move { lsp.query(request, Some(query_signal)).await });
    tokio::task::yield_now().await;
    abort(&signal, "caller cancelled");
    assert_eq!(
        pending.await.unwrap().unwrap_err().to_string(),
        "caller cancelled"
    );

    let preaborted = AbortSignal::default();
    abort(&preaborted, "pre-aborted");
    let error = mounted
        .lsp
        .query(
            query(&workspace, LspOperation::GoToDefinition, "a.ts"),
            Some(preaborted),
        )
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "pre-aborted");

    let timeout = deadline(None, 50.0, "TEST_TIMEOUT").unwrap();
    let error = mounted
        .lsp
        .query(
            query(&workspace, LspOperation::GoToDefinition, "a.ts"),
            Some(timeout.signal.clone()),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("TEST_TIMEOUT"), "{error:#}");
    mounted.finish().await;
}

#[tokio::test]
async fn transport_failures_replace_once_and_idle_death_is_transparent() {
    let (root, workspace) = workspace_fixture().await;
    let stderr = mount(
        root.path(),
        json!({"fake": fake_server(
            json!({
                "LSP_FAKE_STDERR_TEXT": "FATAL: boom\n",
                "LSP_FAKE_EXIT_IMMEDIATELY": "1",
            }),
            json!({}),
        )}),
    )
    .await;
    let error = stderr
        .lsp
        .query(
            query(&workspace, LspOperation::GoToDefinition, "a.ts"),
            None,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("FATAL: boom"), "{error:#}");
    stderr.finish().await;

    let crashing = mount(
        root.path(),
        json!({"fake": fake_server(
            json!({"LSP_FAKE_CRASH_ON_OPEN": "1", "LSP_FAKE_DEF": "null"}),
            json!({"shutdownTimeoutMs": 100, "killGraceMs": 100}),
        )}),
    )
    .await;
    for _ in 0..2 {
        assert!(
            crashing
                .lsp
                .query(
                    query(&workspace, LspOperation::GoToDefinition, "a.ts"),
                    None,
                )
                .await
                .is_err()
        );
    }
    crashing.finish().await;

    let location = location(&workspace, 0).to_string();
    let idle = mount(
        root.path(),
        json!({"fake": fake_server(
            json!({"LSP_FAKE_EXIT_AFTER_REPLY": "1", "LSP_FAKE_DEF": location}),
            json!({}),
        )}),
    )
    .await;
    for _ in 0..2 {
        assert!(
            idle.lsp
                .query(
                    query(&workspace, LspOperation::GoToDefinition, "a.ts"),
                    None,
                )
                .await
                .is_ok()
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    idle.finish().await;
}

#[tokio::test]
async fn distinct_workspaces_parallelize_and_shutdown_escalation_is_owned() {
    let (root, workspace) = workspace_fixture().await;
    let workspace_two = root.path().join("ws2");
    tokio::fs::create_dir(&workspace_two).await.unwrap();
    tokio::fs::write(workspace_two.join("a.ts"), "const z = 2\n")
        .await
        .unwrap();
    let mounted = mount(
        root.path(),
        json!({"fake": fake_server(
            json!({
                "LSP_FAKE_DEF": "null",
                "LSP_FAKE_REPLY_DELAY_MS": "150",
                "LSP_FAKE_NO_SHUTDOWN": "1",
            }),
            json!({"killGraceMs": 100, "shutdownTimeoutMs": 100}),
        )}),
    )
    .await;
    let started = tokio::time::Instant::now();
    let (first, second) = tokio::join!(
        mounted.lsp.query(
            query(&workspace, LspOperation::GoToDefinition, "a.ts"),
            None,
        ),
        mounted.lsp.query(
            query(&workspace_two, LspOperation::GoToDefinition, "a.ts"),
            None,
        ),
    );
    first.unwrap();
    second.unwrap();
    assert!(started.elapsed() < Duration::from_millis(280));
    mounted.finish().await;
}

async fn marker_lines(path: &Path) -> Vec<String> {
    let Ok(text) = tokio::fs::read_to_string(path).await else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

async fn wait_for_lines(path: &Path, count: usize) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while marker_lines(path).await.len() < count {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("didOpen marker did not advance");
}
