//! Deterministic stdio MCP server used by real-process bridge tests.

use std::io::{BufRead as _, Write as _};

use serde_json::{Value, json};

fn send(value: &Value) -> anyhow::Result<()> {
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn record(value: &Value) -> anyhow::Result<()> {
    let Ok(path) = std::env::var("SEEKDEEP_MCP_FIXTURE_RECORD") else {
        return Ok(());
    };
    let mut output = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    serde_json::to_writer(&mut output, value)?;
    output.write_all(b"\n")?;
    Ok(())
}

fn tool(name: &str, description: &str, input_schema: &Value) -> Value {
    json!({"name":name,"description":description,"inputSchema":input_schema})
}

fn tools() -> Vec<Value> {
    vec![
        tool(
            "add",
            "Adds two numbers.",
            &json!({
                "type":"object",
                "properties":{"a":{"type":"number"},"b":{"type":"number"}},
                "required":["a","b"]
            }),
        ),
        tool(
            "greet",
            "Greets a person by name.",
            &json!({
                "type":"object",
                "properties":{"name":{"type":"string"}},
                "required":["name"]
            }),
        ),
        tool(
            "fail",
            "Always returns an error.",
            &json!({"type":"object"}),
        ),
        tool("image", "Returns an image.", &json!({"type":"object"})),
        tool("crash", "Replies and exits.", &json!({"type":"object"})),
        tool(
            "admin.reset",
            "A dotted-name tool.",
            &json!({"type":"object"}),
        ),
    ]
}

fn call(name: &str, arguments: &Value) -> Value {
    match name {
        "add" => json!({"content":[{"type":"text","text":
            (arguments["a"].as_f64().unwrap_or_default()
                + arguments["b"].as_f64().unwrap_or_default()).to_string()
        }]}),
        "greet" => json!({"content":[{"type":"text","text":format!(
            "Hello, {}!", arguments["name"].as_str().unwrap_or_default()
        )}]}),
        "fail" => json!({
            "content":[{"type":"text","text":"Something went wrong"}],
            "isError":true
        }),
        "image" => json!({"content":[
            {"type":"text","text":"Here is an image:"},
            {"type":"image","data":"iVBORw0KGgo=","mimeType":"image/png"},
            {"type":"text","text":"End of image."}
        ]}),
        "crash" => json!({"content":[{"type":"text","text":"crashing"}]}),
        "admin.reset" => json!({"content":[{"type":"text","text":"reset done"}]}),
        _ => json!({"content":[{"type":"text","text":"unknown tool"}],"isError":true}),
    }
}

fn main() -> anyhow::Result<()> {
    for line in std::io::stdin().lock().lines() {
        let frame: Value = serde_json::from_str(&line?)?;
        record(&frame)?;
        let Some(method) = frame.get("method").and_then(Value::as_str) else {
            continue;
        };
        match method {
            "initialize" => send(&json!({
                "jsonrpc":"2.0","id":frame["id"],"result":{
                    "protocolVersion":frame["params"]["protocolVersion"],
                    "capabilities":{"tools":{"listChanged":true}},
                    "serverInfo":{"name":"seekdeep-fixture","version":"1.0.0"}
                }
            }))?,
            "tools/list" => send(&json!({
                "jsonrpc":"2.0","id":frame["id"],"result":{"tools":tools()}
            }))?,
            "tools/call" => {
                let name = frame["params"]["name"].as_str().unwrap_or_default();
                send(&json!({
                    "jsonrpc":"2.0","id":frame["id"],
                    "result":call(name, &frame["params"]["arguments"])
                }))?;
                if name == "crash" {
                    std::thread::sleep(std::time::Duration::from_millis(25));
                    std::process::exit(7);
                }
            }
            "notifications/initialized" | "notifications/cancelled" => {}
            _ if frame.get("id").is_some() => send(&json!({
                "jsonrpc":"2.0","id":frame["id"],
                "error":{"code":-32601,"message":"method not found"}
            }))?,
            _ => {}
        }
    }
    Ok(())
}
