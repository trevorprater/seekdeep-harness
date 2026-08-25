//! Config, Loader shape, stateless envelope, and Streamable HTTP wire parity.

use std::{collections::BTreeMap, sync::Arc};

use parking_lot::Mutex;
use seekdeep_llm::AbortSignal;
use seekdeep_mcp_client::{
    Config, INJECT, McpClientFactory, NAME, NativeMcpClientFactory, ReconnectConfig,
    normalize_tool_schemas, resolve_reconnect_policy,
};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

#[derive(Clone, Debug)]
struct RequestRecord {
    method: String,
    headers: BTreeMap<String, String>,
    body: Value,
}

fn fixture_response(method: &str, body: &Value) -> (&'static str, Option<Value>, bool) {
    let rpc_method = body.get("method").and_then(Value::as_str);
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    match (method, rpc_method) {
        ("DELETE", _) => ("200 OK", None, false),
        (_, Some("initialize")) => (
            "200 OK",
            Some(json!({"jsonrpc":"2.0","id":id,"result":{
                "protocolVersion":"2025-11-25",
                "capabilities":{"tools":{}},
                "serverInfo":{"name":"fixture","version":"1"}
            }})),
            true,
        ),
        (_, Some("notifications/initialized" | "notifications/cancelled")) => {
            ("202 Accepted", None, false)
        }
        (_, Some("tools/list")) => (
            "200 OK",
            Some(json!({"jsonrpc":"2.0","id":id,"result":{"tools":[{
                "name":"ping","description":"Replies pong.",
                "inputSchema":{"properties":{"value":{"type":"string"}}}
            }]}})),
            false,
        ),
        (_, Some("tools/call")) => (
            "200 OK",
            Some(json!({"jsonrpc":"2.0","id":id,"result":{
                "content":[{"type":"text","text":"pong"}]
            }})),
            false,
        ),
        _ => ("404 Not Found", Some(json!({"error":"unknown"})), false),
    }
}

async fn handle_http(mut stream: tokio::net::TcpStream, observed: Arc<Mutex<Vec<RequestRecord>>>) {
    let mut bytes = Vec::new();
    let mut scratch = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut scratch).await.unwrap();
        if read == 0 {
            return;
        }
        bytes.extend_from_slice(&scratch[..read]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let header_text = String::from_utf8_lossy(&bytes[..header_end]);
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let method = request_line
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned();
    let mut headers = BTreeMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    while bytes.len() - header_end < content_length {
        let read = stream.read(&mut scratch).await.unwrap();
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&scratch[..read]);
    }
    let body = if content_length == 0 {
        Value::Null
    } else {
        serde_json::from_slice(&bytes[header_end..header_end + content_length]).unwrap()
    };
    observed.lock().push(RequestRecord {
        method: method.clone(),
        headers: headers.clone(),
        body: body.clone(),
    });
    let (status, response, session_header) = fixture_response(&method, &body);
    let payload = response
        .map(|value| serde_json::to_vec(&value).unwrap())
        .unwrap_or_default();
    let session = if session_header {
        "mcp-session-id: session-1\r\n"
    } else {
        ""
    };
    let head = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n{session}connection: close\r\n\r\n",
        payload.len()
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(&payload).await.unwrap();
    stream.shutdown().await.unwrap();
}

async fn http_fixture() -> (
    String,
    Arc<Mutex<Vec<RequestRecord>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let records = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&records);
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let observed = Arc::clone(&observed);
            tokio::spawn(handle_http(stream, observed));
        }
    });
    (format!("http://{address}/mcp"), records, task)
}

fn stateless(url: String) -> Config {
    Config::StatelessHttp {
        server_name: "stateless".to_owned(),
        url,
        headers: BTreeMap::from([("Authorization".to_owned(), "Bearer tok".to_owned())]),
        protocol_version: "2026-07-28".to_owned(),
        tool_call_timeout_ms: 60_000.0,
        fail_on_startup_error: false,
        reconnect: None,
    }
}

#[test]
fn config_defaults_validation_and_namespace_shape_match_the_source() {
    assert_eq!(NAME, "mcp-client");
    assert_eq!(INJECT, ["tools"]);
    let config: Config = serde_json::from_value(json!({
        "transport":"stdio","serverName":"github-prod_1","command":"echo"
    }))
    .unwrap();
    let normalized = config.normalized().unwrap();
    let value = serde_json::to_value(normalized).unwrap();
    assert_eq!(value["args"], json!([]));
    assert_eq!(value["env"], json!({}));
    assert_eq!(value["cwd"], "");
    assert_eq!(value["toolCallTimeoutMs"], 60_000.0);
    assert_eq!(
        value["reconnect"],
        json!({"enabled":true,"initialDelayMs":500.0,"maxDelayMs":30000.0,"maxAttempts":10.0})
    );
    for server_name in [String::new(), "bad name!".to_owned(), "x".repeat(33)] {
        let invalid: Config = serde_json::from_value(json!({
            "transport":"stdio","serverName":server_name,"command":"echo"
        }))
        .unwrap();
        assert!(invalid.validate().is_err());
    }
}

