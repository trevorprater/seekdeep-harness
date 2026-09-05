//! Snapshot, metadata, validation, and executor-boundary adapter parity tests.

use std::sync::{
    Arc, RwLock,
    atomic::{AtomicU8, AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::{StreamExt, TryStreamExt, stream};
use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_attachment::{
    AttachmentBackend, AttachmentId, AttachmentStore, ImageAttachmentLimits, ImageAttachmentRef,
    ImageMediaType, SaveImageAttachment, StoredImageAttachment,
};
use seekdeep_llm::{
    AdapterRejection, ContentBlock, GenerateOptions, LlmAdapter, LlmError, ModelId, ProviderId,
    ReasoningEffortId, StreamChunk, user_agent,
};
use seekdeep_llm_pi_ai::{
    adapter::{
        BoxPiEventStream, PiAiAdapter, PiAiAdapterOptions, PiApiKeyResolver, PiAttachmentResolver,
        PiExecutionRequest, PiProfileSource, PiProtocolExecutor, PiResolvedAuth,
    },
    catalog::builtin_catalog,
    config::{ResolvedPiProviderProfile, resolve_profiles},
    replay::{
        PiApi, PiAssistantBlock, PiAssistantMessage, PiAssistantRole, PiCost, PiStopReason, PiUsage,
    },
    stream::PiAssistantEvent,
};
use serde_json::json;

struct BytesBackend;

#[async_trait]
impl AttachmentBackend for BytesBackend {
    fn image_limits(&self) -> &ImageAttachmentLimits {
        static LIMITS: std::sync::LazyLock<ImageAttachmentLimits> =
            std::sync::LazyLock::new(|| ImageAttachmentLimits {
                max_image_bytes: 10,
                max_images_per_message: 10,
                max_message_image_bytes: 100,
                max_image_pixels: 100,
                media_types: vec![ImageMediaType::Png],
            });
        &LIMITS
    }

    async fn validate_image(&self, _: &SaveImageAttachment) -> anyhow::Result<()> {
        Ok(())
    }

    async fn save_image(&self, _: SaveImageAttachment) -> anyhow::Result<ImageAttachmentRef> {
        unreachable!("adapter test only reads images")
    }

    async fn read_image(
        &self,
        reference: &ImageAttachmentRef,
        _: Option<seekdeep_llm::AbortSignal>,
    ) -> anyhow::Result<StoredImageAttachment> {
        Ok(StoredImageAttachment {
            reference: reference.clone(),
            data: vec![1],
        })
    }
}

struct LateAttachments(Mutex<Option<AttachmentStore>>);

impl PiAttachmentResolver for LateAttachments {
    fn resolve(&self) -> Option<AttachmentStore> {
        self.0.lock().clone()
    }
}

struct Profiles(RwLock<Arc<IndexMap<String, ResolvedPiProviderProfile>>>);

impl PiProfileSource for Profiles {
    fn profiles(&self) -> Arc<IndexMap<String, ResolvedPiProviderProfile>> {
        self.0.read().unwrap().clone()
    }
}

struct Keys {
    calls: AtomicUsize,
    value: Option<String>,
}

#[async_trait]
impl PiApiKeyResolver for Keys {
    async fn resolve(
        &self,
        _provider: &ProviderId,
        _profile: &ResolvedPiProviderProfile,
    ) -> anyhow::Result<PiResolvedAuth> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(PiResolvedAuth::api_key(self.value.clone()))
    }
}

#[derive(Default)]
struct Executor {
    mode: AtomicU8,
    requests: Mutex<Vec<PiExecutionRequest>>,
}

impl PiProtocolExecutor for Executor {
    fn stream(&self, request: PiExecutionRequest) -> anyhow::Result<BoxPiEventStream> {
        let mode = self.mode.load(Ordering::SeqCst);
        let signal = request.options.signal.clone();
        self.requests.lock().push(request);
        if mode == 1 {
            return Ok(Box::pin(stream::once(async move {
                signal.cancelled().await;
                Err(anyhow::anyhow!("native stream cancelled"))
            })));
        }
        if mode == 2 {
            anyhow::bail!("executor setup failed");
        }
        let partial = assistant(vec![], PiStopReason::Stop);
        let mut done = assistant(vec![text("hello")], PiStopReason::Stop);
        done.usage = usage(3, 1);
        Ok(Box::pin(stream::iter(vec![
            Ok(PiAssistantEvent::TextStart {
                content_index: 0,
                partial: partial.clone(),
            }),
            Ok(PiAssistantEvent::TextDelta {
                content_index: 0,
                delta: "hello".to_owned(),
                partial: partial.clone(),
            }),
            Ok(PiAssistantEvent::TextEnd {
                content_index: 0,
                content: "hello".to_owned(),
                partial,
            }),
            Ok(PiAssistantEvent::Done {
                reason: PiStopReason::Stop,
                message: done,
            }),
        ])))
    }
}

fn usage(input: u64, output: u64) -> PiUsage {
    PiUsage {
        input,
        output,
        cache_read: 0,
        cache_write: 0,
        total_tokens: input + output,
        cost: PiCost::default(),
    }
}

fn text(value: &str) -> PiAssistantBlock {
    PiAssistantBlock::Text {
        text: value.to_owned(),
        text_signature: None,
    }
}

fn assistant(content: Vec<PiAssistantBlock>, stop_reason: PiStopReason) -> PiAssistantMessage {
    PiAssistantMessage {
        role: PiAssistantRole::Assistant,
        content,
        api: PiApi::new("openai-completions"),
        provider: ProviderId::new("deepseek"),
        model: ModelId::new("deepseek-v4-flash"),
        response_model: None,
        response_id: None,
        usage: usage(0, 0),
        stop_reason,
        error_message: None,
        timestamp: 0,
    }
}

fn profile_map(base_url: &str) -> Arc<IndexMap<String, ResolvedPiProviderProfile>> {
    let raw = json!({
        "deepseek": {
            "displayName":"DeepSeek Route",
            "apiKeyEnv":"PI_TEST_KEY",
            "baseURL":base_url,
            "headers":{"x-company":"private","User-Agent":"wrong"},
            "reasoning":"max",
            "models":[{"id":"deepseek-v4-flash","maxTokens":77}]
        }
    });
    Arc::new(resolve_profiles(Some(&raw), builtin_catalog()).unwrap())
}

fn adapter(profiles: Arc<Profiles>, keys: Arc<Keys>, executor: Arc<Executor>) -> PiAiAdapter {
    PiAiAdapter::new(PiAiAdapterOptions {
        profiles,
        api_keys: keys,
        executor,
        attachments: None,
    })
}

fn request() -> GenerateOptions {
    GenerateOptions::new(
        ProviderId::new("deepseek"),
        ModelId::new("deepseek-v4-flash"),
        vec![],
    )
}

async fn collect(
    adapter: &PiAiAdapter,
    options: GenerateOptions,
) -> anyhow::Result<Vec<StreamChunk>> {
    adapter
        .stream(options)
        .map(|result| result.map_err(AdapterRejection::into_anyhow))
        .try_collect()
        .await
}

fn error_code(error: &anyhow::Error) -> &str {
    error.downcast_ref::<LlmError>().unwrap().code()
}

#[tokio::test]
async fn metadata_uses_display_catalog_modalities_capacities_and_reasoning() {
    let profiles = Arc::new(Profiles(RwLock::new(profile_map("https://one.test"))));
    let adapter = adapter(
        profiles,
        Arc::new(Keys {
            calls: AtomicUsize::new(0),
            value: Some("key".to_owned()),
        }),
        Arc::new(Executor::default()),
    );
    assert_eq!(adapter.provider_info("deepseek").name, "DeepSeek Route");
    assert_eq!(adapter.provider_info("departed").name, "departed");
    let models = adapter.list_models("deepseek").await.unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].input_modalities.as_ref().unwrap()[0].0, "text");
    let info = adapter
        .resolve_model("deepseek", "deepseek-v4-flash", None)
        .await
        .unwrap();
    assert_eq!(info.context.unwrap().context_window, 1_000_000);
    assert_eq!(info.default_max_tokens, Some(77));
    let reasoning = info.reasoning.unwrap();
    assert_eq!(
        reasoning
            .efforts
            .iter()
            .map(|effort| effort.id.as_str())
            .collect::<Vec<_>>(),
        vec!["off", "high", "max"]
    );
    assert_eq!(reasoning.default_effort.unwrap().as_str(), "max");
}

