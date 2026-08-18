//! Official file-backed Codex OAuth route integration test.

use std::collections::BTreeMap;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures::TryStreamExt;
use seekdeep_llm::{GenerateOptions, LlmRuntime, ModelId, ProviderId, StreamChunk};
use seekdeep_llm_pi_ai::codex_auth::{OPENAI_CODEX_PROVIDER_ID, create_codex_credential_bridge};
use seekdeep_llm_pi_ai::plugin::plugin;
use seekdeep_util::launch_environment::{
    LaunchEnvironmentLayerInput, LaunchEnvironmentSource, SEEKDEEP_LAUNCH_ENVIRONMENT,
    create_launch_environment_snapshot,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
};

struct Captured {
    request: String,
    body: Value,
}

fn jwt(account: &str) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
    let payload = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "exp":4_102_444_800_u64,
            "https://api.openai.com/auth":{"chatgpt_account_id":account}
        }))
        .unwrap(),
    );
    format!("{header}.{payload}.signature")
}

async fn auth_home() -> TempDir {
    let home = tempfile::tempdir().unwrap();
    let access = jwt("acct_123");
    let path = home.path().join("auth.json");
    tokio::fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "auth_mode":"chatgpt",
        "tokens":{"id_token":"identity","access_token":access,"refresh_token":"refresh","account_id":"acct_123"}
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    home
}

fn response_events() -> Vec<Value> {
    let part = json!({"type":"output_text","annotations":[],"text":"hello"});
    let message = json!({"id":"msg_fixture","type":"message","status":"completed","role":"assistant","content":[part]});
    let completed = json!({
        "id":"resp_fixture","status":"completed","model":"gpt-5.3-codex-spark","output":[message.clone()],
        "usage":{"input_tokens":3,"input_tokens_details":{"cached_tokens":0},"output_tokens":1,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":4}
    });
    vec![
        json!({"type":"response.created","response":{"id":"resp_fixture","status":"in_progress"}}),
        json!({"type":"response.output_item.added","output_index":0,"item":{"id":"msg_fixture","type":"message","status":"in_progress","role":"assistant","content":[]}}),
        json!({"type":"response.output_text.delta","output_index":0,"delta":"hello"}),
        json!({"type":"response.output_item.done","output_index":0,"item":message}),
        json!({"type":"response.completed","response":completed}),
    ]
}

async fn server() -> (String, oneshot::Receiver<Captured>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let header_end = loop {
            let mut buffer = [0_u8; 4096];
            let read = socket.read(&mut buffer).await.unwrap();
            bytes.extend_from_slice(&buffer[..read]);
            if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let request = String::from_utf8_lossy(&bytes[..header_end]).into_owned();
        let length = request
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().ok())
                    .flatten()
            })
            .unwrap_or(0_usize);
        while bytes.len() - header_end < length {
            let mut buffer = vec![0_u8; length - (bytes.len() - header_end)];
            let read = socket.read(&mut buffer).await.unwrap();
            bytes.extend_from_slice(&buffer[..read]);
        }
        let wire_body = &bytes[header_end..header_end + length];
        let body_bytes = if request
            .to_ascii_lowercase()
            .contains("content-encoding: zstd")
        {
            zstd::stream::decode_all(wire_body).unwrap()
        } else {
            wire_body.to_vec()
        };
        let body = serde_json::from_slice(&body_bytes).unwrap();
        let _ = tx.send(Captured { request, body });
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        for event in response_events() {
            socket
                .write_all(
                    format!("data: {}\n\n", serde_json::to_string(&event).unwrap()).as_bytes(),
                )
                .await
                .unwrap();
        }
    });
    (format!("http://{address}"), rx)
}