#[test]
fn reconnect_policy_rejects_every_invalid_numeric_boundary() {
    let path = "mcp-client(srv): reconnect";
    assert_eq!(
        resolve_reconnect_policy(None, path).unwrap().max_attempts,
        10
    );
    assert!(
        resolve_reconnect_policy(
            Some(&ReconnectConfig {
                initial_delay_ms: Some(0.0),
                ..ReconnectConfig::default()
            }),
            path,
        )
        .unwrap_err()
        .to_string()
        .contains("positive finite")
    );
    assert!(
        resolve_reconnect_policy(
            Some(&ReconnectConfig {
                initial_delay_ms: Some(100.0),
                max_delay_ms: Some(5.0),
                ..ReconnectConfig::default()
            }),
            path,
        )
        .unwrap_err()
        .to_string()
        .contains("less than or equal")
    );
    for attempts in [0.0, 1.5] {
        assert!(
            resolve_reconnect_policy(
                Some(&ReconnectConfig {
                    max_attempts: Some(attempts),
                    ..ReconnectConfig::default()
                }),
                path,
            )
            .unwrap_err()
            .to_string()
            .contains("positive integer")
        );
    }
}

#[test]
fn stateless_schema_normalization_is_narrow_and_lossless() {
    let mut response = json!({"result":{"tools":[
        {"name":"missing","inputSchema":{"properties":{"x":{"type":"string"}}}},
        {"name":"array","inputSchema":{"type":"array"}},
        {"name":"invalid","inputSchema":null}
    ]}});
    normalize_tool_schemas(&mut response);
    assert_eq!(
        response.pointer("/result/tools/0/inputSchema/type"),
        Some(&json!("object"))
    );
    assert_eq!(
        response.pointer("/result/tools/1/inputSchema/type"),
        Some(&json!("array"))
    );
    assert_eq!(
        response.pointer("/result/tools/2/inputSchema"),
        Some(&Value::Null)
    );
}

#[tokio::test]
async fn stateless_http_stamps_meta_never_handshakes_and_preserves_inline_replies() {
    let (url, records, server) = http_fixture().await;
    let client = NativeMcpClientFactory
        .create(&stateless(url))
        .await
        .unwrap();
    client.connect().await.unwrap();
    let page = client.list_tools(None).await.unwrap();
    assert_eq!(page.tools[0].name, "ping");
    assert_eq!(page.tools[0].input_schema["type"], "object");
    let result = client
        .call_tool("ping", Map::new(), AbortSignal::default())
        .await
        .unwrap();
    assert_eq!(result.pointer("/content/0/text"), Some(&json!("pong")));
    client.close().await.unwrap();
    let records = records.lock();
    assert_eq!(records.len(), 2);
    assert!(records.iter().all(|request| request.method == "POST"));
    assert!(
        records
            .iter()
            .all(|request| request.body["method"] != "initialize")
    );
    for request in records.iter() {
        assert_eq!(request.headers["authorization"], "Bearer tok");
        assert_eq!(
            request
                .body
                .pointer("/params/_meta/io.modelcontextprotocol~1protocolVersion"),
            Some(&json!("2026-07-28"))
        );
        assert_eq!(
            request
                .body
                .pointer("/params/_meta/io.modelcontextprotocol~1clientCapabilities"),
            Some(&json!({}))
        );
    }
    drop(records);
    server.abort();
}

#[tokio::test]
async fn streamable_http_handshakes_carries_session_headers_and_closes() {
    let (url, records, server) = http_fixture().await;
    let config = Config::StreamableHttp {
        server_name: "web".to_owned(),
        url,
        headers: BTreeMap::from([("Authorization".to_owned(), "Bearer web".to_owned())]),
        tool_call_timeout_ms: 60_000.0,
        fail_on_startup_error: false,
        reconnect: None,
    };
    let client = NativeMcpClientFactory.create(&config).await.unwrap();
    client.connect().await.unwrap();
    client.list_tools(None).await.unwrap();
    client
        .call_tool("ping", Map::new(), AbortSignal::default())
        .await
        .unwrap();
    client.close().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while records.lock().len() < 5 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let records = records.lock();
    assert_eq!(records[0].body["method"], "initialize");
    assert_eq!(records[1].body["method"], "notifications/initialized");
    assert_eq!(records[2].body["method"], "tools/list");
    assert_eq!(records[3].body["method"], "tools/call");
    assert_eq!(records[4].method, "DELETE");
    for request in &records[1..] {
        assert_eq!(request.headers["mcp-session-id"], "session-1");
        assert_eq!(request.headers["mcp-protocol-version"], "2025-11-25");
    }
    drop(records);
    server.abort();
}
