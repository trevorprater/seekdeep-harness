//! Credentialed mirror of `packages/llm/llm-pi-ai/tests/provider-apis.e2e.ts`: real Azure
//! `OpenAI` Responses and Anthropic Messages requests through the production adapter and
//! runtime path.
//!
//! Every case is `#[ignore]` so the workspace gate never spends a credential, and each
//! self-skips without its provider key exactly as the source's
//! `describe.skipIf(profile.apiKey === undefined)` did. The pi-ai provider workflow proves both
//! keys are present, then runs `scripts/run-e2e-lane.sh crates/llm-pi-ai/tests/provider_apis_e2e.rs`,
//! which includes the ignored tests. Run one locally with
//! `ANTHROPIC_API_KEY=... cargo test -p seekdeep-llm-pi-ai --test provider_apis_e2e -- --ignored`.

mod support;

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use seekdeep_attachment::{
    AttachmentBackend, AttachmentId, AttachmentStore, ImageAttachmentLimits, ImageAttachmentRef,
    ImageMediaType, SaveImageAttachment, StoredImageAttachment,
};
use seekdeep_cordis::Context;
use seekdeep_llm::{
    AbortSignal, AdapterRegistrationHandle, ContentBlock, FinishReason, GenerateOptions,
    LlmRuntime, Message, MessageRole, MessageSource, ModelId, ProviderId, ToolSchema,
};
use seekdeep_llm_pi_ai::{
    adapter::{
        PiAiAdapter, PiAiAdapterOptions, PiApiKeyResolver, PiAttachmentResolver, PiProfileSource,
        PiProtocolExecutor, PiResolvedAuth,
    },
    anthropic_messages::AnthropicMessagesExecutor,
    catalog::builtin_catalog,
    config::{ResolvedPiProviderProfile, resolve_profiles},
    openai_responses::OpenAiResponsesExecutor,
};
use serde_json::{Map, Value, json};
use support::assemble::{AssembledResult, assemble};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    sync::oneshot,
};

/// One provider profile the source e2e drove, with its credential and endpoint overrides.
struct ProviderCase {
    provider: &'static str,
    api: &'static str,
    key_env: &'static str,
    model: String,
    api_key: String,
    base_url: Option<String>,
    headers: Option<Map<String, Value>>,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// The Azure `OpenAI` Responses case: the key travels in the `api-key` header and the bearer
/// header is blanked, as the source configured its `openai` profile against the Azure endpoint.
fn openai_case() -> Option<ProviderCase> {
    let api_key = env("AZURE_OPENAI_API_KEY")?;
    Some(ProviderCase {
        provider: "openai",
        api: "openai-responses",
        key_env: "AZURE_OPENAI_API_KEY",
        model: env("SEEKDEEP_PI_AI_OPENAI_MODEL").unwrap_or_else(|| "gpt-5.5".to_owned()),
        headers: Some(Map::from_iter([
            ("api-key".to_owned(), Value::String(api_key.clone())),
            ("Authorization".to_owned(), Value::String(String::new())),
        ])),
        api_key,
        base_url: env("SEEKDEEP_PI_AI_OPENAI_BASE_URL"),
    })
}

/// Strictly `ANTHROPIC_*`: the `DeepSeek` endpoint does not serve the anthropic-messages
/// protocol, so falling back to the `DeepSeek` key would turn the keyless skip into a 404.
fn anthropic_case() -> Option<ProviderCase> {
    Some(ProviderCase {
        provider: "anthropic",
        api: "anthropic-messages",
        key_env: "ANTHROPIC_API_KEY",
        model: env("SEEKDEEP_PI_AI_ANTHROPIC_MODEL")
            .unwrap_or_else(|| "claude-opus-4-8".to_owned()),
        api_key: env("ANTHROPIC_API_KEY")?,
        base_url: env("SEEKDEEP_PI_AI_ANTHROPIC_BASE_URL"),
        headers: None,
    })
}

struct Profiles(Arc<IndexMap<String, ResolvedPiProviderProfile>>);

impl PiProfileSource for Profiles {
    fn profiles(&self) -> Arc<IndexMap<String, ResolvedPiProviderProfile>> {
        self.0.clone()
    }
}

struct Key(String);

#[async_trait]
impl PiApiKeyResolver for Key {
    async fn resolve(
        &self,
        _: &ProviderId,
        _: &ResolvedPiProviderProfile,
    ) -> anyhow::Result<PiResolvedAuth> {
        Ok(PiResolvedAuth::api_key(Some(self.0.clone())))
    }
}

struct Attachments(AttachmentStore);

impl PiAttachmentResolver for Attachments {
    fn resolve(&self) -> Option<AttachmentStore> {
        Some(self.0.clone())
    }
}

/// The source's read-only e2e attachment store: it serves exactly one fixture and refuses
/// every write.
struct FixtureBackend {
    fixture: StoredImageAttachment,
    limits: ImageAttachmentLimits,
}

#[async_trait]
impl AttachmentBackend for FixtureBackend {
    fn image_limits(&self) -> &ImageAttachmentLimits {
        &self.limits
    }