async fn codex_failure(home: &TempDir) -> Vec<StreamChunk> {
    let context = seekdeep_cordis::Context::new();
    let runtime = LlmRuntime::install(&context).unwrap();
    let snapshot = create_launch_environment_snapshot(&[LaunchEnvironmentLayerInput {
        source: LaunchEnvironmentSource::Process,
        path: None,
        values: BTreeMap::from([("CODEX_HOME".to_owned(), home.path().display().to_string())]),
    }]);
    context
        .provide(SEEKDEEP_LAUNCH_ENVIRONMENT, std::sync::Arc::new(snapshot))
        .unwrap();
    let fiber = context
        .plugin(plugin(), json!({"providers":{"openai-codex":{}}}))
        .unwrap();
    fiber.await_settled().await.unwrap();
    let model = runtime.list_models("openai-codex").await.unwrap()[0]
        .id
        .clone();
    let chunks = runtime
        .stream(GenerateOptions::new(
            ProviderId::new("openai-codex"),
            model,
            vec![],
        ))
        .try_collect()
        .await
        .unwrap();
    fiber.dispose().await.unwrap();
    chunks
}

#[tokio::test]
async fn official_auth_file_reaches_codex_sse_with_account_and_replay() {
    let home = auth_home().await;
    let (url, captured) = server().await;
    let context = seekdeep_cordis::Context::new();
    let runtime = LlmRuntime::install(&context).unwrap();
    let snapshot = create_launch_environment_snapshot(&[LaunchEnvironmentLayerInput {
        source: LaunchEnvironmentSource::Process,
        path: None,
        values: BTreeMap::from([("CODEX_HOME".to_owned(), home.path().display().to_string())]),
    }]);
    create_codex_credential_bridge(&snapshot)
        .store
        .read(OPENAI_CODEX_PROVIDER_ID)
        .await
        .unwrap();
    context
        .provide(SEEKDEEP_LAUNCH_ENVIRONMENT, std::sync::Arc::new(snapshot))
        .unwrap();
    let fiber = context
        .plugin(
            plugin(),
            json!({"providers":{"openai-codex":{
                "baseURL":url,"transport":"sse","models":[{"id":"gpt-5.3-codex-spark"}]
            }}}),
        )
        .unwrap();
    fiber.await_settled().await.unwrap();
    let options = GenerateOptions::new(
        ProviderId::new("openai-codex"),
        ModelId::new("gpt-5.3-codex-spark"),
        vec![],
    );
    let chunks: Vec<StreamChunk> = runtime.stream(options).try_collect().await.unwrap();
    assert!(
        matches!(
            chunks.last(),
            Some(StreamChunk::Finish {
                reason: seekdeep_llm::FinishReason::Stop,
                ..
            })
        ),
        "{chunks:#?}"
    );
    let captured = captured.await.unwrap();
    assert!(captured.request.starts_with("POST /codex/responses"));
    let headers = captured.request.to_ascii_lowercase();
    assert!(headers.contains("authorization: bearer "));
    assert!(headers.contains("chatgpt-account-id: acct_123"));
    assert!(headers.contains("openai-beta: responses=experimental"));
    assert!(headers.contains("content-encoding: zstd"));
    assert_eq!(
        captured.body["instructions"],
        json!("You are a helpful assistant.")
    );
    assert_eq!(
        captured.body["include"],
        json!(["reasoning.encrypted_content"])
    );
    fiber.dispose().await.unwrap();
}

#[tokio::test]
async fn missing_shared_session_fails_before_network_and_directs_codex_login() {
    let home = tempfile::tempdir().unwrap();
    let chunks = codex_failure(&home).await;
    let Some(StreamChunk::Finish {
        reason: seekdeep_llm::FinishReason::Error { failure },
        ..
    }) = chunks.last()
    else {
        panic!("expected missing credential finish: {chunks:#?}")
    };
    assert_eq!(failure.code, "MISSING_CREDENTIAL");
    assert!(failure.message.contains("run codex login"));
    assert!(
        failure
            .message
            .contains("cli_auth_credentials_store = \"file\"")
    );
}

#[tokio::test]
async fn malformed_shared_session_is_invalid_credential_before_network() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("auth.json");
    tokio::fs::write(&path, b"{broken").await.unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .await
            .unwrap();
    }
    let chunks = codex_failure(&home).await;
    let Some(StreamChunk::Finish {
        reason: seekdeep_llm::FinishReason::Error { failure },
        ..
    }) = chunks.last()
    else {
        panic!("expected invalid credential finish: {chunks:#?}")
    };
    assert_eq!(failure.code, "INVALID_CREDENTIAL");
    assert!(
        failure
            .message
            .contains("cannot use the ChatGPT OAuth session at $CODEX_HOME/auth.json")
    );
}
