//! Real-process JSON-RPC connection parity coverage.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use parking_lot::Mutex;
use seekdeep_lsp_stdio::{
    ConnectionServerRequestHandler, ConnectionSpec, ConnectionWriter, LspConnection,
};
use seekdeep_subprocess_local::LocalSubprocessRuntime;
use serde_json::{Value, json};

const MAX_MESSAGE_BYTES: usize = 16_000_000;
const MAX_STDERR_BYTES: usize = 100_000;

struct TestConnection {
    connection: LspConnection,
    _runtime: Arc<LocalSubprocessRuntime>,
}

impl TestConnection {
    async fn finish(&self) {
        self.connection.terminate();
        tokio::time::timeout(Duration::from_secs(5), self.connection.closed())
            .await
            .expect("connection did not close");
        assert!(
            tokio::time::timeout(
                Duration::from_secs(5),
                self.connection.wait_for_process_tree_exit(None),
            )
            .await
            .expect("tree-exit wait timed out")
        );
    }
}

impl Drop for TestConnection {
    fn drop(&mut self) {
        self.connection.terminate();
    }
}

fn handler_null() -> ConnectionServerRequestHandler {
    Arc::new(|_, _| Box::pin(async { Ok(Some(Value::Null)) }))
}

fn connect(
    env: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
    handler: ConnectionServerRequestHandler,
    max_stderr_bytes: usize,
    writer: Option<ConnectionWriter>,
) -> TestConnection {
    let runtime = Arc::new(LocalSubprocessRuntime::new());
    let spec = ConnectionSpec {
        command: env!("CARGO_BIN_EXE_seekdeep-lsp-stdio-fixture").to_owned(),
        args: Vec::new(),
        cwd: std::env::current_dir().unwrap(),
        env: env
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect(),
        max_message_bytes: MAX_MESSAGE_BYTES,
        max_stderr_bytes,
        kill_grace_ms: 3_000.0,
        configuration: Some(json!({"setting": 42})),
    };
    let connection = LspConnection::new(&spec, runtime.as_ref(), handler, writer).unwrap();
    TestConnection {
        connection,
        _runtime: runtime,
    }
}

fn default_connection(
    env: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
) -> TestConnection {
    connect(env, handler_null(), MAX_STDERR_BYTES, None)
}

async fn initialize(connection: &LspConnection) -> Value {
    connection
        .request("initialize", Some(json!({"capabilities": {}})))
        .await
        .unwrap()
        .expect("initialize result")
}

async fn wait_for(predicate: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("wait_for timed out");
}

#[tokio::test]
async fn request_round_trip_explicit_managed_env_and_error_response_are_exact() {
    let plain = default_connection(std::iter::empty::<(&str, &str)>());
    let initialized = initialize(&plain.connection).await;
    assert_eq!(
        initialized.pointer("/capabilities/hoverProvider"),
        Some(&Value::Bool(true))
    );
    assert!(plain.connection.pid().as_i64() > 0);
    plain.finish().await;

    let managed = default_connection([
        ("LSP_FAKE_ECHO_ENV", "SEEKDEEP_LSP_TEST_FACT"),
        ("SEEKDEEP_LSP_TEST_FACT", "managed"),
    ]);
    initialize(&managed.connection).await;
    assert_eq!(
        managed
            .connection
            .request("textDocument/hover", Some(json!({})))
            .await
            .unwrap(),
        Some(json!({"contents": "managed"}))
    );
    managed.finish().await;

    let refused = default_connection([("LSP_FAKE_ERROR", "1")]);
    initialize(&refused.connection).await;
    let error = refused
        .connection
        .request("textDocument/hover", Some(json!({})))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("server refused the request"));
    assert!(!refused.connection.failed_with(&error));
    refused.finish().await;
}

#[tokio::test]
async fn server_requests_notifications_and_handler_errors_preserve_connection_health() {
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let handler_seen = seen.clone();
    let configuration_handler: ConnectionServerRequestHandler = Arc::new(move |method, params| {
        handler_seen.lock().push(method.clone());
        Box::pin(async move {
            if method == "workspace/configuration" {
                let count = params
                    .as_ref()
                    .and_then(|params| params.get("items"))
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                return Ok(Some(Value::Array(
                    (0..count).map(|_| json!({"setting": 42})).collect(),
                )));
            }
            Ok(Some(Value::Null))
        })
    });
    let configuration = connect(
        [("LSP_FAKE_ON_OPEN", "configuration")],
        configuration_handler,
        MAX_STDERR_BYTES,
        None,
    );
    initialize(&configuration.connection).await;
    configuration
        .connection
        .notify("textDocument/didOpen", Some(open_params()))
        .await
        .unwrap();
    wait_for(|| {
        seen.lock()
            .iter()
            .any(|method| method == "workspace/configuration")
    })
    .await;
    configuration.finish().await;

    let notification = default_connection([("LSP_FAKE_ON_OPEN", "notification")]);
    initialize(&notification.connection).await;
    notification
        .connection
        .notify("textDocument/didOpen", Some(open_params()))
        .await
        .unwrap();
    assert!(
        notification
            .connection
            .request("textDocument/hover", Some(json!({})))
            .await
            .is_ok()
    );
    notification.finish().await;

    let rejected_seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let handler_seen = rejected_seen.clone();
    let rejecting_handler: ConnectionServerRequestHandler = Arc::new(move |method, _| {
        handler_seen.lock().push(method.clone());
        Box::pin(async move {
            if method == "workspace/applyEdit" {
                anyhow::bail!("not permitted");
            }
            Ok(Some(Value::Null))
        })
    });
    let rejected = connect(
        [("LSP_FAKE_ON_OPEN", "applyEdit")],
        rejecting_handler,
        MAX_STDERR_BYTES,
        None,
    );
    initialize(&rejected.connection).await;
    rejected
        .connection
        .notify("textDocument/didOpen", Some(open_params()))
        .await
        .unwrap();
    wait_for(|| {
        rejected_seen
            .lock()
            .iter()
            .any(|method| method == "workspace/applyEdit")
    })
    .await;
    assert!(
        rejected
            .connection
            .request("textDocument/hover", Some(json!({})))
            .await
            .is_ok()
    );
    rejected.finish().await;
}

