//! Adapter-bound runtime contracts ported from `service.spec.ts`.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use futures::{StreamExt, stream};
use parking_lot::Mutex;
use seekdeep_cordis::Context;
use seekdeep_llm::{
    AbortSignal, AdapterRejection, AdapterStream, ContentBlock, FinishReason, GenerateOptions,
    LlmAdapter, LlmCallConfig, LlmError, LlmFailure, LlmModelContext, LlmModelInfo,
    LlmModelReasoningInfo, LlmProviderInfo, LlmReasoningEffortInfo, LlmResolvedModelInfo,
    LlmRuntime, Message, MessageRole, MessageSource, ModelId, ModelModality, ProviderId,
    ReasoningEffortId, ResolvedRetryPolicy, StreamChunk, resolve_retry_policy,
};
use serde_json::json;
use tokio::sync::Notify;

#[derive(Debug)]
struct ArbitraryRejectionAdapter;

#[async_trait::async_trait]
impl LlmAdapter for ArbitraryRejectionAdapter {
    fn stream(&self, _options: GenerateOptions) -> AdapterStream {
        AdapterStream::from_rejections(stream::iter([Err(AdapterRejection::thrown(
            "plain provider failure",
        ))]))
    }
}

#[derive(Debug)]
struct BlockingResolutionAdapter {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    streams: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl LlmAdapter for BlockingResolutionAdapter {
    async fn resolve_model(
        &self,
        provider: &str,
        model: &str,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<LlmResolvedModelInfo> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(LlmResolvedModelInfo {
            provider: provider.into(),
            id: model.into(),
            name: model.to_owned(),
            description: None,
            input_modalities: None,
            context: None,
            default_max_tokens: None,
            reasoning: None,
        })
    }

