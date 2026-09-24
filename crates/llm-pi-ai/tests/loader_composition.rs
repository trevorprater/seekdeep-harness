//! Real loader composition for LLM, settings, credentials, and `llm-pi-ai`.

mod support;

use std::{collections::BTreeMap, fmt::Write as _, path::Path, sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures::TryStreamExt as _;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, Plugin};
use seekdeep_credentials_local::plugin as credentials_plugin;
use seekdeep_llm::{FinishReason, GenerateOptions, LlmRuntime, ModelId, ProviderId, StreamChunk};
use seekdeep_llm_pi_ai::plugin::plugin as pi_ai_plugin;
use seekdeep_loader::{LoadedComposition, PluginCatalog};
use seekdeep_settings_file::plugin as settings_plugin;
use seekdeep_util::launch_environment::{
    LaunchEnvironmentLayerInput, LaunchEnvironmentSource, SEEKDEEP_LAUNCH_ENVIRONMENT,
    create_launch_environment_snapshot,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
};

#[derive(Clone, Debug)]
struct CapturedRequest {
    path: String,
    headers: BTreeMap<String, String>,
}

struct MockServer {
    url: String,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    task: tokio::task::AbortHandle,
}

impl MockServer {
    async fn start() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let capture = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let capture = capture.clone();
                tokio::spawn(async move {
                    let _ = respond(stream, capture).await;
                });
            }
        })
        .abort_handle();
        Self {
            url: format!("http://{address}"),
            requests,
            task,
        }
    }

    fn requests(&self) -> Vec<CapturedRequest> {
        self.requests.lock().clone()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn respond(
    mut stream: TcpStream,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
) -> anyhow::Result<()> {
    let request = read_request(&mut stream).await?;
    let responses = request.path.ends_with("/responses");
    let anthropic = request.path.ends_with("/v1/messages");
    requests.lock().push(request);
    let body = if responses {
        codex_events()
    } else if anthropic {
        anthropic_events()
    } else {
        concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1}}\n\n",
            "data: [DONE]\n\n",
        )
        .to_owned()
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    Ok(())
}

fn anthropic_events() -> String {
    [
        json!({"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":1,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}),
        json!({"type":"message_stop"}),
    ]
    .into_iter()
    .fold(String::new(), |mut output, event| {
        writeln!(output, "event: {}", event["type"].as_str().unwrap()).unwrap();
        writeln!(output, "data: {event}\n").unwrap();
        output
    })
}

fn codex_events() -> String {
    let part = json!({"type":"output_text","annotations":[],"text":"hello"});
    let message = json!({
        "id":"msg_fixture","type":"message","status":"completed",
        "role":"assistant","content":[part]
    });
    let completed = json!({
        "id":"resp_fixture","status":"completed","model":"gpt-5.3-codex-spark",
        "output":[message.clone()],
        "usage":{"input_tokens":1,"input_tokens_details":{"cached_tokens":0},
            "output_tokens":1,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":2}
    });
    [
        json!({"type":"response.created","response":{"id":"resp_fixture","status":"in_progress"}}),
        json!({"type":"response.output_item.added","output_index":0,"item":{
            "id":"msg_fixture","type":"message","status":"in_progress","role":"assistant","content":[]
        }}),
        json!({"type":"response.output_text.delta","output_index":0,"delta":"hello"}),
        json!({"type":"response.output_item.done","output_index":0,"item":message}),
        json!({"type":"response.completed","response":completed}),
    ]
    .into_iter()
    .fold(String::new(), |mut output, event| {
        writeln!(output, "data: {event}\n").unwrap();
        output
    })
}

async fn read_request(stream: &mut TcpStream) -> anyhow::Result<CapturedRequest> {
    let mut bytes = Vec::new();
    let boundary = loop {
        let mut chunk = [0_u8; 4096];
        let count = stream.read(&mut chunk).await?;
        anyhow::ensure!(count > 0, "request closed before headers");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(boundary) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break boundary + 4;
        }
    };
    let head = std::str::from_utf8(&bytes[..boundary])?;
    let path = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_owned();
    let mut headers = BTreeMap::new();
    for line in head.split("\r\n").skip(1).filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("malformed request header"))?;
        headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
    }
    let length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or_default();
    while bytes.len() < boundary + length {
        let mut chunk = [0_u8; 4096];
        let count = stream.read(&mut chunk).await?;
        anyhow::ensure!(count > 0, "request closed before body");
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(CapturedRequest { path, headers })
}

struct Composition {
    context: Context,
    loaded: LoadedComposition,
    settings_path: std::path::PathBuf,
    codex_home: std::path::PathBuf,
}

fn llm_plugin() -> Plugin {
    Plugin::new("llm", std::iter::empty::<&str>(), |context, _| {
        Box::pin(async move {
            LlmRuntime::install(&context)?;
            Ok(())
        })
    })
}