#[tokio::test]
async fn stream_forwards_frozen_options_headers_and_translates_events() {
    let profiles = Arc::new(Profiles(RwLock::new(profile_map("https://one.test"))));
    let keys = Arc::new(Keys {
        calls: AtomicUsize::new(0),
        value: Some("test-key".to_owned()),
    });
    let executor = Arc::new(Executor::default());
    let adapter = adapter(profiles, keys.clone(), executor.clone());
    let mut options = request();
    options.temperature = Some(0.2);
    options.max_tokens = Some(31);
    options.session_id = Some(seekdeep_llm::SessionId::new("session-for-pi"));
    let chunks = collect(&adapter, options).await.unwrap();
    assert_eq!(keys.calls.load(Ordering::SeqCst), 1);
    assert!(matches!(chunks.last(), Some(StreamChunk::Finish { .. })));
    let requests = executor.requests.lock();
    let execution = &requests[0];
    assert_eq!(execution.options.api_key.as_deref(), Some("test-key"));
    assert_eq!(execution.options.reasoning.unwrap().as_str(), "max");
    assert_eq!(execution.options.temperature, Some(0.2));
    assert_eq!(execution.options.max_tokens, Some(31));
    assert_eq!(execution.options.max_retries, 0);
    assert_eq!(execution.options.headers["x-company"], "private");
    assert_eq!(execution.options.headers["user-agent"], user_agent());
    assert_eq!(execution.model.base_url, "https://one.test");
}