#[tokio::test]
async fn close_cancel_and_stderr_tail_boundaries_are_exact() {
    let closed = default_connection([("LSP_FAKE_EXIT_IMMEDIATELY", "1")]);
    tokio::time::timeout(Duration::from_secs(5), closed.connection.closed())
        .await
        .unwrap();
    closed.connection.terminate();
    let error = closed
        .connection
        .request("textDocument/hover", Some(json!({})))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("exited"));
    closed.connection.cancel(1);
    assert!(closed.connection.stderr_tail().len() <= MAX_STDERR_BYTES);
    closed.finish().await;

    let capped = connect(
        [
            ("LSP_FAKE_STDERR_TEXT".to_owned(), "E".repeat(200)),
            ("LSP_FAKE_STDERR_REPEAT".to_owned(), "3".to_owned()),
            ("LSP_FAKE_EXIT_IMMEDIATELY".to_owned(), "1".to_owned()),
        ],
        handler_null(),
        100,
        None,
    );
    capped.connection.closed().await;
    assert_eq!(capped.connection.stderr_tail(), "E".repeat(100));
    capped.finish().await;

    let multibyte = connect(
        [
            ("LSP_FAKE_STDERR_TEXT", "😀😀"),
            ("LSP_FAKE_EXIT_IMMEDIATELY", "1"),
        ],
        handler_null(),
        4,
        None,
    );
    multibyte.connection.closed().await;
    assert_eq!(multibyte.connection.stderr_tail(), "😀");
    assert_eq!(multibyte.connection.stderr_tail().len(), 4);
    multibyte.finish().await;
}

#[tokio::test]
async fn framing_spawn_write_and_midflight_failures_reject_every_waiter() {
    let framing = default_connection([("LSP_FAKE_INVALID_FRAME", "1")]);
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        framing.connection.request("initialize", Some(json!({}))),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.to_string().contains("Content-Length"));
    assert!(framing.connection.failed_with(&error));
    framing.finish().await;

    let exited = default_connection([("LSP_FAKE_EXIT_ON_REQUEST", "1")]);
    initialize(&exited.connection).await;
    let error = exited
        .connection
        .request("textDocument/hover", Some(json!({})))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("exited"));
    exited.finish().await;

    let writer: ConnectionWriter =
        Arc::new(|_, _| Box::pin(async { anyhow::bail!("fixture stdin failure") }));
    let failed_write = connect(
        std::iter::empty::<(&str, &str)>(),
        handler_null(),
        MAX_STDERR_BYTES,
        Some(writer),
    );
    let error = failed_write
        .connection
        .request("initialize", Some(json!({})))
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "fixture stdin failure");
    assert!(failed_write.connection.failed_with(&error));
    failed_write.finish().await;

    let runtime = Arc::new(LocalSubprocessRuntime::new());
    let missing = ConnectionSpec {
        command: "/definitely/not/a/real/binary/xyz".to_owned(),
        args: Vec::new(),
        cwd: std::env::current_dir().unwrap(),
        env: BTreeMap::new(),
        max_message_bytes: 1_000,
        max_stderr_bytes: 1_000,
        kill_grace_ms: 3_000.0,
        configuration: None,
    };
    let connection = LspConnection::new(&missing, runtime.as_ref(), handler_null(), None).unwrap();
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        connection.request("initialize", Some(json!({}))),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(!error.to_string().is_empty());
    connection.closed().await;
}

#[tokio::test]
async fn malformed_or_unowned_inbound_frames_are_ignored_and_fallback_errors_are_stable() {
    let ignored = default_connection([
        ("LSP_FAKE_NON_OBJECT_FRAMES", "1"),
        ("LSP_FAKE_STRAY_RESPONSES", "1"),
        ("LSP_FAKE_GARBAGE", "1"),
    ]);
    assert!(initialize(&ignored.connection).await.is_object());
    ignored.finish().await;

    let fallback = default_connection([("LSP_FAKE_ERROR_NO_MESSAGE", "1")]);
    initialize(&fallback.connection).await;
    let error = fallback
        .connection
        .request("textDocument/hover", Some(json!({})))
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "LSP error response");
    fallback.finish().await;
}

fn open_params() -> Value {
    json!({
        "textDocument": {
            "uri": "file:///x",
            "languageId": "ts",
            "version": 1,
            "text": "",
        }
    })
}