fn catalog() -> PluginCatalog {
    let catalog = PluginCatalog::new();
    catalog
        .register_named("test-llm-service", llm_plugin())
        .unwrap();
    catalog
        .register_named("seekdeep-settings-file", settings_plugin())
        .unwrap();
    catalog
        .register_named("seekdeep-credentials-local", credentials_plugin())
        .unwrap();
    catalog
        .register_named("seekdeep-llm-pi-ai", pi_ai_plugin())
        .unwrap();
    catalog
}

async fn write_private(path: &Path, value: impl AsRef<[u8]>) {
    tokio::fs::write(path, value).await.unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .await
            .unwrap();
    }
}

async fn load_composition(home: &Path) -> Composition {
    let settings_path = home.join("settings.yaml");
    let credentials_path = home.join(".credentials.yaml");
    let codex_home = home.join("codex-home");
    tokio::fs::write(&settings_path, "# personal settings\n")
        .await
        .unwrap();
    write_private(&credentials_path, "PI_COMPOSITION_KEY: key-from-store\n").await;
    let mut source = String::from("- id: llm\n  name: test-llm-service\n");
    write!(
        source,
        concat!(
            "- id: settings\n",
            "  name: seekdeep-settings-file\n",
            "  config:\n",
            "    path: {}\n",
            "    debounceMs: 5\n",
            "- id: credentials\n",
            "  name: seekdeep-credentials-local\n",
            "  config:\n",
            "    path: {}\n",
            "    debounceMs: 5\n",
            "- id: llm-pi-ai\n",
            "  name: seekdeep-llm-pi-ai\n",
        ),
        serde_json::to_string(&settings_path).unwrap(),
        serde_json::to_string(&credentials_path).unwrap(),
    )
    .unwrap();
    let context = Context::new();
    context
        .provide(
            SEEKDEEP_LAUNCH_ENVIRONMENT,
            Arc::new(create_launch_environment_snapshot(&[
                LaunchEnvironmentLayerInput {
                    source: LaunchEnvironmentSource::Process,
                    path: None,
                    values: BTreeMap::from([
                        (
                            "SEEKDEEP_HOME".to_owned(),
                            home.to_string_lossy().into_owned(),
                        ),
                        (
                            "CODEX_HOME".to_owned(),
                            codex_home.to_string_lossy().into_owned(),
                        ),
                        ("PI_COMPOSITION_KEY".to_owned(), String::new()),
                        ("OPENAI_API_KEY".to_owned(), "ambient-openai-key".to_owned()),
                        (
                            "ANTHROPIC_AUTH_TOKEN".to_owned(),
                            "ambient-anthropic-token".to_owned(),
                        ),
                    ]),
                },
            ])),
        )
        .unwrap();
    let loaded = catalog().load_yaml(&context, &source).await.unwrap();
    Composition {
        context,
        loaded,
        settings_path,
        codex_home,
    }
}

async fn wait_for_provider(context: &Context, expected: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let providers = context.get(seekdeep_llm::LLM).unwrap().list_providers();
            if providers.len() == 1 && providers[0].id.as_str() == expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

async fn stream(context: &Context, provider: &str, model: &str) -> Vec<StreamChunk> {
    context
        .get(seekdeep_llm::LLM)
        .unwrap()
        .stream(GenerateOptions::new(
            ProviderId::new(provider),
            ModelId::new(model),
            vec![],
        ))
        .try_collect()
        .await
        .unwrap()
}

fn jwt(account: &str) -> String {
    let encode = |value: Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).unwrap());
    format!(
        "{}.{}.x",
        encode(json!({"alg":"none"})),
        encode(json!({
            "exp":4_102_444_800_u64,
            "https://api.openai.com/auth":{"chatgpt_account_id":account}
        }))
    )
}

async fn write_codex_auth(composition: &Composition, value: &[u8]) {
    tokio::fs::create_dir_all(&composition.codex_home)
        .await
        .unwrap();
    write_private(&composition.codex_home.join("auth.json"), value).await;
}

async fn configure(composition: &Composition, yaml: String, provider: &str) {
    tokio::fs::write(&composition.settings_path, yaml)
        .await
        .unwrap();
    wait_for_provider(&composition.context, provider).await;
}

