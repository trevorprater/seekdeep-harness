//! Deterministic Rust LSP server for the ACP definition snapshot.

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut input = BufReader::new(tokio::io::stdin());
    while let Some(message) = read_message(&mut input).await? {
        let id = message.get("id").cloned();
        match message.get("method").and_then(Value::as_str) {
            Some("initialize") => {
                send(json!({
                    "id": id,
                    "result": {
                        "capabilities": {
                            "positionEncoding": "utf-16",
                            "textDocumentSync": 1,
                            "definitionProvider": true
                        }
                    }
                }))
                .await?;
            }
            Some("textDocument/definition") => {
                let uri = url::Url::from_file_path(std::env::current_dir()?.join("subject.ts"))
                    .map_err(|()| anyhow::anyhow!("LSP fixture workspace is not an absolute path"))?
                    .to_string();
                send(json!({
                    "id": id,
                    "result": [location(&uri, 0), location(&uri, 1)]
                }))
                .await?;
            }
            Some("shutdown") => send(json!({"id": id, "result": null})).await?,
            Some("exit") => return Ok(()),
            Some(_) if id.is_some() => send(json!({"id": id, "result": null})).await?,
            _ => {}
        }
    }
    Ok(())
}

fn location(uri: &str, line: u64) -> Value {
    json!({
        "uri": uri,
        "range": {
            "start": {"line": line, "character": 6},
            "end": {"line": line, "character": 12}
        }
    })
}

async fn read_message(input: &mut BufReader<tokio::io::Stdin>) -> anyhow::Result<Option<Value>> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).await? == 0 {
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
    input.read_exact(&mut body).await?;
    Ok(Some(serde_json::from_slice(&body)?))
}

async fn send(message: Value) -> anyhow::Result<()> {
    let mut object = message
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("LSP fixture response must be an object"))?;
    object.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
    let body = serde_json::to_vec(&object)?;
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(&body);
    let mut output = tokio::io::stdout();
    output.write_all(&frame).await?;
    output.flush().await?;
    Ok(())
}
