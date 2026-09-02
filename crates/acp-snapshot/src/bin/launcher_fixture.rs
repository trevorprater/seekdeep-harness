//! Scripted NDJSON ACP child used by the launcher lifecycle tests.

use std::{
    env,
    io::{self, BufRead as _, Write as _},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use serde_json::{Map, Value, json};

const SESSION_ID: &str = "11111111-2222-4333-8444-555555555555";

fn main() -> anyhow::Result<()> {
    if env::args().any(|argument| argument == "--late-child") {
        return late_child();
    }
    if let Ok(note) = env::var("SEEKDEEP_ACP_FIXTURE_STDERR") {
        eprintln!("{note}");
    }
    if env::var_os("SEEKDEEP_ACP_FIXTURE_FAIL_BOOT").is_some() {
        std::process::exit(7);
    }

    let stdin = io::stdin();
    let mut parked_prompt = None::<Value>;
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let frame: Value = serde_json::from_str(&line)?;
        let method = frame.get("method").and_then(Value::as_str);
        let id = frame.get("id").cloned();
        match method {
            Some("initialize") => respond(
                id,
                json!({"protocolVersion":1,"agentCapabilities":{"loadSession":false}}),
            )?,
            Some("session/new") => respond(id, json!({"sessionId":SESSION_ID}))?,
            Some("session/prompt") => {
                update("thinking about it")?;
                if env::var_os("SEEKDEEP_ACP_FIXTURE_ECHO_ENV").is_some() {
                    update(&format!(
                        "env:{}",
                        json!({
                            "home":env::var("SEEKDEEP_HOME").ok(),
                            "agentsHome":env::var("SEEKDEEP_AGENTS_HOME").ok(),
                            "custom":env::var("SEEKDEEP_ACP_FIXTURE_CUSTOM").ok(),
                        })
                    ))?;
                }
                if env::var_os("SEEKDEEP_ACP_FIXTURE_PERMISSION").is_some() {
                    parked_prompt = id;
                    send(json!({
                        "id":1000,
                        "method":"session/request_permission",
                        "params":{
                            "sessionId":SESSION_ID,
                            "toolCall":{"toolCallId":"call_fixture_1"},
                            "options":[
                                {"optionId":"opt-allow","name":"Allow once","kind":"allow_once"},
                                {"optionId":"opt-reject","name":"Reject once","kind":"reject_once"}
                            ]
                        }
                    }))?;
                } else if env::var_os("SEEKDEEP_ACP_FIXTURE_HANG_PROMPT").is_none() {
                    respond(id, json!({"stopReason":"end_turn"}))?;
                } else {
                    parked_prompt = id;
                }
            }
            Some("session/cancel") => {
                if let Some(prompt) = parked_prompt.take() {
                    respond(Some(prompt), json!({"stopReason":"cancelled"}))?;
                }
            }
            None if id.as_ref().and_then(Value::as_u64) == Some(1000) => {
                update(&format!(
                    "permission:{}",
                    frame.get("result").cloned().unwrap_or(Value::Null)
                ))?;
                if let Some(prompt) = parked_prompt.take() {
                    respond(Some(prompt), json!({"stopReason":"end_turn"}))?;
                }
            }
            Some(method) => {
                if id.is_some() {
                    send(json!({
                        "id":id,
                        "error":{"code":-32603,"message":format!("unhandled method {method}")}
                    }))?;
                }
            }
            None => {}
        }
    }
    if env::var_os("SEEKDEEP_ACP_FIXTURE_LATE_OUTPUT").is_some() {
        Command::new(env::current_exe()?)
            .arg("--late-child")
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;
    }
    Ok(())
}

fn late_child() -> anyhow::Result<()> {
    thread::sleep(Duration::from_millis(50));
    update("late inherited stdout")?;
    thread::sleep(Duration::from_millis(25));
    eprintln!("late inherited stderr");
    Ok(())
}

fn update(text: &str) -> anyhow::Result<()> {
    send(json!({
        "method":"session/update",
        "params":{
            "sessionId":SESSION_ID,
            "update":{
                "sessionUpdate":"agent_message_chunk",
                "content":{"type":"text","text":text}
            }
        }
    }))
}

fn respond(id: Option<Value>, result: Value) -> anyhow::Result<()> {
    let mut frame = Map::new();
    frame.insert("id".to_owned(), id.unwrap_or(Value::Null));
    frame.insert("result".to_owned(), result);
    send(Value::Object(frame))
}

fn send(frame: Value) -> anyhow::Result<()> {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let Value::Object(frame) = frame else {
        anyhow::bail!("fixture ACP frame must be an object");
    };
    let mut envelope = Map::new();
    envelope.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
    envelope.extend(frame);
    stdout.write_all(serde_json::to_string(&Value::Object(envelope))?.as_bytes())?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}