#[tokio::test]
async fn bare_yaml_composition_activates_from_external_settings_and_stored_key() {
    let home = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let composition = load_composition(home.path()).await;
    assert!(
        composition
            .context
            .get(seekdeep_llm::LLM)
            .unwrap()
            .list_providers()
            .is_empty()
    );
    configure(
        &composition,
        format!(
            "llm-pi-ai:\n  providers:\n    deepseek:\n      apiKeyEnv: PI_COMPOSITION_KEY\n      baseURL: {}\n",
            server.url
        ),
        "deepseek",
    )
    .await;
    let result = support::assemble::assemble(
        &composition.context.get(seekdeep_llm::LLM).unwrap(),
        GenerateOptions::new(
            ProviderId::new("deepseek"),
            ModelId::new("deepseek-v4-flash"),
            vec![],
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::to_value(result.message).unwrap()["content"],
        json!([{"type":"text","text":"hello"}])
    );
    assert_eq!(result.finish, FinishReason::Stop);
    assert!(result.usage.is_some());
    assert_eq!(
        server.requests()[0].headers["authorization"],
        "Bearer key-from-store"
    );
    composition.loaded.dispose().await.unwrap();
}

#[tokio::test]
async fn catalog_route_without_reference_uses_provider_native_process_key() {
    let home = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let composition = load_composition(home.path()).await;
    configure(
        &composition,
        format!(
            "llm-pi-ai:\n  providers:\n    openai:\n      baseURL: {}\n",
            server.url
        ),
        "openai",
    )
    .await;
    let chunks = stream(&composition.context, "openai", "gpt-4.1").await;
    assert!(matches!(
        chunks.last(),
        Some(StreamChunk::Finish {
            reason: FinishReason::Stop,
            ..
        })
    ));
    let captured = &server.requests()[0];
    assert_eq!(captured.path, "/responses");
    assert_eq!(
        captured.headers["authorization"],
        "Bearer ambient-openai-key"
    );
    composition.loaded.dispose().await.unwrap();
}

#[tokio::test]
async fn anthropic_native_auth_token_becomes_bearer_header_not_api_key() {
    let home = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let composition = load_composition(home.path()).await;
    configure(
        &composition,
        format!(
            "llm-pi-ai:\n  providers:\n    anthropic:\n      baseURL: {}\n",
            server.url
        ),
        "anthropic",
    )
    .await;
    let chunks = stream(&composition.context, "anthropic", "claude-sonnet-4-5").await;
    assert!(matches!(
        chunks.last(),
        Some(StreamChunk::Finish {
            reason: FinishReason::Stop,
            ..
        })
    ));
    let captured = &server.requests()[0];
    assert_eq!(captured.path, "/v1/messages");
    assert_eq!(
        captured.headers["authorization"],
        "Bearer ambient-anthropic-token"
    );
    assert!(!captured.headers.contains_key("x-api-key"));
    composition.loaded.dispose().await.unwrap();
}

#[tokio::test]
async fn assembled_codex_route_uses_official_file_backed_session() {
    let home = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let composition = load_composition(home.path()).await;
    let access = jwt("account-from-codex");
    write_codex_auth(
        &composition,
        &serde_json::to_vec(&json!({
            "auth_mode":"chatgpt","OPENAI_API_KEY":null,
            "tokens":{"id_token":"id","access_token":access,
                "refresh_token":"refresh","account_id":"account-from-codex"}
        }))
        .unwrap(),
    )
    .await;
    configure(
        &composition,
        format!(
            "llm-pi-ai:\n  providers:\n    openai-codex:\n      baseURL: {}\n      transport: sse\n",
            server.url
        ),
        "openai-codex",
    )
    .await;
    let model = composition
        .context
        .get(seekdeep_llm::LLM)
        .unwrap()
        .list_models("openai-codex")
        .await
        .unwrap()[0]
        .id
        .clone();
    let chunks = stream(&composition.context, "openai-codex", model.as_str()).await;
    assert!(matches!(
        chunks.last(),
        Some(StreamChunk::Finish {
            reason: FinishReason::Stop,
            ..
        })
    ));
    let captured = &server.requests()[0];
    assert_eq!(captured.path, "/codex/responses");
    assert_eq!(
        captured.headers["authorization"],
        format!("Bearer {access}")
    );
    assert_eq!(captured.headers["chatgpt-account-id"], "account-from-codex");
    composition.loaded.dispose().await.unwrap();
}

#[tokio::test]
async fn assembled_codex_route_reports_missing_and_malformed_shared_sessions() {
    for malformed in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let composition = load_composition(home.path()).await;
        if malformed {
            write_codex_auth(&composition, b"{broken").await;
        }
        configure(
            &composition,
            "llm-pi-ai:\n  providers:\n    openai-codex: {}\n".to_owned(),
            "openai-codex",
        )
        .await;
        let model = composition
            .context
            .get(seekdeep_llm::LLM)
            .unwrap()
            .list_models("openai-codex")
            .await
            .unwrap()[0]
            .id
            .clone();
        let chunks = stream(&composition.context, "openai-codex", model.as_str()).await;
        let Some(StreamChunk::Finish {
            reason: FinishReason::Error { failure },
            ..
        }) = chunks.last()
        else {
            panic!("expected credential failure: {chunks:#?}")
        };
        if malformed {
            assert_eq!(failure.code, "INVALID_CREDENTIAL");
            assert!(
                failure
                    .message
                    .contains("cannot use the ChatGPT OAuth session")
            );
        } else {
            assert_eq!(failure.code, "MISSING_CREDENTIAL");
            assert!(failure.message.contains("run codex login"));
        }
        composition.loaded.dispose().await.unwrap();
    }
}