    async fn validate_image(&self, _input: &SaveImageAttachment) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("e2e attachment fixture is read-only"))
    }

    async fn save_image(&self, _input: SaveImageAttachment) -> anyhow::Result<ImageAttachmentRef> {
        Err(anyhow::anyhow!("e2e attachment fixture is read-only"))
    }

    async fn read_image(
        &self,
        reference: &ImageAttachmentRef,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<StoredImageAttachment> {
        anyhow::ensure!(
            reference.attachment_id == self.fixture.reference.attachment_id,
            "unknown e2e attachment fixture"
        );
        Ok(self.fixture.clone())
    }
}

fn fixture_store(fixture: StoredImageAttachment) -> AttachmentStore {
    let bytes = u64::try_from(fixture.data.len()).expect("fixture size");
    let limits = ImageAttachmentLimits {
        max_image_bytes: bytes,
        max_images_per_message: 1,
        max_message_image_bytes: bytes,
        max_image_pixels: fixture.reference.width * fixture.reference.height,
        media_types: vec![fixture.reference.media_type],
    };
    AttachmentStore::new(Arc::new(FixtureBackend { fixture, limits }))
}

/// A runtime with the case's adapter registered: the production stream path the source drove
/// through `ctx.llm`.
struct Harness {
    _context: Context,
    runtime: Arc<LlmRuntime>,
    _registration: AdapterRegistrationHandle,
}

fn harness(case: &ProviderCase, image: Option<StoredImageAttachment>) -> Harness {
    let mut profile = json!({"apiKeyEnv": case.key_env, "models": [{"id": case.model}]});
    if let Some(base_url) = &case.base_url {
        profile["baseURL"] = json!(base_url);
    }
    if let Some(headers) = &case.headers {
        profile["headers"] = Value::Object(headers.clone());
    }
    let mut raw = Map::new();
    raw.insert(case.provider.to_owned(), profile);
    let profiles =
        Arc::new(resolve_profiles(Some(&Value::Object(raw)), builtin_catalog()).unwrap());
    let executor: Arc<dyn PiProtocolExecutor> = if case.api == "anthropic-messages" {
        Arc::new(AnthropicMessagesExecutor::new(reqwest::Client::new()))
    } else {
        Arc::new(OpenAiResponsesExecutor::new(reqwest::Client::new()))
    };
    let attachments = image.map(|fixture| {
        Arc::new(Attachments(fixture_store(fixture))) as Arc<dyn PiAttachmentResolver>
    });
    let adapter = PiAiAdapter::new(PiAiAdapterOptions {
        profiles: Arc::new(Profiles(profiles)),
        api_keys: Arc::new(Key(case.api_key.clone())),
        executor,
        attachments,
    });
    let context = Context::new();
    let runtime = LlmRuntime::install(&context).unwrap();
    let registration = runtime
        .register_adapter(&[case.provider.to_owned()], Arc::new(adapter))
        .unwrap();
    Harness {
        _context: context,
        runtime,
        _registration: registration,
    }
}

fn ask(text: &str) -> Vec<Message> {
    vec![Message::new(
        MessageRole::User,
        vec![ContentBlock::Text { text: text.into() }],
        MessageSource::plugin("test"),
    )]
}

fn options(
    case: &ProviderCase,
    messages: Vec<Message>,
    max_tokens: u64,
    tools: Option<Vec<ToolSchema>>,
) -> GenerateOptions {
    let mut options = GenerateOptions::new(
        ProviderId::new(case.provider),
        ModelId::new(&case.model),
        messages,
    );
    options.max_tokens = Some(max_tokens);
    options.tools = tools;
    options
}

fn text_of(result: &AssembledResult) -> String {
    result
        .message
        .content()
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => text.as_str().map(str::to_owned),
            _ => None,
        })
        .collect()
}

fn expect_finish(result: &AssembledResult, expected: &FinishReason) {
    if let FinishReason::Error { failure } = &result.finish {
        panic!(
            "provider request failed ({}): {}",
            failure.code, failure.message
        );
    }
    assert_eq!(&result.finish, expected);
}

fn expect_native_replay(result: &AssembledResult, case: &ProviderCase) -> Value {
    let source = result.message.source();
    assert_eq!(source.kind, "model");
    let replay = source
        .fields
        .get("replayState")
        .and_then(|value: &seekdeep_lossless_json::JsonValue| value.as_serde_json().cloned())
        .expect("native replay metadata");
    assert_eq!(replay["kind"], "pi-ai");
    assert_eq!(replay["version"], 1);
    assert_eq!(replay["api"], case.api);
    assert_eq!(replay["provider"], case.provider);
    assert_eq!(replay["model"], case.model.as_str());
    replay
}

