//! Scriptable native Claude stream-json peer for real process tests.

use std::io::{BufRead as _, Write as _};

use serde_json::{Value, json};

#[allow(clippy::needless_pass_by_value)]
fn emit(value: Value) -> anyhow::Result<()> {
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, &value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let mode =
        std::env::var("SEEKDEEP_CLAUDE_FIXTURE_MODE").unwrap_or_else(|_| "success".to_owned());
    if let Ok(path) = std::env::var("SEEKDEEP_CLAUDE_FIXTURE_SPAWNED") {
        std::fs::write(path, b"spawned\n")?;
    }
    let mut input = String::new();
    if let Some(line) = std::io::stdin().lock().lines().next() {
        input = line?;
    }
    if let Ok(path) = std::env::var("SEEKDEEP_CLAUDE_FIXTURE_RECORD") {
        std::fs::write(
            path,
            serde_json::to_vec(&json!({
                "args":std::env::args().skip(1).collect::<Vec<_>>(),
                "cwd":std::env::current_dir()?.to_string_lossy(),
                "input":serde_json::from_str::<Value>(&input)?,
                "apiKey":std::env::var("ANTHROPIC_API_KEY").ok()
            }))?,
        )?;
    }
    match mode.as_str() {
        "success" => emit(json!({
            "type":"result","subtype":"success","is_error":false,
            "result":std::env::var("SEEKDEEP_CLAUDE_FIXTURE_ANSWER")
                .unwrap_or_else(|_| "fixture answer".to_owned())
        }))?,
        "two-success" => {
            emit(json!({"type":"result","subtype":"success","is_error":false,"result":"first"}))?;
            emit(json!({"type":"result","subtype":"success","is_error":false,"result":"latest"}))?;
        }
        "error-result" => emit(json!({
            "type":"result","subtype":"error_during_execution","is_error":true,
            "errors":["fixture failure","second detail"]
        }))?,
        "invalid-success" => emit(json!({
            "type":"result","subtype":"success","is_error":true,"result":""
        }))?,
        "missing-result" => emit(json!({"type":"assistant","message":{"content":[]}}))?,
        "malformed-after-success" => {
            emit(
                json!({"type":"result","subtype":"success","is_error":false,"result":"must be discarded"}),
            )?;
            std::io::stdout().write_all(b"not-json\n")?;
        }
        "success-then-error" => {
            emit(json!({
                "type":"result","subtype":"success","is_error":false,
                "result":"must be discarded"
            }))?;
            emit(json!({
                "type":"result","subtype":"error_during_execution","is_error":true,
                "errors":["late failure"]
            }))?;
        }
        "exit-error" => std::process::exit(7),
        "hold" => {
            if let Ok(path) = std::env::var("SEEKDEEP_CLAUDE_FIXTURE_READY") {
                std::fs::write(path, b"ready\n")?;
            }
            loop {
                std::thread::park();
            }
        }
        other => anyhow::bail!("unknown fixture mode: {other}"),
    }
    Ok(())
}