    fn stream(&self, _options: GenerateOptions) -> AdapterStream {
        self.streams.fetch_add(1, Ordering::AcqRel);
        AdapterStream::new(stream::iter([Ok(StreamChunk::Finish {
            reason: FinishReason::Stop,
            replay_state: None,
        })]))
    }
}

#[derive(Debug)]
struct CancellationObservingAdapter {
    observed_aborted_signal: Arc<AtomicBool>,
}

#[derive(Debug)]
struct CountingResolutionAdapter {
    resolutions: Arc<AtomicUsize>,
    streams: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl LlmAdapter for CountingResolutionAdapter {
    async fn resolve_model(
        &self,
        provider: &str,
        model: &str,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<LlmResolvedModelInfo> {
        let resolution = self.resolutions.fetch_add(1, Ordering::AcqRel) + 1;
        Ok(LlmResolvedModelInfo {
            provider: provider.into(),
            id: model.into(),
            name: model.to_owned(),
            description: Some("Resolved model".to_owned()),
            input_modalities: None,
            context: Some(LlmModelContext {
                context_window: if resolution == 1 { 128_000 } else { 64_000 },
            }),
            default_max_tokens: None,
            reasoning: (model != "no-default").then(|| LlmModelReasoningInfo {
                efforts: vec![LlmReasoningEffortInfo {
                    id: ReasoningEffortId::new("high"),
                    name: "High".to_owned(),
                    description: None,
                }],
                default_effort: Some(ReasoningEffortId::new("high")),
            }),
        })
    }

    fn stream(&self, _options: GenerateOptions) -> AdapterStream {
        self.streams.fetch_add(1, Ordering::AcqRel);
        AdapterStream::new(stream::iter([Ok(StreamChunk::Finish {
            reason: FinishReason::Stop,
            replay_state: None,
        })]))
    }
}

#[async_trait::async_trait]
impl LlmAdapter for CancellationObservingAdapter {
    async fn resolve_model(
        &self,
        _provider: &str,
        _model: &str,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<LlmResolvedModelInfo> {
        self.observed_aborted_signal.store(
            signal.is_some_and(AbortSignal::is_aborted),
            Ordering::Release,
        );
        anyhow::bail!("cancelled lookup")
    }

    fn stream(&self, _options: GenerateOptions) -> AdapterStream {
        AdapterStream::new(stream::empty())
    }
}

fn options(provider: &str) -> GenerateOptions {
    GenerateOptions::new(ProviderId::new(provider), ModelId::new("model"), Vec::new())
}

fn call_config(provider: &str) -> LlmCallConfig {
    LlmCallConfig {
        provider: provider.into(),
        model: "model".into(),
        reasoning_effort: None,
        temperature: None,
        max_tokens: None,
        stop: None,
    }
}

async fn collect(mut output: seekdeep_llm::LlmStream) -> anyhow::Result<Vec<StreamChunk>> {
    let mut chunks = Vec::new();
    while let Some(chunk) = output.next().await {
        chunks.push(chunk?);
    }
    Ok(chunks)
}

#[derive(Debug)]
struct RecordingAdapter {
    requests: Arc<Mutex<Vec<GenerateOptions>>>,
    failure: Option<LlmFailure>,
    retry_policy: Option<ResolvedRetryPolicy>,
}

impl RecordingAdapter {
    fn successful(requests: Arc<Mutex<Vec<GenerateOptions>>>) -> Self {
        Self {
            requests,
            failure: None,
            retry_policy: None,
        }
    }
}

#[async_trait::async_trait]
impl LlmAdapter for RecordingAdapter {
    fn provider_retry_policy(&self, _provider: &str) -> Option<ResolvedRetryPolicy> {
        self.retry_policy.clone()
    }

    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        self.requests.lock().push(options);
        let chunk = self.failure.as_ref().map_or_else(
            || {
                Ok(StreamChunk::Finish {
                    reason: FinishReason::Stop,
                    replay_state: None,
                })
            },
            |failure| {
                Err(anyhow::Error::new(
                    LlmError::new(
                        failure.message.clone(),
                        failure.code.clone(),
                        failure.status,
                        failure.provider_retry_after_ms,
                        failure.request_id.clone(),
                    )
                    .expect("valid scripted failure"),
                ))
            },
        );
        AdapterStream::new(stream::iter([chunk]))
    }
}

#[derive(Debug)]
struct MetadataAdapter {
    provider: LlmProviderInfo,
    models: Vec<LlmModelInfo>,
    resolved: LlmResolvedModelInfo,
}

#[async_trait::async_trait]
impl LlmAdapter for MetadataAdapter {
    fn provider_info(&self, _provider: &str) -> LlmProviderInfo {
        self.provider.clone()
    }

    async fn list_models(&self, _provider: &str) -> anyhow::Result<Vec<LlmModelInfo>> {
        Ok(self.models.clone())
    }

    async fn resolve_model(
        &self,
        provider: &str,
        model: &str,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<LlmResolvedModelInfo> {
        let mut resolved = self.resolved.clone();
        resolved.provider = provider.into();
        resolved.id = model.into();
        Ok(resolved)
    }

    fn stream(&self, _options: GenerateOptions) -> AdapterStream {
        AdapterStream::new(stream::iter([Ok(StreamChunk::Finish {
            reason: FinishReason::Stop,
            replay_state: None,
        })]))
    }
}

fn model_source(provider: &str, replay: bool) -> MessageSource {
    let mut source = MessageSource::model(provider, "old-model");
    if replay {
        source
            .fields
            .insert("replayState".to_owned(), json!({"private": "state"}));
    }
    source
        .fields
        .insert("extension".to_owned(), json!("must be rebuilt away"));
    source
}

#[tokio::test]
async fn routing_retry_defaults_and_adapter_failures_match_the_source_boundary() {
    let context = Context::new();
    let runtime = LlmRuntime::install(&context).expect("runtime");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let always =
        resolve_retry_policy(Some(&json!({"mode": "always"})), "test retryPolicy").expect("policy");
    runtime
        .register_adapter(
            &["configured".to_owned()],
            Arc::new(RecordingAdapter {
                requests: requests.clone(),
                failure: None,
                retry_policy: Some(always.clone()),
            }),
        )
        .expect("configured adapter");
    runtime
        .register_adapter(
            &["defaulted".to_owned()],
            Arc::new(RecordingAdapter::successful(requests.clone())),
        )
        .expect("defaulted adapter");

    assert_eq!(runtime.provider_retry_policy("configured").unwrap(), always);
    assert_eq!(
        runtime
            .provider_retry_policy("defaulted")
            .unwrap()
            .max_retries(),
        Some(2)
    );
    let missing = runtime.provider_retry_policy("missing").unwrap_err();
    assert_eq!(
        missing.downcast_ref::<LlmError>().map(LlmError::code),
        Some("NO_ADAPTER")
    );
    assert!(collect(runtime.stream(options("configured"))).await.is_ok());
    assert_eq!(
        requests.lock().last().unwrap().provider.as_str(),
        "configured"
    );

    let chunks = collect(runtime.stream(options("unregistered")))
        .await
        .expect("normalized missing route");
    assert!(matches!(
        chunks.last(),
        Some(StreamChunk::Finish {
            reason: FinishReason::Error { failure },
            ..
        }) if failure.code == "NO_ADAPTER"
    ));

    let aborted = AbortSignal::default();
    aborted.abort_with_reason(json!("cancelled"));
    let mut request = options("failing");
    request.signal = Some(aborted);
    runtime
        .register_adapter(
            &["failing".to_owned()],
            Arc::new(RecordingAdapter {
                requests,
                failure: Some(LlmFailure {
                    message: "stopped".to_owned(),
                    code: "UNKNOWN".to_owned(),
                    status: None,
                    provider_retry_after_ms: None,
                    request_id: None,
                }),
                retry_policy: None,
            }),
        )
        .expect("failing adapter");
    let chunks = collect(runtime.stream(request))
        .await
        .expect("aborted finish");
    assert!(matches!(
        chunks.last(),
        Some(StreamChunk::Finish {
            reason: FinishReason::Aborted { failure },
            ..
        }) if failure.message == "stopped"
    ));
}

#[tokio::test]
async fn arbitrary_adapter_rejections_become_terminal_unknown_failures() {
    let context = Context::new();
    let runtime = LlmRuntime::install(&context).expect("runtime");
    runtime
        .register_adapter(&["test".to_owned()], Arc::new(ArbitraryRejectionAdapter))
        .expect("adapter");

    let chunks = collect(runtime.stream(options("test")))
        .await
        .expect("normalized rejection");
    assert_eq!(
        chunks.last(),
        Some(&StreamChunk::Finish {
            reason: FinishReason::Error {
                failure: LlmFailure {
                    message: "plain provider failure".to_owned(),
                    code: "UNKNOWN".to_owned(),
                    status: None,
                    provider_retry_after_ms: None,
                    request_id: None,
                },
            },
            replay_state: None,
        })
    );
}

#[tokio::test]
async fn exact_metadata_catalog_defaults_and_rejections_are_adapter_owned() {
    let context = Context::new();
    let runtime = LlmRuntime::install(&context).expect("runtime");
    let resolved = LlmResolvedModelInfo {
        provider: "route".into(),
        id: "model".into(),
        name: "Model".to_owned(),
        description: Some("Resolved model".to_owned()),
        input_modalities: Some(vec![
            ModelModality("text".to_owned()),
            ModelModality("image".to_owned()),
        ]),
        context: Some(LlmModelContext {
            context_window: 128_000,
        }),
        default_max_tokens: Some(8_192),
        reasoning: Some(LlmModelReasoningInfo {
            efforts: vec![
                LlmReasoningEffortInfo {
                    id: ReasoningEffortId::new("standard"),
                    name: "Standard".to_owned(),
                    description: None,
                },
                LlmReasoningEffortInfo {
                    id: ReasoningEffortId::new("ultra"),
                    name: "Ultra".to_owned(),
                    description: Some("Largest budget".to_owned()),
                },
            ],
            default_effort: Some(ReasoningEffortId::new("standard")),
        }),
    };
    runtime
        .register_adapter(
            &["route".to_owned()],
            Arc::new(MetadataAdapter {
                provider: LlmProviderInfo {
                    id: "route".into(),
                    name: "Route".to_owned(),
                },
                models: vec![LlmModelInfo {
                    provider: "route".into(),
                    id: "advisory".into(),
                    name: "Advisory".to_owned(),
                    description: None,
                    input_modalities: Some(vec![ModelModality("text".to_owned())]),
                }],
                resolved: resolved.clone(),
            }),
        )
        .expect("metadata adapter");

    assert_eq!(runtime.list_models("route").await.unwrap().len(), 1);
    assert_eq!(
        runtime
            .resolve_model_info("route", "unlisted", None)
            .await
            .unwrap()
            .context,
        resolved.context
    );
    let config = runtime
        .resolve_call_config(
            &LlmCallConfig {
                provider: "route".into(),
                model: "unlisted".into(),
                reasoning_effort: None,
                temperature: None,
                max_tokens: None,
                stop: None,
            },
            None,
        )
        .await
        .expect("defaults");
    assert_eq!(config.max_tokens, Some(8_192));
    assert_eq!(
        config
            .reasoning_effort
            .as_ref()
            .map(ReasoningEffortId::as_str),
        Some("standard")
    );

    let unsupported = runtime
        .resolve_call_config(
            &LlmCallConfig {
                reasoning_effort: Some(ReasoningEffortId::new("impossible")),
                ..config
            },
            None,
        )
        .await
        .expect_err("unsupported effort");
    assert_eq!(
        unsupported.downcast_ref::<LlmError>().map(LlmError::code),
        Some("UNSUPPORTED_REASONING_EFFORT")
    );
}

#[tokio::test]
async fn duplicate_catalog_model_ids_are_rejected() {
    let context = Context::new();
    let runtime = LlmRuntime::install(&context).expect("runtime");
    runtime
        .register_adapter(
            &["duplicate".to_owned()],
            Arc::new(MetadataAdapter {
                provider: LlmProviderInfo {
                    id: "duplicate".into(),
                    name: "Duplicate".to_owned(),
                },
                models: vec![
                    LlmModelInfo {
                        provider: "duplicate".into(),
                        id: "same".into(),
                        name: "One".to_owned(),
                        description: None,
                        input_modalities: None,
                    },
                    LlmModelInfo {
                        provider: "duplicate".into(),
                        id: "same".into(),
                        name: "Two".to_owned(),
                        description: None,
                        input_modalities: None,
                    },
                ],
                resolved: LlmResolvedModelInfo {
                    provider: "duplicate".into(),
                    id: "model".into(),
                    name: "Model".to_owned(),
                    description: None,
                    input_modalities: None,
                    context: None,
                    default_max_tokens: None,
                    reasoning: None,
                },
            }),
        )
        .expect("duplicate catalog adapter");
    let duplicate = runtime
        .list_models("duplicate")
        .await
        .expect_err("duplicate catalog");
    assert_eq!(
        duplicate.downcast_ref::<LlmError>().map(LlmError::code),
        Some("INVALID_CATALOG")
    );
}

#[tokio::test]
async fn lookup_cancellation_is_forwarded_to_the_adapter() {
    let context = Context::new();
    let runtime = LlmRuntime::install(&context).expect("runtime");
    let observed = Arc::new(AtomicBool::new(false));
    runtime
        .register_adapter(
            &["cancel".to_owned()],
            Arc::new(CancellationObservingAdapter {
                observed_aborted_signal: observed.clone(),
            }),
        )
        .expect("cancellation adapter");
    let signal = AbortSignal::default();
    signal.abort_with_reason(json!("stop"));
    runtime
        .resolve_model_info("cancel", "model", Some(&signal))
        .await
        .expect_err("lookup cancellation");
    assert!(observed.load(Ordering::Acquire));
}

#[tokio::test]
async fn registration_is_pinned_across_async_resolution_and_streaming() {
    let context = Context::new();
    let runtime = LlmRuntime::install(&context).expect("runtime");
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let old_streams = Arc::new(AtomicUsize::new(0));
    let old = runtime
        .register_adapter(
            &["pinned".to_owned()],
            Arc::new(BlockingResolutionAdapter {
                entered: entered.clone(),
                release: release.clone(),
                streams: old_streams.clone(),
            }),
        )
        .expect("old pinned adapter");
    let runtime_for_call = runtime.clone();
    let pending = tokio::spawn(async move {
        collect(runtime_for_call.stream(options("pinned")))
            .await
            .expect("pinned stream")
    });
    entered.notified().await;
    old.dispose().await.expect("withdraw old route");
    let replacement_requests = Arc::new(Mutex::new(Vec::new()));
    runtime
        .register_adapter(
            &["pinned".to_owned()],
            Arc::new(RecordingAdapter::successful(replacement_requests.clone())),
        )
        .expect("replacement adapter");
    release.notify_one();
    let chunks = pending.await.expect("pinned join");
    assert!(matches!(
        chunks.last(),
        Some(StreamChunk::Finish {
            reason: FinishReason::Stop,
            ..
        })
    ));
    assert_eq!(old_streams.load(Ordering::Acquire), 1);
    assert!(replacement_requests.lock().is_empty());
}

#[tokio::test]
async fn prepared_call_reuses_one_exact_lookup_for_config_and_context() {
    let context = Context::new();
    let runtime = LlmRuntime::install(&context).expect("runtime");
    let resolutions = Arc::new(AtomicUsize::new(0));
    let streams = Arc::new(AtomicUsize::new(0));
    runtime
        .register_adapter(
            &["route".to_owned()],
            Arc::new(CountingResolutionAdapter {
                resolutions: resolutions.clone(),
                streams: streams.clone(),
            }),
        )
        .expect("counting adapter");

    let prepared = runtime
        .prepare_call(&call_config("route"), None)
        .await
        .expect("prepared call");
    assert_eq!(
        prepared
            .config()
            .reasoning_effort
            .as_ref()
            .map(ReasoningEffortId::as_str),
        Some("high")
    );
    assert_eq!(
        prepared.context(),
        Some(&LlmModelContext {
            context_window: 128_000,
        })
    );
    assert_eq!(resolutions.load(Ordering::Acquire), 1);

    let mut request = options("route");
    request.reasoning_effort = Some(ReasoningEffortId::new("high"));
    collect(prepared.stream(request).expect("prepared stream"))
        .await
        .expect("drain prepared stream");
    assert_eq!(resolutions.load(Ordering::Acquire), 1);
    assert_eq!(streams.load(Ordering::Acquire), 1);

    let no_default = runtime
        .prepare_call(
            &LlmCallConfig {
                model: "no-default".into(),
                ..call_config("route")
            },
            None,
        )
        .await
        .expect("second prepared call");
    assert!(no_default.config().reasoning_effort.is_none());
    assert_eq!(
        no_default.context(),
        Some(&LlmModelContext {
            context_window: 64_000,
        })
    );
    assert_eq!(resolutions.load(Ordering::Acquire), 2);
}

#[tokio::test]
async fn prepared_call_keeps_old_registration_and_policy_after_route_replacement() {
    let context = Context::new();
    let runtime = LlmRuntime::install(&context).expect("runtime");
    let old_requests = Arc::new(Mutex::new(Vec::new()));
    let new_requests = Arc::new(Mutex::new(Vec::new()));
    let old_policy =
        resolve_retry_policy(Some(&json!({"mode": "always"})), "old").expect("old policy");
    let old = runtime
        .register_adapter(
            &["route".to_owned()],
            Arc::new(RecordingAdapter {
                requests: old_requests.clone(),
                failure: Some(LlmFailure {
                    message: "old route failed".to_owned(),
                    code: "AUTH".to_owned(),
                    status: None,
                    provider_retry_after_ms: None,
                    request_id: None,
                }),
                retry_policy: Some(old_policy.clone()),
            }),
        )
        .expect("old adapter");
    let prepared = runtime
        .prepare_call(
            &LlmCallConfig {
                provider: "route".into(),
                model: "model".into(),
                reasoning_effort: None,
                temperature: None,
                max_tokens: None,
                stop: None,
            },
            None,
        )
        .await
        .expect("prepare");
    old.dispose().await.expect("dispose old");
    runtime
        .register_adapter(
            &["route".to_owned()],
            Arc::new(RecordingAdapter::successful(new_requests.clone())),
        )
        .expect("new adapter");

    let chunks = collect(
        prepared
            .stream(options("route"))
            .expect("dispatch prepared"),
    )
    .await
    .expect("normalized old failure");
    assert!(matches!(
        chunks.last(),
        Some(StreamChunk::Finish {
            reason: FinishReason::Error { failure },
            ..
        }) if failure.code == "AUTH"
    ));
    assert_eq!(prepared.retry_policy(), &old_policy);
    assert_eq!(old_requests.lock().len(), 1);
    assert!(new_requests.lock().is_empty());
    assert!(prepared.stream(options("route")).is_err());
}

#[tokio::test]
async fn middleware_routes_before_resolution_and_replay_filtering_uses_adapter_identity() {
    let context = Context::new();
    let runtime = LlmRuntime::install(&context).expect("runtime");
    let shared_requests = Arc::new(Mutex::new(Vec::new()));
    let shared: Arc<dyn LlmAdapter> =
        Arc::new(RecordingAdapter::successful(shared_requests.clone()));
    runtime
        .register_adapter(&["historical".to_owned(), "routed".to_owned()], shared)
        .expect("shared routes");
    runtime
        .register_stream_middleware(
            &context,
            Arc::new(|mut request, next| {
                request.provider = "routed".into();
                next(request)
            }),
            false,
        )
        .expect("routing middleware");

    let mut same = options("initial").mark_agent_loop_request();
    same.messages.push(Message::new(
        MessageRole::Assistant,
        vec![ContentBlock::Text {
            text: "old response".to_owned(),
        }],
        model_source("historical", true),
    ));
    collect(runtime.stream(same)).await.expect("same adapter");
    let same_source = shared_requests.lock().last().unwrap().messages[0]
        .source()
        .clone();
    assert!(same_source.fields.contains_key("replayState"));
    assert!(same_source.fields.contains_key("extension"));
    assert!(
        shared_requests
            .lock()
            .last()
            .unwrap()
            .is_agent_loop_request()
    );

    let other_requests = Arc::new(Mutex::new(Vec::new()));
    runtime
        .register_adapter(
            &["other".to_owned()],
            Arc::new(RecordingAdapter::successful(other_requests.clone())),
        )
        .expect("other adapter");
    let mut cross = options("other").mark_agent_loop_request();
    cross.messages.push(Message::new(
        MessageRole::Assistant,
        vec![ContentBlock::Text {
            text: "old response".to_owned(),
        }],
        model_source("historical", true),
    ));
    let routing = runtime
        .register_stream_middleware(&context, Arc::new(|request, next| next(request)), true)
        .expect("identity wrapper");
    // The earlier routing middleware still routes every request, so withdraw it
    // by disposing the whole context-owned registration is not possible here;
    // use a second isolated runtime for the cross-adapter case.
    routing.dispose().await.expect("identity wrapper dispose");

    let cross_context = Context::new();
    let cross_runtime = LlmRuntime::install(&cross_context).expect("cross runtime");
    cross_runtime
        .register_adapter(
            &["historical".to_owned()],
            Arc::new(RecordingAdapter::successful(Arc::new(Mutex::new(
                Vec::new(),
            )))),
        )
        .expect("historical");
    cross_runtime
        .register_adapter(
            &["other".to_owned()],
            Arc::new(RecordingAdapter::successful(other_requests.clone())),
        )
        .expect("other");
    collect(cross_runtime.stream(cross))
        .await
        .expect("cross adapter");
    let cross_source = other_requests.lock().last().unwrap().messages[0]
        .source()
        .clone();
    assert_eq!(
        cross_source
            .fields
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["provider", "model"]
    );
    assert_eq!(cross_source.fields["provider"], json!("historical"));
    assert_eq!(cross_source.fields["model"], json!("old-model"));
    assert!(
        !other_requests
            .lock()
            .last()
            .unwrap()
            .is_agent_loop_request()
    );
}
