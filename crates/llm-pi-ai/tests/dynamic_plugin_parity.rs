//! Dynamic settings, credential, collision, and registration-swap parity tests.

use std::{path::Path, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::{TryStreamExt as _, stream};
use parking_lot::Mutex;
use seekdeep_cordis::{Context, Fiber};
use seekdeep_credentials::{CREDENTIALS, credential_ref};
use seekdeep_credentials_local::{LocalCredentialConfig, install as install_credentials};
use seekdeep_llm::{
    AdapterStream, FinishReason, GenerateOptions, LlmAdapter, LlmRuntime, ModelId, ProviderId,
    StreamChunk,
};
use seekdeep_llm_pi_ai::plugin::{NAME, plugin};
use seekdeep_settings::{
    SETTINGS, SettingsDocument, SettingsService, SettingsStorage, settings_namespace,
};
use serde_json::{Map, Value, json};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    task::JoinHandle,
};

struct MemorySettingsStorage {
    document: Mutex<SettingsDocument>,
}

impl MemorySettingsStorage {
    fn new(document: Value) -> Arc<Self> {
        let Value::Object(document) = document else {
            panic!("settings document")
        };
        Arc::new(Self {
            document: Mutex::new(document),
        })
    }
}

#[async_trait]
impl SettingsStorage for MemorySettingsStorage {
    fn writable(&self) -> bool {
        true
    }

    fn document_path(&self) -> Option<&Path> {
        None
    }

    async fn load(&self) -> anyhow::Result<SettingsDocument> {
        Ok(self.document.lock().clone())
    }

    async fn persist(
        &self,
        namespace: &seekdeep_settings::SettingsNamespace,
        section: &Map<String, Value>,
    ) -> anyhow::Result<()> {
        self.document
            .lock()
            .insert(namespace.to_string(), Value::Object(section.clone()));
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct CapturedRequest {
    path: String,
    headers: String,
}

struct MockServer {
    url: String,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    task: JoinHandle<()>,
}

impl MockServer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let task_requests = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let requests = task_requests.clone();
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let header_end = loop {
                        let mut buffer = [0_u8; 4096];
                        let Ok(read) = socket.read(&mut buffer).await else {
                            return;
                        };
                        if read == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&buffer[..read]);
                        if let Some(index) =
                            bytes.windows(4).position(|window| window == b"\r\n\r\n")
                        {
                            break index + 4;
                        }
                    };
                    let headers = String::from_utf8_lossy(&bytes[..header_end]).into_owned();
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or_default();
                    while bytes.len() - header_end < length {
                        let mut buffer = vec![0_u8; length - (bytes.len() - header_end)];
                        let Ok(read) = socket.read(&mut buffer).await else {
                            return;
                        };
                        if read == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&buffer[..read]);
                    }
                    let path = headers
                        .lines()
                        .next()
                        .and_then(|line| line.split_whitespace().nth(1))
                        .unwrap_or_default()
                        .to_owned();
                    requests.lock().push(CapturedRequest { path, headers });
                    if socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
                        )
                        .await
                        .is_err()
                    {
                        return;
                    }
                    for event in [
                        r#"{"choices":[{"delta":{"role":"assistant","content":""},"index":0,"finish_reason":null}]}"#,
                        r#"{"choices":[{"delta":{"content":"hello"},"index":0,"finish_reason":null}]}"#,
                        r#"{"choices":[{"delta":{},"index":0,"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1}}"#,
                        "[DONE]",
                    ] {
                        if socket
                            .write_all(format!("data: {event}\n\n").as_bytes())
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                });
            }
        });
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

struct Harness {
    context: Context,
    runtime: Arc<LlmRuntime>,
    settings_fiber: Arc<Fiber>,
    _home: tempfile::TempDir,
}

impl Harness {
    async fn boot(config: Value, credentials: &[(&str, &str)]) -> Self {
        let home = tempfile::tempdir().unwrap();
        let context = Context::new();
        let runtime = LlmRuntime::install(&context).unwrap();
        let settings_fiber = Fiber::active_child("settings-provider");
        SettingsService::install(
            &context.with_fiber(settings_fiber.clone()),
            MemorySettingsStorage::new(json!({})),
        )
        .await
        .unwrap();
        let credential_fiber = install_credentials(
            &context,
            LocalCredentialConfig {
                path: Some(home.path().join(".credentials.yaml")),
                seekdeep_home: None,
                watch: false,
                debounce_ms: 0.0,
            },
        )
        .unwrap();
        credential_fiber.await_settled().await.unwrap();
        let credential_service = context.get(CREDENTIALS).unwrap();
        for (reference, value) in credentials {
            credential_service
                .set(&credential_ref(*reference).unwrap(), value)
                .await
                .unwrap();
        }
        let provider_fiber = context.plugin(plugin(), config).unwrap();
        provider_fiber.await_settled().await.unwrap();
        Self {
            context,
            runtime,
            settings_fiber,
            _home: home,
        }
    }

