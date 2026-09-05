//! Scriptable SDK runtime peer for the out-of-process subagent provider.

use std::{
    fs::{OpenOptions, exists, write},
    io::{BufRead as _, Write as _},
    path::Path,
    time::Duration,
};

use serde_json::{Value, json};

const SERVER_NAME: &str = "seekdeep-harness-sdk-runtime";

fn variable(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

#[allow(clippy::needless_pass_by_value)]
fn send(value: Value) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &value)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

#[allow(clippy::needless_pass_by_value)]
fn event(event_type: &str, seq: u64, data: Value) -> Value {
    json!({"type":event_type,"seq":seq,"time":seq,"data":data})
}

#[allow(clippy::needless_pass_by_value)]
fn notify(session: &str, event: Value) -> anyhow::Result<()> {
    send(json!({
        "jsonrpc":"2.0",
        "method":"session.event",
        "params":{"sessionId":session,"event":event}
    }))
}

#[allow(clippy::needless_pass_by_value)]
fn response(id: &Value, result: Value) -> anyhow::Result<()> {
    send(json!({"jsonrpc":"2.0","id":id,"result":result}))
}

fn append_initialize(params: &Value) -> anyhow::Result<()> {
    let Some(path) = variable("SEEKDEEP_SDK_FIXTURE_RECORD_INIT") else {
        return Ok(());
    };
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut file, params)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn wait_for_go(path: &Path) {
    while !exists(path).unwrap_or(false) {
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn assistant_text() -> String {
    let mut lines = Vec::new();
    if variable("SEEKDEEP_SDK_FIXTURE_ECHO_CWD").is_some() {
        lines.push(format!(
            "cwd={}",
            std::env::current_dir().unwrap().display()
        ));
    }
    for name in variable("SEEKDEEP_SDK_FIXTURE_ECHO_ENV")
        .unwrap_or_default()
        .split(',')
        .filter(|name| !name.is_empty())
    {
        lines.push(format!("{name}={}", variable(name).unwrap_or_default()));
    }
    lines.push(
        variable("SEEKDEEP_SDK_FIXTURE_TEXT")
            .unwrap_or_else(|| "hello from fake runtime".to_owned()),
    );
    lines.join("\n")
}

fn initialize(frame: &Value) -> anyhow::Result<()> {
    append_initialize(&frame["params"])?;
    if variable("SEEKDEEP_SDK_FIXTURE_HANG_INIT").is_some() {
        return Ok(());
    }
    if let (Some(ready), Some(go)) = (
        variable("SEEKDEEP_SDK_FIXTURE_INIT_READY"),
        variable("SEEKDEEP_SDK_FIXTURE_INIT_GO"),
    ) {
        write(&ready, b"ready\n")?;
        wait_for_go(Path::new(&go));
    }
    if variable("SEEKDEEP_SDK_FIXTURE_BAD_INIT").is_some() {
        return response(&frame["id"], Value::Null);
    }
    response(
        &frame["id"],
        json!({"serverInfo":{"name":SERVER_NAME,"version":"0.0.1"}}),
    )
}

fn prompt(frame: &Value) -> anyhow::Result<()> {
    let session = frame["params"]["sessionId"].as_str().unwrap_or("session");
    let message_id = "fixture-user-1";
    notify(
        session,
        event(
            "agent/inbox/spliced",
            0,
            json!({"target":"next-turn","start":0,"inserted":[{"id":message_id}]}),
        ),
    )?;
    send(json!({
        "jsonrpc":"2.0","method":"session.status",
        "params":{"sessionId":session,"status":"running"}
    }))?;
    if variable("SEEKDEEP_SDK_FIXTURE_STREAM_THEN_MALFORMED").is_some() {
        notify(
            session,
            event(
                "assistant/chunk",
                1,
                json!({"chunk":{"type":"text-delta","index":0,"text":"unowned stream"}}),
            ),
        )?;
        return response(&frame["id"], json!({}));
    }
    if variable("SEEKDEEP_SDK_FIXTURE_HANG_PROMPT").is_some() {
        return Ok(());
    }
    if variable("SEEKDEEP_SDK_FIXTURE_BAD_PROMPT").is_some() {
        return response(&frame["id"], json!({}));
    }
    let text = assistant_text();
    notify(session, event("turn/start", 1, json!({"turn":0})))?;
    notify(
        session,
        event(
            "assistant/chunk",
            2,
            json!({"turn":0,"step":0,"chunk":{"type":"text-delta","index":0,"text":text}}),
        ),
    )?;
    if variable("SEEKDEEP_SDK_FIXTURE_MALFORMED_MESSAGE").is_some() {
        notify(
            session,
            event(
                "assistant/message",
                3,
                json!({"message":{"content":"not-an-array"}}),
            ),
        )?;
    } else {
        let content = if variable("SEEKDEEP_SDK_FIXTURE_EMPTY_MESSAGE").is_some() {
            json!([])
        } else {
            json!([{"type":"text","text":text}])
        };
        notify(
            session,
            event(
                "assistant/message",
                3,
                json!({"message":{"content":content}}),
            ),
        )?;
        if variable("SEEKDEEP_SDK_FIXTURE_REASON").as_deref() != Some("none") {
            let reason =
                variable("SEEKDEEP_SDK_FIXTURE_REASON").unwrap_or_else(|| "completed".to_owned());
            notify(
                session,
                event("turn/end", 4, json!({"reason":{"kind":reason}})),
            )?;
        }
    }
    send(json!({
        "jsonrpc":"2.0","method":"session.status",
        "params":{"sessionId":session,"status":"idle"}
    }))?;
    response(&frame["id"], json!({"messageId":message_id}))
}

fn main() -> anyhow::Result<()> {
    if let Some(path) = variable("SEEKDEEP_SDK_FIXTURE_SPAWNED") {
        write(path, b"spawned\n")?;
    }
    if variable("SEEKDEEP_SDK_FIXTURE_EXIT_BEFORE_INIT").is_some() {
        eprintln!("scripted boot failure");
        std::process::exit(3);
    }
    for line in std::io::stdin().lock().lines() {
        let frame: Value = serde_json::from_str(&line?)?;
        match frame.get("method").and_then(Value::as_str) {
            Some("initialize") => initialize(&frame)?,
            Some("session/prompt") => prompt(&frame)?,
            Some("shutdown") => {
                response(&frame["id"], json!({}))?;
                return Ok(());
            }
            Some(method) => send(json!({
                "jsonrpc":"2.0","id":frame["id"],
                "error":{"code":-32601,"message":format!("unknown method: {method}")}
            }))?,
            None => {}
        }
    }
    Ok(())
}