fn lookup_tool() -> ToolSchema {
    ToolSchema {
        name: "lookup_code".to_owned(),
        description: "Look up the word represented by a short code.".to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {"code": {"type": "string", "description": "The code to look up."}},
            "required": ["code"],
        })
        .as_object()
        .expect("object schema")
        .clone(),
    }
}

async fn streams_text_with_usage_and_native_replay_metadata(case: ProviderCase) {
    let harness = harness(&case, None);
    let result = assemble(
        &harness.runtime,
        options(&case, ask("Reply with exactly the word: pong"), 1024, None),
    )
    .await
    .unwrap();
    expect_finish(&result, &FinishReason::Stop);
    assert!(text_of(&result).to_lowercase().contains("pong"));
    let usage = result.usage.as_ref().expect("usage");
    assert!(usage.input_tokens > 0);
    assert!(usage.output_tokens > 0);
    assert_eq!(expect_native_replay(&result, &case)["stopReason"], "stop");
}

async fn round_trips_a_tool_call_with_provider_native_replay_metadata(case: ProviderCase) {
    let harness = harness(&case, None);
    let prompt = ask("Use lookup_code with code \"blue\". Do not answer without calling the tool.");
    let first = assemble(
        &harness.runtime,
        options(&case, prompt.clone(), 2048, Some(vec![lookup_tool()])),
    )
    .await
    .unwrap();
    expect_finish(&first, &FinishReason::ToolCalls);
    let (call_id, arguments) = first
        .message
        .content()
        .iter()
        .find_map(|block| match block {
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } if name == "lookup_code" => Some((id.clone(), arguments.clone())),
            _ => None,
        })
        .expect("a lookup_code call");
    let parsed: Value = serde_json::from_str(&arguments).unwrap();
    assert_eq!(parsed["code"], "blue");
    assert_eq!(expect_native_replay(&first, &case)["stopReason"], "toolUse");

    let mut messages = prompt;
    messages.push(first.message.clone());
    messages.push(Message::new(
        MessageRole::User,
        vec![ContentBlock::ToolResult {
            tool_call_id: call_id,
            content: vec![ContentBlock::Text {
                text: "The code blue means ocean.".into(),
            }],
            is_error: None,
        }],
        MessageSource::plugin("test"),
    ));
    let second = assemble(
        &harness.runtime,
        options(&case, messages, 2048, Some(vec![lookup_tool()])),
    )
    .await
    .unwrap();
    expect_finish(&second, &FinishReason::Stop);
    assert!(text_of(&second).to_lowercase().contains("ocean"));
    assert_eq!(expect_native_replay(&second, &case)["stopReason"], "stop");
}

#[tokio::test]
#[ignore = "spends Azure OpenAI credit; the pi-ai provider workflow runs it with the key"]
async fn openai_streams_text_with_usage_and_native_replay_metadata() {
    let Some(case) = openai_case() else {
        eprintln!("skipping: AZURE_OPENAI_API_KEY is not set");
        return;
    };
    streams_text_with_usage_and_native_replay_metadata(case).await;
}

#[tokio::test]
#[ignore = "spends Azure OpenAI credit; the pi-ai provider workflow runs it with the key"]
async fn openai_round_trips_a_tool_call_with_provider_native_replay_metadata() {
    let Some(case) = openai_case() else {
        eprintln!("skipping: AZURE_OPENAI_API_KEY is not set");
        return;
    };
    round_trips_a_tool_call_with_provider_native_replay_metadata(case).await;
}

#[tokio::test]
#[ignore = "spends Anthropic credit; the pi-ai provider workflow runs it with the key"]
async fn anthropic_streams_text_with_usage_and_native_replay_metadata() {
    let Some(case) = anthropic_case() else {
        eprintln!("skipping: ANTHROPIC_API_KEY is not set");
        return;
    };
    streams_text_with_usage_and_native_replay_metadata(case).await;
}

#[tokio::test]
#[ignore = "spends Anthropic credit; the pi-ai provider workflow runs it with the key"]
async fn anthropic_round_trips_a_tool_call_with_provider_native_replay_metadata() {
    let Some(case) = anthropic_case() else {
        eprintln!("skipping: ANTHROPIC_API_KEY is not set");
        return;
    };
    round_trips_a_tool_call_with_provider_native_replay_metadata(case).await;
}