    fn settings(&self) -> Arc<SettingsService> {
        self.context.get(SETTINGS).unwrap()
    }

    async fn close(self) {
        self.context.fiber().dispose().await.unwrap();
        self.settings_fiber.dispose().await.unwrap();
    }
}

#[derive(Debug)]
struct StubAdapter;

#[async_trait]
impl LlmAdapter for StubAdapter {
    fn stream(&self, _options: GenerateOptions) -> AdapterStream {
        AdapterStream::new(stream::empty())
    }
}

fn request(provider: &str, model: &str) -> GenerateOptions {
    GenerateOptions::new(ProviderId::new(provider), ModelId::new(model), vec![])
}

async fn chunks(runtime: &Arc<LlmRuntime>, provider: &str, model: &str) -> Vec<StreamChunk> {
    runtime
        .stream(request(provider, model))
        .try_collect()
        .await
        .unwrap()
}

async fn wait_until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

fn authorization(request: &CapturedRequest) -> Option<&str> {
    request.headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("authorization")
            .then_some(value.trim())
    })
}

#[tokio::test]
async fn dormant_mount_registers_settings_route_immediately_and_reset_removes_it() {
    let server = MockServer::start().await;
    let harness = Harness::boot(json!({}), &[("PI_DYNAMIC_KEY", "pk-from-settings")]).await;
    assert!(harness.runtime.list_providers().is_empty());
    let directory = harness.runtime.list_configurable_providers();
    assert!(directory.len() > 30);
    let openai = directory
        .iter()
        .find(|entry| entry.provider.as_str() == "openai")
        .unwrap();
    assert_eq!(openai.settings_ns, NAME);
    assert_eq!(openai.settings_path, ["providers", "openai"]);
    assert_eq!(openai.declared, Some(false));

    harness
        .settings()
        .update(
            &settings_namespace(NAME).unwrap(),
            json!({"providers":{"deepseek":{
                "apiKeyEnv":"PI_DYNAMIC_KEY","baseURL":server.url
            }}}),
            None,
        )
        .await
        .unwrap();
    wait_until(|| !harness.runtime.list_providers().is_empty()).await;
    let output = chunks(&harness.runtime, "deepseek", "deepseek-v4-flash").await;
    assert!(
        output.iter().any(|chunk| {
            matches!(chunk, StreamChunk::TextDelta { text, .. } if text == "hello")
        })
    );
    assert_eq!(
        authorization(&server.requests()[0]),
        Some("Bearer pk-from-settings")
    );

    harness
        .settings()
        .replace(&settings_namespace(NAME).unwrap(), json!({}), None)
        .await
        .unwrap();
    wait_until(|| harness.runtime.list_providers().is_empty()).await;
    harness.close().await;
}

#[tokio::test]
async fn settings_route_is_added_beside_static_route_and_reset_only_drops_user_layer() {
    let server = MockServer::start().await;
    let harness = Harness::boot(
        json!({"providers":{"openai":{}}}),
        &[("PI_LIVE_KEY", "live-key")],
    )
    .await;
    harness
        .settings()
        .update(
            &settings_namespace(NAME).unwrap(),
            json!({"providers":{"deepseek":{
                "apiKeyEnv":"PI_LIVE_KEY","baseURL":server.url
            }}}),
            None,
        )
        .await
        .unwrap();
    wait_until(|| harness.runtime.list_providers().len() == 2).await;
    assert_eq!(
        harness
            .runtime
            .list_providers()
            .iter()
            .map(|provider| provider.id.as_str())
            .collect::<Vec<_>>(),
        ["openai", "deepseek"]
    );
    chunks(&harness.runtime, "deepseek", "deepseek-v4-flash").await;
    assert_eq!(
        authorization(&server.requests()[0]),
        Some("Bearer live-key")
    );

    harness
        .settings()
        .replace(&settings_namespace(NAME).unwrap(), json!({}), None)
        .await
        .unwrap();
    wait_until(|| harness.runtime.list_providers().len() == 1).await;
    assert_eq!(harness.runtime.list_providers()[0].id.as_str(), "openai");
    let removed = chunks(&harness.runtime, "deepseek", "deepseek-v4-flash").await;
    assert!(matches!(
        removed.last(),
        Some(StreamChunk::Finish {
            reason: FinishReason::Error { failure },
            ..
        }) if failure.code == "NO_ADAPTER"
    ));
    harness.close().await;
}

#[tokio::test]
async fn per_request_credential_reference_rotates_without_rebuilding_the_route() {
    let server = MockServer::start().await;
    let harness = Harness::boot(
        json!({"providers":{"deepseek":{
            "apiKeyEnv":"PI_DYNAMIC_KEY","baseURL":server.url
        }}}),
        &[("PI_DYNAMIC_KEY", "pk-one")],
    )
    .await;
    chunks(&harness.runtime, "deepseek", "deepseek-v4-flash").await;
    harness
        .context
        .get(CREDENTIALS)
        .unwrap()
        .set(&credential_ref("PI_DYNAMIC_KEY").unwrap(), "pk-two")
        .await
        .unwrap();
    chunks(&harness.runtime, "deepseek", "deepseek-v4-flash").await;
    let requests = server.requests();
    assert_eq!(authorization(&requests[0]), Some("Bearer pk-one"));
    assert_eq!(authorization(&requests[1]), Some("Bearer pk-two"));
    harness.close().await;
}

