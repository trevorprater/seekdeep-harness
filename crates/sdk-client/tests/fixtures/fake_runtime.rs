//! Scriptable SDK runtime peer for real subprocess client tests.

use std::{
    fs::{OpenOptions, exists, write},
    io::{BufRead as _, Write as _},
};

use serde_json::{Value, json};

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
fn notify(method: &str, params: Value) -> anyhow::Result<()> {
    send(json!({"jsonrpc":"2.0","method":method,"params":params}))
}

#[allow(clippy::needless_pass_by_value)]
fn response(id: &Value, result: Value) -> anyhow::Result<()> {
    send(json!({"jsonrpc":"2.0","id":id,"result":result}))
}

fn mode_value<'a>(mode: &'a str, prefix: &str) -> Option<&'a str> {
    mode.strip_prefix(prefix)
        .and_then(|value| value.strip_prefix('='))
}

fn record(path: &str, value: &Value) -> anyhow::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn initialize(mode: &str, frame: &Value) -> anyhow::Result<()> {
    if let Some(path) = mode_value(mode, "record-init") {
        record(path, &frame["params"])?;
    }
    if let Some(path) = mode_value(mode, "record-cwd") {
        record(path, &frame["params"])?;
    }
    if mode == "init-error" {
        return send(json!({
            "jsonrpc":"2.0","id":frame["id"],
            "error":{"code":7,"message":"scripted init failure","data":{"hint":"fake"}}
        }));
    }
    if let Some(path) = mode_value(mode, "init-error-once")
        && !exists(path)?
    {
        write(path, b"failed once\n")?;
        return send(json!({
            "jsonrpc":"2.0","id":frame["id"],
            "error":{"code":7,"message":"scripted first-boot failure"}
        }));
    }
    if mode == "bad-init" {
        return response(&frame["id"], Value::Null);
    }
    let version = if mode_value(mode, "record-cwd").is_some() {
        std::env::current_dir()?.to_string_lossy().into_owned()
    } else {
        "0.0.1".to_owned()
    };
    response(
        &frame["id"],
        json!({"serverInfo":{"name":"seekdeep-harness-sdk-runtime","version":version}}),
    )
}

fn prompt(mode: &str, frame: &Value) -> anyhow::Result<()> {
    let session = frame["params"]["sessionId"].as_str().unwrap_or("session");
    let message_id = "message-1";
    notify(
        "session.event",
        json!({"sessionId":session,"event":event("fixture/before",0,json!({}))}),
    )?;
    notify(
        "session.event",
        json!({
            "sessionId":session,
            "event":event("agent/inbox/spliced",1,json!({"inserted":[{"id":message_id}]}))
        }),
    )?;
    notify(
        "session.status",
        json!({"sessionId":session,"status":"running"}),
    )?;
    if mode == "hang-prompt" {
        return Ok(());
    }
    if mode == "bad-prompt" {
        return response(&frame["id"], json!({}));
    }
    if mode == "malformed-event" {
        notify("session.event", json!({"sessionId":session,"event":42}))?;
    } else if mode == "malformed-message" {
        notify(
            "session.event",
            json!({
                "sessionId":session,
                "event":event("assistant/message",2,json!({"message":{"content":"bad"}}))
            }),
        )?;
    } else if mode == "message-no-data" {
        notify(
            "session.event",
            json!({"sessionId":session,"event":{"type":"assistant/message","seq":2,"time":2}}),
        )?;
    } else {
        notify(
            "session.event",
            json!({
                "sessionId":session,
                "event":event("assistant/message",2,json!({"message":{"content":[{"type":"text","text":"fixture final"}]}}))
            }),
        )?;
        notify(
            "session.event",
            json!({
                "sessionId":session,
                "event":event("turn/end",3,json!({"reason":{"kind":"completed"}}))
            }),
        )?;
        notify(
            "subagent.started",
            json!({"parentSessionId":session,"childSessionId":"child-1"}),
        )?;
        notify(
            "session.event",
            json!({"sessionId":"child-1","event":event("fixture/child",4,json!({}))}),
        )?;
        notify(
            "subagent.started",
            json!({"parentSessionId":"child-1","childSessionId":"grandchild-1"}),
        )?;
        notify(
            "session.event",
            json!({"sessionId":"grandchild-1","event":event("fixture/grandchild",5,json!({}))}),
        )?;
        notify(
            "session.event",
            json!({"sessionId":"stranger","event":event("fixture/foreign",6,json!({}))}),
        )?;
        notify(
            "subagent.finished",
            json!({
                "provider":"fixture","agentId":"grandchild-1",
                "parentSessionId":"child-1","childSessionId":"grandchild-1",
                "status":"ok","stopReason":"completed"
            }),
        )?;
        notify(
            "subagent.finished",
            json!({
                "provider":"fixture","agentId":"child-1",
                "parentSessionId":session,"childSessionId":"child-1",
                "status":"ok","stopReason":"completed"
            }),
        )?;
    }
    notify(
        "session.status",
        json!({"sessionId":session,"status":"idle"}),
    )?;
    response(&frame["id"], json!({"messageId":message_id}))
}

fn main() -> anyhow::Result<()> {
    let mode = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "normal".to_owned());
    match mode.as_str() {
        "exit-error" => {
            eprintln!("fixture runtime stderr marker");
            std::process::exit(7);
        }
        "exit-no-newline" => {
            std::io::stderr().write_all(b"unterminated stderr marker")?;
            std::process::exit(7);
        }
        "exit-many-lines" => {
            for index in 0..450 {
                eprintln!("line-{index}");
            }
            std::process::exit(7);
        }
        _ => {}
    }
    for line in std::io::stdin().lock().lines() {
        let frame: Value = serde_json::from_str(&line?)?;
        match frame.get("method").and_then(Value::as_str) {
            Some("initialize") => initialize(&mode, &frame)?,
            Some("session/prompt") => prompt(&mode, &frame)?,
            Some("hold") | None => {}
            Some("shutdown") => {
                response(&frame["id"], json!({}))?;
                return Ok(());
            }
            Some(other) => send(json!({
                "jsonrpc":"2.0","id":frame["id"],
                "error":{"code":-32601,"message":format!("method not found: {other}")}
            }))?,
        }
    }
    Ok(())
}
