//! Scriptable Rust LSP fixture used by stdio transport integration tests.

use std::{collections::HashMap, env, path::Path};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = run_protocol() => result,
            _ = sigterm.recv() => {
                if enabled("LSP_FAKE_IGNORE_SIGTERM") {
                    std::future::pending::<()>().await;
                }
                mark_exit("TERM");
                Ok(())
            }
        }
    }
    #[cfg(not(unix))]
    run_protocol().await
}

async fn run_protocol() -> anyhow::Result<()> {
    if enabled("LSP_FAKE_HELPER_ONLY") {
        std::future::pending::<()>().await;
    }
    spawn_helper();
    if emit_startup_output().await? {
        return Ok(());
    }
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut server_request_id = 10_000_u64;
    let mut pending_server_requests = HashMap::<u64, String>::new();
    let mut pending_document_request = None;
    loop {
        let Some(message) = read_message(&mut reader).await? else {
            return Ok(());
        };
        let id = message.get("id").cloned();
        let method = message.get("method").and_then(Value::as_str);
        if log_server_reply(&message, &mut pending_server_requests) {
            continue;
        }
        match method {
            Some("initialize") => {
                if enabled("LSP_FAKE_HANG_INITIALIZE") {
                    continue;
                }
                send_initialize(id).await?;
            }
            Some("shutdown") => {
                if !enabled("LSP_FAKE_NO_SHUTDOWN") {
                    send(json!({"id": id, "result": null})).await?;
                }
            }
            Some("exit") => {
                mark_exit("EXIT");
                if let Some(delay) = env_u64("LSP_FAKE_EXIT_DELAY_MS") {
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                        mark_exit("CLEAN");
                        std::process::exit(0);
                    });
                } else {
                    mark_exit("CLEAN");
                    return Ok(());
                }
            }
            Some("initialized") => {
                append_marker("LSP_FAKE_INITIALIZED_MARKER", "INITIALIZED\n");
                if enabled("LSP_FAKE_PAUSE_STDIN_AFTER_INITIALIZED") {
                    std::future::pending::<()>().await;
                }
            }
            Some("textDocument/didOpen") => {
                if enabled("LSP_FAKE_CRASH_ON_OPEN") {
                    std::process::exit(1);
                }
                if let Ok(marker) = env::var("LSP_FAKE_OPEN_MARKER") {
                    let text = message
                        .pointer("/params/textDocument/text")
                        .map_or_else(|| Ok("undefined".to_owned()), serde_json::to_string)?;
                    append(&marker, &format!("{text}\n"));
                }
                if let Ok(kind) = env::var("LSP_FAKE_ON_OPEN") {
                    emit_server_request(
                        &kind,
                        &mut server_request_id,
                        &mut pending_server_requests,
                    )
                    .await?;
                }
            }
            Some("$/cancelRequest") => {
                if enabled("LSP_FAKE_CANCEL_ACK")
                    && let Some(request_id) = pending_document_request.take()
                {
                    send(json!({
                        "id": request_id,
                        "error": {"code": -32800, "message": "request cancelled"}
                    }))
                    .await?;
                }
            }
            Some("textDocument/didClose") => {}
            Some(method) if method.starts_with("textDocument/") => {
                if handle_document_request(method, id, &mut pending_document_request).await? {
                    return Ok(());
                }
            }
            Some(_) if id.is_some() => {
                send(json!({"id": id, "result": null})).await?;
            }
            _ => {}
        }
    }
}

fn log_server_reply(message: &Value, pending: &mut HashMap<u64, String>) -> bool {
    if message.get("method").is_some() {
        return false;
    }
    let Some(kind) = message
        .get("id")
        .and_then(Value::as_u64)
        .and_then(|id| pending.remove(&id))
    else {
        return false;
    };
    let mut reply = serde_json::Map::new();
    for field in ["result", "error"] {
        if let Some(value) = message.get(field) {
            reply.insert(field.to_owned(), value.clone());
        }
    }
    eprintln!("REPLY {kind} {}", Value::Object(reply));
    true
}

// The parent deliberately stays live while the helper is exercised; the
// subprocess tree owner kills and observes both processes during disposal.
#[allow(clippy::zombie_processes)]
fn spawn_helper() {
    let Ok(marker) = env::var("LSP_FAKE_HELPER_PID_MARKER") else {
        return;
    };
    let child = std::process::Command::new(env::current_exe().expect("fixture executable"))
        .env_clear()
        .env("LSP_FAKE_HELPER_ONLY", "1")
        .env("LSP_FAKE_IGNORE_SIGTERM", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("fixture helper spawn");
    append(&marker, &child.id().to_string());
}

async fn send_initialize(id: Option<Value>) -> anyhow::Result<()> {
    if enabled("LSP_FAKE_GARBAGE") {
        tokio::io::stdout()
            .write_all(b"this is not a framed message\r\n")
            .await?;
    }
    send(json!({"id": id, "result": {"capabilities": capabilities()}})).await
}

async fn handle_document_request(
    method: &str,
    id: Option<Value>,
    pending_document_request: &mut Option<Value>,
) -> anyhow::Result<bool> {
    if enabled("LSP_FAKE_EXIT_ON_REQUEST") {
        return Ok(true);
    }
    if enabled("LSP_FAKE_HANG") {
        *pending_document_request = id;
        return Ok(false);
    }
    let method = method.to_owned();
    let delay = env_u64("LSP_FAKE_REPLY_DELAY_MS").unwrap_or(0);
    if delay > 0 {
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            if send_document_reply(&method, id).await.is_ok() {
                schedule_exit_after_reply();
            }
        });
        return Ok(false);
    }
    send_document_reply(&method, id).await?;
    schedule_exit_after_reply();
    Ok(false)
}