#[tokio::test]
async fn captured_retry_policy_changes_in_place_without_reordering_routes() {
    let harness = Harness::boot(json!({"providers":{"openai":{}}}), &[]).await;
    harness
        .settings()
        .update(
            &settings_namespace(NAME).unwrap(),
            json!({"providers":{"openai":{"retryPolicy":{
                "mode":"always","backoff":{
                    "initialDelayMs":25,"maxDelayMs":100,"jitterRatio":0.2
                }
            }}}}),
            None,
        )
        .await
        .unwrap();
    wait_until(|| {
        harness
            .runtime
            .provider_retry_policy("openai")
            .is_ok_and(|policy| policy.mode() == seekdeep_llm::RetryPolicyMode::Always)
    })
    .await;
    let policy = harness.runtime.provider_retry_policy("openai").unwrap();
    assert_eq!(policy.mode(), seekdeep_llm::RetryPolicyMode::Always);
    assert_eq!(policy.initial_delay_ms().to_bits(), 25.0_f64.to_bits());
    assert_eq!(policy.max_delay_ms().to_bits(), 100.0_f64.to_bits());
    assert_eq!(policy.jitter_ratio().to_bits(), 0.2_f64.to_bits());
    assert_eq!(harness.runtime.list_providers()[0].id.as_str(), "openai");
    harness.close().await;
}

#[tokio::test]
async fn unserviceable_settings_write_is_refused_and_keeps_previous_routes() {
    let harness = Harness::boot(json!({"providers":{"openai":{}}}), &[]).await;
    let error = harness
        .settings()
        .update(
            &settings_namespace(NAME).unwrap(),
            json!({"providers":{"not-a-real-provider":{}}}),
            None,
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("resolves no models"),
        "{error:#}"
    );
    assert_eq!(harness.runtime.list_providers()[0].id.as_str(), "openai");
    harness.close().await;
}

#[tokio::test]
async fn route_collision_keeps_previous_registration_and_recovery_reuses_it() {
    let server = MockServer::start().await;
    let harness = Harness::boot(
        json!({"providers":{"openai":{
            "apiKeyEnv":"PI_LIVE_KEY","baseURL":format!("{}/v1", server.url)
        }}}),
        &[("PI_LIVE_KEY", "live-key"), ("PI_OTHER_KEY", "other")],
    )
    .await;
    let _foreign = harness
        .runtime
        .register_adapter(&["anthropic".to_owned()], Arc::new(StubAdapter))
        .unwrap();
    harness
        .settings()
        .update(
            &settings_namespace(NAME).unwrap(),
            json!({"providers":{
                "openai":{"apiKeyEnv":"PI_LIVE_KEY","baseURL":format!("{}/v1", server.url)},
                "anthropic":{"apiKeyEnv":"PI_OTHER_KEY"}
            }}),
            None,
        )
        .await
        .unwrap();
    let mut providers = harness
        .runtime
        .list_providers()
        .iter()
        .map(|provider| provider.id.to_string())
        .collect::<Vec<_>>();
    providers.sort();
    assert_eq!(providers, ["anthropic", "openai"]);
    let first = chunks(&harness.runtime, "openai", "gpt-4.1").await;
    assert!(matches!(
        first.last(),
        Some(StreamChunk::Finish {
            reason: FinishReason::Error { .. },
            ..
        })
    ));
    assert_eq!(server.requests()[0].path, "/v1/responses");

    harness
        .settings()
        .replace(&settings_namespace(NAME).unwrap(), json!({}), None)
        .await
        .unwrap();
    chunks(&harness.runtime, "openai", "gpt-4.1").await;
    assert_eq!(
        server
            .requests()
            .iter()
            .map(|request| request.path.as_str())
            .collect::<Vec<_>>(),
        ["/v1/responses", "/v1/responses"]
    );
    harness.close().await;
}

#[tokio::test]
async fn provider_key_reordering_does_not_change_registered_route_order() {
    let harness = Harness::boot(json!({"providers":{"openai":{},"anthropic":{}}}), &[]).await;
    let before = harness
        .runtime
        .list_providers()
        .iter()
        .map(|provider| provider.id.to_string())
        .collect::<Vec<_>>();
    harness
        .settings()
        .update(
            &settings_namespace(NAME).unwrap(),
            json!({"providers":{"anthropic":{},"openai":{}}}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        harness
            .runtime
            .list_providers()
            .iter()
            .map(|provider| provider.id.to_string())
            .collect::<Vec<_>>(),
        before
    );
    harness.close().await;
}