#[tokio::test]
async fn rejects_options_models_efforts_and_images_before_executor_io() {
    let profiles = Arc::new(Profiles(RwLock::new(profile_map("https://one.test"))));
    let executor = Arc::new(Executor::default());
    let adapter = adapter(
        profiles,
        Arc::new(Keys {
            calls: AtomicUsize::new(0),
            value: Some("key".to_owned()),
        }),
        executor.clone(),
    );
    let mut stop = request();
    stop.stop = Some(vec!["END".to_owned()]);
    assert_eq!(
        error_code(&collect(&adapter, stop).await.unwrap_err()),
        "UNSUPPORTED_OPTION"
    );

    let unknown =
        GenerateOptions::new(ProviderId::new("deepseek"), ModelId::new("missing"), vec![]);
    assert_eq!(
        error_code(&collect(&adapter, unknown).await.unwrap_err()),
        "UNKNOWN_MODEL"
    );

    let mut effort = request();
    effort.reasoning_effort = Some(ReasoningEffortId::new("xhigh"));
    assert_eq!(
        error_code(&collect(&adapter, effort).await.unwrap_err()),
        "UNSUPPORTED_REASONING_EFFORT"
    );

    let image: ContentBlock = serde_json::from_value(json!({
        "type":"image",
        "attachment":{
            "attachmentId":format!("sha256:{}", "a".repeat(64)),
            "mediaType":"image/png","bytes":1,"width":1,"height":1
        }
    }))
    .unwrap();
    let image = GenerateOptions::new(
        ProviderId::new("deepseek"),
        ModelId::new("deepseek-v4-flash"),
        vec![seekdeep_llm::Message::new(
            seekdeep_llm::MessageRole::User,
            vec![image],
            seekdeep_llm::MessageSource::plugin("test"),
        )],
    );
    assert_eq!(
        error_code(&collect(&adapter, image).await.unwrap_err()),
        "UNSUPPORTED_CONTENT"
    );
    assert!(executor.requests.lock().is_empty());
}