#[tokio::test]
#[ignore = "spends Anthropic credit; the pi-ai provider workflow runs it with the key"]
async fn anthropic_sends_a_real_image_through_the_authenticated_visual_path() {
    let Some(case) = anthropic_case() else {
        eprintln!("skipping: ANTHROPIC_API_KEY is not set");
        return;
    };
    let data = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/community-wecom-survey.png"
    ))
    .unwrap();
    let reference = ImageAttachmentRef {
        attachment_id: AttachmentId::new(format!("sha256:{}", "a".repeat(64))),
        media_type: ImageMediaType::Png,
        bytes: u64::try_from(data.len()).unwrap(),
        width: 256,
        height: 256,
        name: Some("qr-code.png".to_owned()),
    };
    let fixture = StoredImageAttachment {
        reference: reference.clone(),
        data,
    };
    let harness = harness(&case, Some(fixture));
    let message = Message::new(
        MessageRole::User,
        vec![
            ContentBlock::Text {
                text: "What type of machine-readable symbol is shown in the attached image? Reply with exactly: QR code".into(),
            },
            ContentBlock::Image {
                attachment: reference,
            },
        ],
        MessageSource::plugin("test"),
    );
    let result = assemble(&harness.runtime, options(&case, vec![message], 256, None))
        .await
        .unwrap();
    expect_finish(&result, &FinishReason::Stop);
    assert!(text_of(&result).to_lowercase().contains("qr code"));
}

/// A loopback Responses endpoint that records the request head and answers one text turn.
async fn responses_server() -> (String, oneshot::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let head_end = loop {
            let mut buffer = [0_u8; 4096];
            let read = socket.read(&mut buffer).await.unwrap();
            bytes.extend_from_slice(&buffer[..read]);
            if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let head = String::from_utf8_lossy(&bytes[..head_end]).into_owned();
        let length = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        while bytes.len() - head_end < length {
            let mut buffer = vec![0_u8; length - (bytes.len() - head_end)];
            let read = socket.read(&mut buffer).await.unwrap();
            bytes.extend_from_slice(&buffer[..read]);
        }
        let _ = sender.send(head);
        let message = json!({
            "id": "msg_1", "type": "message", "status": "completed", "role": "assistant",
            "content": [{"type": "output_text", "annotations": [], "text": "hello"}]
        });
        let events = [
            json!({"type": "response.created", "response": {"id": "resp_1", "status": "in_progress"}}),
            json!({"type": "response.output_item.added", "output_index": 0, "item": {
                "id": "msg_1", "type": "message", "status": "in_progress", "role": "assistant", "content": []
            }}),
            json!({"type": "response.output_text.delta", "output_index": 0, "delta": "hello"}),
            json!({"type": "response.output_item.done", "output_index": 0, "item": message.clone()}),
            json!({"type": "response.completed", "response": {
                "id": "resp_1", "status": "completed", "model": "gpt-5.5", "output": [message],
                "usage": {"input_tokens": 1, "input_tokens_details": {"cached_tokens": 0},
                          "output_tokens": 1, "output_tokens_details": {"reasoning_tokens": 0},
                          "total_tokens": 2}
            }}),
        ];
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        for event in events {
            socket
                .write_all(format!("data: {event}\n\n").as_bytes())
                .await
                .unwrap();
        }
    });
    (format!("http://{address}"), receiver)
}

/// The source's `openai` profile reached Azure by sending the key in the `api-key` header and
/// blanking `Authorization`; the port's executor must put exactly that on the wire, keylessly
/// proven against a loopback endpoint.
#[tokio::test]
async fn the_azure_profile_sends_its_key_header_and_blanks_the_bearer_header() {
    let (url, captured) = responses_server().await;
    let case = ProviderCase {
        provider: "openai",
        api: "openai-responses",
        key_env: "AZURE_OPENAI_API_KEY",
        model: "gpt-5.5".to_owned(),
        api_key: "azure-secret".to_owned(),
        base_url: Some(url),
        headers: Some(Map::from_iter([
            (
                "api-key".to_owned(),
                Value::String("azure-secret".to_owned()),
            ),
            ("Authorization".to_owned(), Value::String(String::new())),
        ])),
    };
    let harness = harness(&case, None);
    let result = assemble(&harness.runtime, options(&case, ask("ping"), 64, None))
        .await
        .unwrap();
    expect_finish(&result, &FinishReason::Stop);
    assert_eq!(text_of(&result), "hello");
    assert_eq!(expect_native_replay(&result, &case)["stopReason"], "stop");
    let head = captured.await.unwrap().to_ascii_lowercase();
    assert!(head.starts_with("post /responses"), "{head}");
    assert!(head.contains("api-key: azure-secret"), "{head}");
    let bearer = head
        .lines()
        .find_map(|line| line.strip_prefix("authorization:"))
        .map(str::trim);
    assert!(
        bearer.is_none_or(str::is_empty),
        "the bearer header must be blank, got {bearer:?}"
    );
}
