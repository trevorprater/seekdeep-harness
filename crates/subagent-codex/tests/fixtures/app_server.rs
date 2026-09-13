//! Scriptable line-delimited app-server fixture for the real subprocess boundary.

use std::io::{BufRead as _, Write as _};

use seekdeep_lossless_json::JsonValue;
use serde_json::json;

fn send(value: &impl serde::Serialize) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

fn raw_text_frame(frame: &JsonValue) -> anyhow::Result<()> {
    let id = frame["id"].as_raw();
    match frame["method"].as_str() {
        Some("initialize") => send(&JsonValue::parse(format!(
            r#"{{"id":{id},"result":{{"userAgent":"fixture","\ud800":{{"value":"\udfff","huge":9007199254740993,"tiny":1e-400}}}}}}"#
        ))?),
        Some("thread/start") => send(&JsonValue::parse(format!(
            r#"{{"id":{id},"result":{{"thread":{{"id":"fixture-thread","ephemeral":true,"ignored":"\ud800"}},"ignored":"\udfff"}}}}"#
        ))?),
        Some("turn/start") => {
            anyhow::ensure!(
                frame["params"]["input"][0]["text"].to_utf16()
                    == Some(vec![0x41, 0xd800, 0x42, 0xdc00])
            );
            send(&JsonValue::parse(
                r#"{"method":"item/completed","params":{"threadId":"fixture-thread","turnId":"fixture-turn","item":{"type":"agentMessage","phase":"final_answer","text":"A\ud800B\udc00","ignored":{"\ud800":"\udfff"}}}}"#.to_owned()
            )?)?;
            send(&JsonValue::parse(format!(
                r#"{{"id":{id},"result":{{"turn":{{"id":"fixture-turn","ignored":"\ud800"}},"ignored":"\udfff"}}}}"#
            ))?)?;
            send(&JsonValue::parse(
                r#"{"id":"raw-approval","method":"item/commandExecution/requestApproval","params":{"threadId":"fixture-thread","turnId":"fixture-turn","availableDecisions":["\ud800","decline","cancel"],"ignored":{"\udfff":"\ud800"}}}"#.to_owned()
            )?)
        }
        None if frame["id"] == "raw-approval" => {
            anyhow::ensure!(frame["result"]["decision"] == "cancel");
            send(&JsonValue::parse(
                r#"{"method":"turn/completed","params":{"threadId":"fixture-thread","turn":{"id":"fixture-turn","status":"completed","error":null,"ignored":{"\ud800":"\udfff"}}}}"#.to_owned()
            )?)
        }
        _ => Ok(()),
    }
}

fn main() -> anyhow::Result<()> {
    let mode =
        std::env::var("SEEKDEEP_CODEX_FIXTURE_MODE").unwrap_or_else(|_| "success".to_owned());
    let stdin = std::io::stdin();
    let mut turn_open = false;
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let frame = JsonValue::parse(line)?;
        if mode == "raw-text" {
            raw_text_frame(&frame)?;
            continue;
        }
        let method = frame.get_value("method").and_then(JsonValue::as_str);
        match method {
            Some("initialize") => {
                if mode == "bad-initialize" {
                    send(&json!({"id":frame["id"], "result":null}))?;
                } else {
                    send(&json!({"id":frame["id"], "result":{"userAgent":"fixture"}}))?;
                }
            }
            Some("thread/start") => send(&json!({
                "id":frame["id"],
                "result":{"thread":{"id":"fixture-thread", "ephemeral":true}}
            }))?,
            Some("turn/start") => {
                turn_open = true;
                send(&json!({"id":frame["id"], "result":{"turn":{"id":"fixture-turn"}}}))?;
                if mode == "approval" {
                    send(&json!({
                        "id":"approval",
                        "method":"item/commandExecution/requestApproval",
                        "params":{
                            "threadId":"fixture-thread",
                            "turnId":"fixture-turn",
                            "availableDecisions":["decline","cancel"]
                        }
                    }))?;
                } else if mode != "wait" {
                    if mode != "empty" {
                        send(&json!({
                            "method":"item/completed",
                            "params":{
                                "threadId":"fixture-thread",
                                "turnId":"fixture-turn",
                                "item":{"type":"agentMessage","text":"fixture answer","phase":"final_answer"}
                            }
                        }))?;
                    }
                    send(&json!({
                        "method":"turn/completed",
                        "params":{
                            "threadId":"fixture-thread",
                            "turn":{"id":"fixture-turn","status":"completed","error":null}
                        }
                    }))?;
                    turn_open = false;
                }
            }
            Some("turn/interrupt") => {
                send(&json!({"id":frame["id"], "result":{}}))?;
                if turn_open {
                    send(&json!({
                        "method":"turn/completed",
                        "params":{
                            "threadId":"fixture-thread",
                            "turn":{"id":"fixture-turn","status":"interrupted","error":null}
                        }
                    }))?;
                    turn_open = false;
                }
            }
            None if frame["id"] == "approval" => {
                anyhow::ensure!(frame["result"]["decision"] == "cancel");
                send(&json!({
                    "method":"item/completed",
                    "params":{
                        "threadId":"fixture-thread",
                        "turnId":"fixture-turn",
                        "item":{"type":"agentMessage","text":"approval denied safely","phase":"final_answer"}
                    }
                }))?;
                send(&json!({
                    "method":"turn/completed",
                    "params":{
                        "threadId":"fixture-thread",
                        "turn":{"id":"fixture-turn","status":"completed","error":null}
                    }
                }))?;
                turn_open = false;
            }
            _ => {}
        }
    }
    Ok(())
}