#[tokio::test]
async fn attachment_service_mounted_after_adapter_is_resolved_for_the_next_image_request() {
    let mut map = (*profile_map("https://one.test")).clone();
    map.get_mut("deepseek").unwrap().pi_provider.models[0]
        .input
        .push(seekdeep_llm_pi_ai::catalog::PiModality::Image);
    let profiles = Arc::new(Profiles(RwLock::new(Arc::new(map))));
    let executor = Arc::new(Executor::default());
    let attachments = Arc::new(LateAttachments(Mutex::new(None)));
    let adapter = PiAiAdapter::new(PiAiAdapterOptions {
        profiles,
        api_keys: Arc::new(Keys {
            calls: AtomicUsize::new(0),
            value: Some("key".to_owned()),
        }),
        executor: executor.clone(),
        attachments: Some(attachments.clone()),
    });
    *attachments.0.lock() = Some(AttachmentStore::new(Arc::new(BytesBackend)));
    let mut image_request = request();
    image_request.messages = vec![seekdeep_llm::Message::new(
        seekdeep_llm::MessageRole::User,
        vec![ContentBlock::Image {
            attachment: ImageAttachmentRef {
                attachment_id: AttachmentId::new(format!("sha256:{}", "a".repeat(64))),
                media_type: ImageMediaType::Png,
                bytes: 1,
                width: 1,
                height: 1,
                name: None,
            },
        }],
        seekdeep_llm::MessageSource::plugin("test"),
    )];
    collect(&adapter, image_request).await.unwrap();
    let request = &executor.requests.lock()[0];
    assert_eq!(
        serde_json::to_value(&request.context).unwrap()["messages"][0]["content"][0],
        json!({"type":"image","data":"AQ==","mimeType":"image/png"})
    );
}

#[tokio::test]
async fn profile_snapshot_is_captured_before_credential_await() {
    struct GatedKeys {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }
    #[async_trait]
    impl PiApiKeyResolver for GatedKeys {
        async fn resolve(
            &self,
            _provider: &ProviderId,
            _profile: &ResolvedPiProviderProfile,
        ) -> anyhow::Result<PiResolvedAuth> {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(PiResolvedAuth::api_key(Some("key".to_owned())))
        }
    }

    let profiles = Arc::new(Profiles(RwLock::new(profile_map("https://one.test"))));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let executor = Arc::new(Executor::default());
    let adapter = PiAiAdapter::new(PiAiAdapterOptions {
        profiles: profiles.clone(),
        api_keys: Arc::new(GatedKeys {
            entered: entered.clone(),
            release: release.clone(),
        }),
        executor: executor.clone(),
        attachments: None,
    });
    let task = tokio::spawn(async move { collect(&adapter, request()).await });
    entered.notified().await;
    *profiles.0.write().unwrap() = profile_map("https://two.test");
    release.notify_one();
    task.await.unwrap().unwrap();
    assert_eq!(
        executor.requests.lock()[0].model.base_url,
        "https://one.test"
    );
}

#[tokio::test]
async fn idle_timeout_and_caller_abort_are_stable_llm_failures() {
    let profiles = Arc::new(Profiles(RwLock::new(profile_map("https://one.test"))));
    let executor = Arc::new(Executor::default());
    executor.mode.store(1, Ordering::SeqCst);
    {
        let mut map = profiles.0.write().unwrap();
        let profile = Arc::make_mut(&mut map).get_mut("deepseek").unwrap();
        profile.stream_idle_timeout_ms = 10.0;
    }
    let adapter = adapter(
        profiles,
        Arc::new(Keys {
            calls: AtomicUsize::new(0),
            value: Some("key".to_owned()),
        }),
        executor,
    );
    assert_eq!(
        error_code(&collect(&adapter, request()).await.unwrap_err()),
        "TIMEOUT"
    );

    let signal = seekdeep_llm::AbortSignal::default();
    signal.abort();
    let mut aborted = request();
    aborted.signal = Some(signal);
    assert_eq!(
        error_code(&collect(&adapter, aborted).await.unwrap_err()),
        "ABORTED"
    );
}

