//! Scriptable ACP JSON-RPC child for real subprocess provider tests.

use std::io::{BufRead as _, Write as _};

use serde_json::{Value, json};

fn send(value: &Value) -> anyhow::Result<()> {
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn append_record(value: &Value) -> anyhow::Result<()> {
    let Ok(path) = std::env::var("SEEKDEEP_ACP_FIXTURE_RECORD") else {
        return Ok(());
    };
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn main() -> anyhow::Result<()> {
    let mode = std::env::var("SEEKDEEP_ACP_FIXTURE_MODE").unwrap_or_else(|_| "normal".to_owned());
    if let Ok(path) = std::env::var("SEEKDEEP_ACP_FIXTURE_SPAWNED") {
        std::fs::write(path, b"spawned\n")?;
    }
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let mut pending_prompt: Option<Value> = None;
    let mut announced_cwd = String::new();
    while let Some(line) = lines.next() {
        let frame: Value = serde_json::from_str(&line?)?;
        append_record(&frame)?;
        let method = frame.get("method").and_then(Value::as_str);
        match method {
            Some("initialize") => send(&json!({
                "jsonrpc":"2.0","id":frame["id"],"result":{
                    "protocolVersion":1,"agentInfo":{"name":"fixture","version":"1"},
                    "agentCapabilities":{"promptCapabilities":{"image":false,"audio":false,"embeddedContext":false}},
                    "authMethods":[]
                }
            }))?,
            Some("session/new") => {
                frame["params"]["cwd"]
                    .as_str()
                    .unwrap_or_default()
                    .clone_into(&mut announced_cwd);
                if let (Ok(ready), Ok(go)) = (
                    std::env::var("SEEKDEEP_ACP_FIXTURE_NEW_READY"),
                    std::env::var("SEEKDEEP_ACP_FIXTURE_NEW_GO"),
                ) {
                    std::fs::write(ready, b"ready\n")?;
                    while !std::path::Path::new(&go).exists() {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                }
                if mode == "missing-session" {
                    send(&json!({"jsonrpc":"2.0","id":frame["id"],"result":{}}))?;
                } else {
                    send(
                        &json!({"jsonrpc":"2.0","id":frame["id"],"result":{"sessionId":"remote-session"}}),
                    )?;
                }
            }
            Some("session/prompt") => {
                if mode == "crash-prompt" {
                    std::process::exit(7);
                }
                let session = frame["params"]["sessionId"].clone();
                if std::env::var_os("SEEKDEEP_ACP_FIXTURE_THOUGHT").is_some() {
                    send(&json!({
                        "jsonrpc":"2.0","method":"session/update",
                        "params":{"sessionId":session,"update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"hidden"}}}
                    }))?;
                }
                if std::env::var_os("SEEKDEEP_ACP_FIXTURE_PERMISSION").is_some() {
                    let options = if std::env::var_os("SEEKDEEP_ACP_FIXTURE_NO_ALLOW").is_some() {
                        json!([{"optionId":"reject","name":"Reject","kind":"reject_once"}])
                    } else {
                        json!([
                            {"optionId":"allow","name":"Allow","kind":"allow_once"},
                            {"optionId":"reject","name":"Reject","kind":"reject_once"}
                        ])
                    };
                    send(&json!({
                        "jsonrpc":"2.0","id":"permission-1","method":"session/request_permission",
                        "params":{"sessionId":session,"toolCall":{"toolCallId":"call-1"},"options":options}
                    }))?;
                    let permission: Value = serde_json::from_str(
                        &lines
                            .next()
                            .ok_or_else(|| anyhow::anyhow!("permission response EOF"))??,
                    )?;
                    append_record(&permission)?;
                    let allowed =
                        permission.pointer("/result/outcome/optionId") == Some(&json!("allow"));
                    if !allowed {
                        send(&json!({
                            "jsonrpc":"2.0","id":frame["id"],"result":{"stopReason":"cancelled"}
                        }))?;
                        continue;
                    }
                }
                if mode == "hang" || mode == "ignore-cancel" {
                    pending_prompt = Some(frame["id"].clone());
                    if let Ok(path) = std::env::var("SEEKDEEP_ACP_FIXTURE_READY") {
                        std::fs::write(path, b"ready\n")?;
                    }
                    continue;
                }
                let answer = if mode == "cwd" {
                    format!(
                        "{}\n{announced_cwd}",
                        std::fs::canonicalize(std::env::current_dir()?)?.display()
                    )
                } else {
                    std::env::var("SEEKDEEP_ACP_FIXTURE_TEXT")
                        .unwrap_or_else(|_| "fixture answer".to_owned())
                };
                send(&json!({
                    "jsonrpc":"2.0","method":"session/update",
                    "params":{"sessionId":session,"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":answer}}}
                }))?;
                send(&json!({
                    "jsonrpc":"2.0","id":frame["id"],"result":{
                        "stopReason":std::env::var("SEEKDEEP_ACP_FIXTURE_STOP").unwrap_or_else(|_| "end_turn".to_owned())
                    }
                }))?;
            }
            Some("session/cancel") => {
                if mode != "ignore-cancel"
                    && let Some(id) = pending_prompt.take()
                {
                    send(&json!({"jsonrpc":"2.0","id":id,"result":{"stopReason":"cancelled"}}))?;
                }
            }
            Some(method) => send(&json!({
                "jsonrpc":"2.0","id":frame["id"],
                "error":{"code":-32601,"message":format!("unknown method: {method}")}
            }))?,
            None => {}
        }
    }
    if let Ok(path) = std::env::var("SEEKDEEP_ACP_FIXTURE_FLUSH") {
        std::fs::write(path, b"flushed\n")?;
    }
    Ok(())
}