async fn send_document_reply(method: &str, id: Option<Value>) -> anyhow::Result<()> {
    if enabled("LSP_FAKE_ERROR_NO_MESSAGE") {
        send(json!({"id": id, "error": {"code": -1}})).await?;
    } else if enabled("LSP_FAKE_ERROR") {
        send(json!({
            "id": id,
            "error": {"code": -32000, "message": "server refused the request"}
        }))
        .await?;
    } else {
        send(json!({"id": id, "result": result_for(method)})).await?;
    }
    Ok(())
}

fn schedule_exit_after_reply() {
    if enabled("LSP_FAKE_EXIT_AFTER_REPLY") {
        tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            std::process::exit(0);
        });
    }
}

fn capabilities() -> Value {
    let mut capabilities = serde_json::Map::from_iter([
        (
            "positionEncoding".to_owned(),
            Value::String(env::var("LSP_FAKE_ENCODING").unwrap_or_else(|_| "utf-16".to_owned())),
        ),
        (
            "textDocumentSync".to_owned(),
            env_json("LSP_FAKE_SYNC").unwrap_or(json!(1)),
        ),
        ("definitionProvider".to_owned(), Value::Bool(true)),
        ("referencesProvider".to_owned(), Value::Bool(true)),
        ("implementationProvider".to_owned(), Value::Bool(true)),
        ("hoverProvider".to_owned(), Value::Bool(true)),
    ]);
    if let Some(Value::Object(extra)) = env_json("LSP_FAKE_CAPS") {
        capabilities.extend(extra);
    }
    Value::Object(capabilities)
}

async fn emit_startup_output() -> anyhow::Result<bool> {
    if enabled("LSP_FAKE_INVALID_FRAME") {
        tokio::io::stdout()
            .write_all(b"Content-Length: abc\r\n\r\n{}")
            .await?;
    }
    if enabled("LSP_FAKE_NON_OBJECT_FRAMES") {
        send(Value::from(42)).await?;
        send(Value::Null).await?;
    }
    if enabled("LSP_FAKE_STRAY_RESPONSES") {
        send(json!({"id": 999, "result": {"stray": true}})).await?;
        send(json!({"id": "str-id"})).await?;
    }
    if let Ok(text) = env::var("LSP_FAKE_STDERR_TEXT") {
        let repeats = env_u64("LSP_FAKE_STDERR_REPEAT").unwrap_or(1);
        for _ in 0..repeats {
            tokio::io::stderr().write_all(text.as_bytes()).await?;
        }
    }
    if enabled("LSP_FAKE_EXIT_IMMEDIATELY") {
        return Ok(true);
    }
    Ok(false)
}

async fn read_message(reader: &mut BufReader<tokio::io::Stdin>) -> anyhow::Result<Option<Value>> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            return Ok(None);
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            content_length = Some(value.trim().parse::<usize>()?);
        }
    }
    let length = content_length.ok_or_else(|| anyhow::anyhow!("missing Content-Length"))?;
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body).await?;
    Ok(Some(serde_json::from_slice(&body)?))
}

async fn emit_server_request(
    kind: &str,
    next_id: &mut u64,
    pending: &mut HashMap<u64, String>,
) -> anyhow::Result<()> {
    if kind == "notification" {
        return send(json!({
            "method": "window/logMessage",
            "params": {"type": 3, "message": "hello"}
        }))
        .await;
    }
    let id = *next_id;
    *next_id += 1;
    let method = match kind {
        "configuration" => "workspace/configuration",
        "applyEdit" => "workspace/applyEdit",
        "lifecycle" => "client/registerCapability",
        _ => "window/showMessageRequest",
    };
    let params = if kind == "configuration" {
        json!({"items": [{"section": "a"}, {"section": "b"}]})
    } else {
        json!({})
    };
    pending.insert(id, method.to_owned());
    send(json!({"id": id, "method": method, "params": params})).await
}

async fn send(message: Value) -> anyhow::Result<()> {
    let message = match message {
        Value::Object(mut object) => {
            object.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
            Value::Object(object)
        }
        value => value,
    };
    let body = serde_json::to_vec(&message)?;
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(&body);
    let mut stdout = tokio::io::stdout();
    stdout.write_all(&frame).await?;
    stdout.flush().await?;
    Ok(())
}

fn result_for(method: &str) -> Value {
    if method == "textDocument/hover"
        && let Ok(name) = env::var("LSP_FAKE_ECHO_ENV")
    {
        return json!({
            "contents": env::var(&name).unwrap_or_else(|_| format!("<{name} unset>"))
        });
    }
    let variable = match method {
        "textDocument/definition" => "LSP_FAKE_DEF",
        "textDocument/references" => "LSP_FAKE_REFS",
        "textDocument/implementation" => "LSP_FAKE_IMPL",
        "textDocument/hover" => "LSP_FAKE_HOVER",
        _ => return Value::Null,
    };
    env_json(variable).unwrap_or(Value::Null)
}

fn env_json(name: &str) -> Option<Value> {
    env::var(name)
        .ok()
        .map(|value| serde_json::from_str(&value).expect("fixture JSON environment"))
}

fn env_u64(name: &str) -> Option<u64> {
    env::var(name).ok().and_then(|value| value.parse().ok())
}

fn enabled(name: &str) -> bool {
    env::var(name).as_deref() == Ok("1")
}

fn mark_exit(event: &str) {
    append_marker("LSP_FAKE_EXIT_MARKER", &format!("{event}\n"));
}

fn append_marker(variable: &str, text: &str) {
    if let Ok(path) = env::var(variable) {
        append(&path, text);
    }
}

fn append(path: impl AsRef<Path>, text: &str) {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("fixture marker");
    file.write_all(text.as_bytes())
        .expect("fixture marker write");
}