#[tokio::test]
async fn synchronous_executor_setup_failure_is_an_in_stream_error_finish() {
    let profiles = Arc::new(Profiles(RwLock::new(profile_map("https://one.test"))));
    let executor = Arc::new(Executor::default());
    executor.mode.store(2, Ordering::SeqCst);
    let adapter = adapter(
        profiles,
        Arc::new(Keys {
            calls: AtomicUsize::new(0),
            value: Some("test-key".to_owned()),
        }),
        executor,
    );
    let chunks = collect(&adapter, request()).await.unwrap();
    let value = serde_json::to_value(chunks.last().unwrap()).unwrap();
    assert_eq!(value["reason"]["kind"], json!("error"));
    assert_eq!(
        value["reason"]["failure"]["message"],
        json!("executor setup failed")
    );
}

#[tokio::test]
async fn unresolved_provider_native_auth_fails_in_stream_before_executor_io() {
    struct Unconfigured;
    #[async_trait]
    impl PiApiKeyResolver for Unconfigured {
        async fn resolve(
            &self,
            _: &ProviderId,
            _: &ResolvedPiProviderProfile,
        ) -> anyhow::Result<PiResolvedAuth> {
            Ok(PiResolvedAuth::default())
        }
    }

    let profiles = Arc::new(Profiles(RwLock::new(profile_map("https://one.test"))));
    let executor = Arc::new(Executor::default());
    let adapter = PiAiAdapter::new(PiAiAdapterOptions {
        profiles,
        api_keys: Arc::new(Unconfigured),
        executor: executor.clone(),
        attachments: None,
    });
    let chunks = collect(&adapter, request()).await.unwrap();
    let value = serde_json::to_value(chunks.last().unwrap()).unwrap();
    assert_eq!(value["reason"]["kind"], json!("error"));
    assert_eq!(
        value["reason"]["failure"]["message"],
        json!("Provider is not configured: deepseek")
    );
    assert!(executor.requests.lock().is_empty());
}

#[tokio::test]
async fn provider_auth_materializes_endpoint_headers_and_environment_before_dispatch() {
    struct NativeAuth;
    #[async_trait]
    impl PiApiKeyResolver for NativeAuth {
        async fn resolve(
            &self,
            _: &ProviderId,
            _: &ResolvedPiProviderProfile,
        ) -> anyhow::Result<PiResolvedAuth> {
            Ok(PiResolvedAuth {
                configured: true,
                api_key: None,
                headers: std::collections::HashMap::from([
                    ("Authorization".to_owned(), None),
                    (
                        "cf-aig-authorization".to_owned(),
                        Some("Bearer gateway".to_owned()),
                    ),
                ]),
                environment: std::collections::HashMap::from([
                    ("ACCOUNT".to_owned(), "account-one".to_owned()),
                    ("GATEWAY".to_owned(), "gateway-one".to_owned()),
                ]),
            })
        }
    }

    let mut profiles = (*profile_map("https://{ACCOUNT}/{GATEWAY}")).clone();
    let profile = profiles.get_mut("deepseek").unwrap();
    profile.pi_provider.models[0].extra.insert(
        "headers".to_owned(),
        json!({"Authorization":"Bearer old","x-model":"kept"}),
    );
    let profiles = Arc::new(Profiles(RwLock::new(Arc::new(profiles))));
    let executor = Arc::new(Executor::default());
    let adapter = PiAiAdapter::new(PiAiAdapterOptions {
        profiles,
        api_keys: Arc::new(NativeAuth),
        executor: executor.clone(),
        attachments: None,
    });
    collect(&adapter, request()).await.unwrap();
    let requests = executor.requests.lock();
    let captured = &requests[0];
    assert_eq!(captured.model.base_url, "https://account-one/gateway-one");
    assert_eq!(
        captured.model.extra["headers"],
        json!({"x-model":"kept","cf-aig-authorization":"Bearer gateway"})
    );
    assert_eq!(captured.options.auth_environment["ACCOUNT"], "account-one");
}
