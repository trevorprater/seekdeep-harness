//! Scriptable line-delimited app-server fixture for the real subprocess boundary.

use std::io::{BufRead as _, Write as _};

use serde_json::{Value, json};

fn send(value: &Value) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
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
        let frame: Value = serde_json::from_str(&line)?;
        let method = frame.get("method").and_then(Value::as_str);
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
            None if frame.get("id") == Some(&json!("approval")) => {
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
