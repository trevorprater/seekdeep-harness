//! Production configuration-domain cases ported from `api-proxy-config.spec.ts`.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use futures::{FutureExt as _, StreamExt as _, future::BoxFuture, stream};
use parking_lot::Mutex;
use seekdeep_client_connection::{HttpResponse, RpcError, RpcResult};
use seekdeep_cordis::Context;
use seekdeep_credentials::{
    CredentialInfo, CredentialNotifier, CredentialProvider, CredentialRef, CredentialService,
    ResolvedCredential,
};
use seekdeep_host_apiproxy::{
    ApiDownlinkStream, ApiProxyRuntime, ClientResponse, ConfigurationApiProxyOptions,
    ConfigurationApiProxyRuntime, RpcId, RpcMethod, RpcReceipt, RpcRequest, RpcResponse,
    api::{
        downloads::SessionLogQuery,
        events::{HostFrame, MuxFrame},
    },
};
use seekdeep_llm::{
    AbortSignal, AdapterStream, GenerateOptions, LlmAdapter, LlmConfigurableProvider,
    LlmDiscoveredModel, LlmModelContext, LlmModelInfo, LlmModelReasoningInfo, LlmProviderInfo,
    LlmReasoningEffortInfo, LlmResolvedModelInfo, LlmRuntime, ReasoningEffortId,
};
use seekdeep_schemastery::Schema;
use seekdeep_settings::{
    SettingsDocument, SettingsRegisterOptions, SettingsService, SettingsStorage, settings_namespace,
};
use serde_json::{Map, Value, json};

#[derive(Debug, Default)]
struct RemainingDomains {
    methods: Mutex<Vec<RpcMethod>>,
}

impl ApiProxyRuntime for RemainingDomains {
    fn unary(
        &self,
        method: RpcMethod,
        request: RpcRequest<Value>,
        _signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcResponse<Value>>> {
        self.methods.lock().push(method);
        async move {
            Ok(RpcResponse::new(
                request.rpc_id,
                RpcResult::Success {
                    value: Some(json!({ "delegated": method.as_str() })),
                },
            ))
        }
        .boxed()
    }

    fn respond(
        &self,
        _message: ClientResponse,
        _signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcReceipt>> {
        async { Ok(RpcReceipt::Accepted) }.boxed()
    }

    fn mux(
        &self,
        _request: RpcRequest<Value>,
        _signal: AbortSignal,
    ) -> ApiDownlinkStream<MuxFrame> {
        futures::stream::empty().boxed()
    }

    fn host(
        &self,
        _request: RpcRequest<Value>,
        _signal: AbortSignal,
    ) -> ApiDownlinkStream<HostFrame> {
        futures::stream::empty().boxed()
    }

    fn session_log(
        &self,
        _query: SessionLogQuery,
        _signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<HttpResponse>> {
        async { Ok(HttpResponse::text(501, "not used")) }.boxed()
    }
}

#[derive(Default)]
struct MemorySettingsStorage {
    document: Mutex<SettingsDocument>,
    writable: AtomicBool,
    document_path: Option<PathBuf>,
    prepared_path: Option<PathBuf>,
    prepare_entered: Option<Arc<tokio::sync::Notify>>,
    prepare_release: Option<Arc<tokio::sync::Notify>>,
}

impl MemorySettingsStorage {
    fn new(document: Value) -> Arc<Self> {
        let Value::Object(document) = document else {
            panic!("settings fixture must be an object");
        };
        Arc::new(Self {
            document: Mutex::new(document),
            writable: AtomicBool::new(true),
            document_path: None,
            prepared_path: None,
            prepare_entered: None,
            prepare_release: None,
        })
    }

    fn with_document_path(document: Value, described: &str, prepared: &str) -> Arc<Self> {
        let Value::Object(document) = document else {
            panic!("settings fixture must be an object");
        };
        Arc::new(Self {
            document: Mutex::new(document),
            writable: AtomicBool::new(true),
            document_path: Some(PathBuf::from(described)),
            prepared_path: Some(PathBuf::from(prepared)),
            prepare_entered: None,
            prepare_release: None,
        })
    }

    fn with_blocking_prepare(
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    ) -> Arc<Self> {
        Arc::new(Self {
            document: Mutex::new(Map::new()),
            writable: AtomicBool::new(true),
            document_path: Some(PathBuf::from("/tmp/settings.yaml")),
            prepared_path: Some(PathBuf::from("/tmp/settings.yaml")),
            prepare_entered: Some(entered),
            prepare_release: Some(release),
        })
    }
}

#[async_trait]
impl SettingsStorage for MemorySettingsStorage {
    fn writable(&self) -> bool {
        self.writable.load(Ordering::Acquire)
    }

    fn document_path(&self) -> Option<&Path> {
        self.document_path.as_deref()
    }

    async fn prepare_document(&self) -> anyhow::Result<Option<PathBuf>> {
        if let Some(entered) = &self.prepare_entered {
            entered.notify_one();
        }
        if let Some(release) = &self.prepare_release {
            release.notified().await;
        }
        Ok(self.prepared_path.clone())
    }

    async fn load(&self) -> anyhow::Result<SettingsDocument> {
        Ok(self.document.lock().clone())
    }

    async fn persist(
        &self,
        namespace: &seekdeep_settings::SettingsNamespace,
        section: &Map<String, Value>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(self.writable(), "settings provider is read-only");
        self.document
            .lock()
            .insert(namespace.to_string(), Value::Object(section.clone()));
        Ok(())
    }
}

struct MemoryCredentials {
    values: Mutex<HashMap<CredentialRef, String>>,
    shadowed: HashSet<CredentialRef>,
    notifier: CredentialNotifier,
}

#[async_trait]
impl CredentialProvider for MemoryCredentials {
    async fn resolve(
        &self,
        reference: &CredentialRef,
    ) -> anyhow::Result<Option<ResolvedCredential>> {
        if self.shadowed.contains(reference) {
            return Ok(Some(ResolvedCredential {
                value: "ambient-secret".to_owned(),
                source: "env".to_owned(),
            }));
        }
        Ok(self
            .values
            .lock()
            .get(reference)
            .map(|value| ResolvedCredential {
                value: value.clone(),
                source: "file".to_owned(),
            }))
    }

    async fn describe(&self, reference: &CredentialRef) -> anyhow::Result<CredentialInfo> {
        if self.shadowed.contains(reference) {
            return Ok(CredentialInfo {
                configured: true,
                source: Some("env".to_owned()),
                writable: false,
            });
        }
        let configured = self.values.lock().contains_key(reference);
        Ok(CredentialInfo {
            configured,
            source: configured.then(|| "file".to_owned()),
            writable: true,
        })
    }

    async fn set(&self, reference: &CredentialRef, value: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.shadowed.contains(reference),
            "credential is shadowed by env"
        );
        self.values
            .lock()
            .insert(reference.clone(), value.to_owned());
        self.notifier.notify_updated(reference)
    }

    async fn unset(&self, reference: &CredentialRef) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.shadowed.contains(reference),
            "credential is shadowed by env"
        );
        if self.values.lock().remove(reference).is_some() {
            self.notifier.notify_updated(reference)?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct CatalogAdapter {
    name: String,
    models: Vec<String>,
    broken: bool,
}

#[async_trait]
impl LlmAdapter for CatalogAdapter {
    fn provider_info(&self, provider: &str) -> LlmProviderInfo {
        LlmProviderInfo {
            id: provider.into(),
            name: self.name.clone(),
        }
    }

    async fn list_models(&self, provider: &str) -> anyhow::Result<Vec<LlmModelInfo>> {
        anyhow::ensure!(!self.broken, "catalog backend down");
        Ok(self
            .models
            .iter()
            .map(|model| LlmModelInfo {
                provider: provider.into(),
                id: model.as_str().into(),
                name: model.clone(),
                description: (model == "reasoner").then(|| "Reasoning model".to_owned()),
                input_modalities: None,
            })
            .collect())
    }

    async fn resolve_model(
        &self,
        provider: &str,
        model: &str,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<LlmResolvedModelInfo> {
        Ok(LlmResolvedModelInfo {
            provider: provider.into(),
            id: model.into(),
            name: model.to_owned(),
            description: None,
            input_modalities: None,
            context: Some(LlmModelContext {
                context_window: 128_000,
            }),
            default_max_tokens: None,
            reasoning: (model == "reasoner").then(|| LlmModelReasoningInfo {
                efforts: vec![LlmReasoningEffortInfo {
                    id: ReasoningEffortId::new("high"),
                    name: "High".to_owned(),
                    description: Some("More reasoning".to_owned()),
                }],
                default_effort: Some(ReasoningEffortId::new("high")),
            }),
        })
    }

    fn stream(&self, _options: GenerateOptions) -> AdapterStream {
        AdapterStream::new(stream::empty())
    }
}

struct Harness {
    context: Context,
    llm: Arc<LlmRuntime>,
    runtime: Arc<ConfigurationApiProxyRuntime>,
    remaining: Arc<RemainingDomains>,
}

impl Harness {
    async fn new(
        settings: Option<Arc<MemorySettingsStorage>>,
        credentials: bool,
        options: ConfigurationApiProxyOptions,
    ) -> Self {
        let context = Context::new();
        let llm = LlmRuntime::install(&context).expect("LLM runtime");
        if let Some(storage) = settings {
            SettingsService::install(&context, storage)
                .await
                .expect("settings runtime");
        }
        if credentials {
            let provider = Arc::new(MemoryCredentials {
                values: Mutex::new(HashMap::new()),
                shadowed: HashSet::new(),
                notifier: CredentialNotifier::new(&context),
            });
            CredentialService::new(provider)
                .provide(&context)
                .expect("credentials service");
        }
        let remaining = Arc::new(RemainingDomains::default());
        let runtime =
            ConfigurationApiProxyRuntime::from_context(&context, options, remaining.clone())
                .expect("configuration runtime");
        Self {
            context,
            llm,
            runtime,
            remaining,
        }
    }

    fn register_provider_directory(&self) {
        self.llm
            .register_configurable_providers(&[LlmConfigurableProvider {
                provider: "deepseek-official".into(),
                display_name: "DeepSeek".to_owned(),
                settings_ns: "llm-deepseek".to_owned(),
                settings_path: Vec::new(),
                authentication: seekdeep_llm::LlmProviderAuthentication::ApiKey,
                declared: None,
            }])
            .expect("provider directory");
    }
}

fn adapter(name: &str, models: &[&str]) -> Arc<CatalogAdapter> {
    Arc::new(CatalogAdapter {
        name: name.to_owned(),
        models: models.iter().map(ToString::to_string).collect(),
        broken: false,
    })
}

fn provider_schema() -> Schema {
    Schema::object([
        ("apiKey", Schema::string().role("secret")),
        (
            "apiKeyEnv",
            Schema::string().with_default("DEEPSEEK_API_KEY"),
        ),
        ("baseURL", Schema::string()),
    ])
}

fn request(payload: Value) -> RpcRequest<Value> {
    RpcRequest::new(RpcId::new("configuration-test"), payload)
}

async fn invoke(
    runtime: &ConfigurationApiProxyRuntime,
    method: RpcMethod,
    payload: Value,
) -> RpcResult<Value> {
    runtime
        .unary(method, request(payload), AbortSignal::default())
        .await
        .expect("runtime call")
        .result
}

fn value(result: RpcResult<Value>) -> Value {
    match result {
        RpcResult::Success { value: Some(value) } => value,
        other => panic!("expected success value, got {other:?}"),
    }
}

fn error(result: RpcResult<Value>) -> RpcError {
    match result {
        RpcResult::Failure { error } => error,
        other @ RpcResult::Success { .. } => panic!("expected failure, got {other:?}"),
    }
}

async fn provider_settings_harness() -> Harness {
    let harness = Harness::new(
        Some(MemorySettingsStorage::new(json!({}))),
        false,
        ConfigurationApiProxyOptions::default(),
    )
    .await;
    harness.register_provider_directory();
    let settings = harness.context.get(seekdeep_settings::SETTINGS).unwrap();
    settings
        .register(
            &harness.context,
            &settings_namespace("llm-deepseek").unwrap(),
            provider_schema(),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    settings
        .register(
            &harness.context,
            &settings_namespace("some-other-plugin").unwrap(),
            Schema::object([("secretPath", Schema::string())]),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    harness
}

async fn next_remote_event(stream: &mut ApiDownlinkStream<HostFrame>) -> (String, Vec<Value>) {
    let frame = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("remote event timeout")
        .expect("remote event stream ended")
        .expect("remote event stream failure");
    match frame.payload {
        HostFrame::RemoteEvent { event, args } => (event, args),
        other => panic!("expected remote event, got {other:?}"),
    }
}

#[tokio::test]
async fn optional_services_fail_locally_and_unrelated_methods_still_delegate() {
    let harness = Harness::new(None, false, ConfigurationApiProxyOptions::default()).await;
    let settings = error(invoke(&harness.runtime, RpcMethod::SettingsDescribe, json!({})).await);
    assert_eq!(settings.code, "internal");
    assert!(settings.message.contains("seekdeep-settings-file"));
    let credentials = error(
        invoke(
            &harness.runtime,
            RpcMethod::CredentialsDescribe,
            json!({ "refs": ["OPENAI_API_KEY"] }),
        )
        .await,
    );
    assert_eq!(credentials.code, "internal");
    assert!(credentials.message.contains("seekdeep-credentials-local"));

    assert_eq!(
        value(invoke(&harness.runtime, RpcMethod::SessionList, json!({})).await),
        json!({ "delegated": "session.list" })
    );
    assert_eq!(*harness.remaining.methods.lock(), [RpcMethod::SessionList]);
}

#[tokio::test]
async fn settings_describe_redacts_layers_and_exposes_only_the_configuration_boundary() {
    let storage = MemorySettingsStorage::with_document_path(
        json!({
            "llm-deepseek": { "apiKey": "user-secret", "baseURL": "https://user" }
        }),
        "/tmp/described-settings.yaml",
        "/tmp/prepared-settings.yaml",
    );
    let harness = Harness::new(
        Some(storage),
        false,
        ConfigurationApiProxyOptions::default(),
    )
    .await;
    harness.register_provider_directory();
    let settings = harness.context.get(seekdeep_settings::SETTINGS).unwrap();
    settings
        .register(
            &harness.context,
            &settings_namespace("llm-deepseek").unwrap(),
            provider_schema(),
            SettingsRegisterOptions {
                base: Some(json!({ "baseURL": "https://base" })),
                ..SettingsRegisterOptions::default()
            },
        )
        .unwrap();
    settings
        .register(
            &harness.context,
            &settings_namespace("some-other-plugin").unwrap(),
            Schema::object([("secretPath", Schema::string())]),
            SettingsRegisterOptions::default(),
        )
        .unwrap();

    let described = value(invoke(&harness.runtime, RpcMethod::SettingsDescribe, json!({})).await);
    assert_eq!(described["writable"], true);
    assert_eq!(described["hasDocument"], true);
    assert_eq!(described["namespaces"].as_array().unwrap().len(), 1);
    let view = &described["namespaces"][0];
    assert_eq!(view["ns"], "llm-deepseek");
    assert_eq!(
        view["value"],
        json!({ "apiKeyEnv": "DEEPSEEK_API_KEY", "baseURL": "https://user" })
    );
    assert_eq!(view["base"], json!({ "baseURL": "https://base" }));
    assert_eq!(view["user"], json!({ "baseURL": "https://user" }));
    assert_eq!(
        view["secrets"],
        json!([{ "path": ["apiKey"], "set": true }])
    );
    assert!(!described.to_string().contains("user-secret"));
}

#[tokio::test]
async fn settings_open_uses_only_the_provider_path_and_honors_cancellation() {
    let storage = MemorySettingsStorage::with_document_path(
        json!({}),
        "/tmp/described-settings.yaml",
        "/tmp/prepared-settings.yaml",
    );
    let opened = Arc::new(Mutex::new(Vec::new()));
    let opened_for_boundary = opened.clone();
    let harness = Harness::new(
        Some(storage),
        false,
        ConfigurationApiProxyOptions {
            open_text_file: Some(Arc::new(move |path, _| {
                opened_for_boundary.lock().push(path);
                async { Ok(()) }.boxed()
            })),
            ..ConfigurationApiProxyOptions::default()
        },
    )
    .await;
    assert_eq!(
        value(invoke(&harness.runtime, RpcMethod::SettingsOpenDocument, json!({})).await),
        json!({ "opened": true })
    );
    assert_eq!(*opened.lock(), ["/tmp/prepared-settings.yaml"]);

    let signal = AbortSignal::default();
    signal.abort();
    let cancelled = harness
        .runtime
        .unary(RpcMethod::SettingsOpenDocument, request(json!({})), signal)
        .await
        .unwrap()
        .result;
    assert_eq!(error(cancelled).code, "cancelled");
    assert_eq!(opened.lock().len(), 1);

    let no_document = Harness::new(
        Some(MemorySettingsStorage::new(json!({}))),
        false,
        ConfigurationApiProxyOptions::default(),
    )
    .await;
    assert!(
        error(
            invoke(
                &no_document.runtime,
                RpcMethod::SettingsOpenDocument,
                json!({})
            )
            .await
        )
        .message
        .contains("no local document")
    );
}

#[tokio::test]
async fn settings_open_cancellation_during_prepare_never_reaches_the_native_boundary() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let opened = Arc::new(AtomicBool::new(false));
    let opened_for_boundary = opened.clone();
    let harness = Harness::new(
        Some(MemorySettingsStorage::with_blocking_prepare(
            entered.clone(),
            release.clone(),
        )),
        false,
        ConfigurationApiProxyOptions {
            open_text_file: Some(Arc::new(move |_, _| {
                opened_for_boundary.store(true, Ordering::Release);
                async { Ok(()) }.boxed()
            })),
            ..ConfigurationApiProxyOptions::default()
        },
    )
    .await;
    let signal = AbortSignal::default();
    let runtime = harness.runtime.clone();
    let opening_signal = signal.clone();
    let opening = tokio::spawn(async move {
        runtime
            .unary(
                RpcMethod::SettingsOpenDocument,
                request(json!({})),
                opening_signal,
            )
            .await
            .unwrap()
            .result
    });
    entered.notified().await;
    signal.abort();
    release.notify_one();
    assert_eq!(error(opening.await.unwrap()).code, "cancelled");
    assert!(!opened.load(Ordering::Acquire));
}

#[tokio::test]
async fn settings_writes_redact_and_enforce_revision_cas() {
    let harness = provider_settings_harness().await;
    let opened =
        value(invoke(&harness.runtime, RpcMethod::SettingsDescribe, json!({})).await)["namespaces"]
            [0]["revision"]
            .as_f64()
            .unwrap();
    let updated = value(
        invoke(
            &harness.runtime,
            RpcMethod::SettingsUpdate,
            json!({
                "ns": "llm-deepseek",
                "patch": { "apiKey": "sk-new", "baseURL": "https://next" },
                "expectedRevision": opened
            }),
        )
        .await,
    );
    assert_eq!(
        updated["value"],
        json!({ "apiKeyEnv": "DEEPSEEK_API_KEY", "baseURL": "https://next" })
    );
    assert_eq!(updated["user"], json!({ "baseURL": "https://next" }));
    assert_eq!(updated["secrets"][0]["set"], true);
    assert!(!updated.to_string().contains("sk-new"));

    let conflict = error(
        invoke(
            &harness.runtime,
            RpcMethod::SettingsUpdate,
            json!({
                "ns": "llm-deepseek", "patch": { "baseURL": "https://stale" },
                "expectedRevision": opened
            }),
        )
        .await,
    );
    assert_eq!(conflict.code, "settings-conflict");
    assert_eq!(conflict.details["expected"], json!(opened));
    assert_eq!(conflict.details["actual"].as_f64(), Some(opened + 1.0));
}

#[tokio::test]
async fn settings_mutation_reset_and_exposure_boundary_match_the_source() {
    let harness = provider_settings_harness().await;
    value(
        invoke(
            &harness.runtime,
            RpcMethod::SettingsUpdate,
            json!({
                "ns": "llm-deepseek",
                "patch": { "apiKey": "sk-new", "baseURL": "https://next" }
            }),
        )
        .await,
    );
    let mutated = value(
        invoke(
            &harness.runtime,
            RpcMethod::SettingsMutate,
            json!({
                "ns": "llm-deepseek",
                "ops": [{ "op": "unset", "path": ["baseURL"] }]
            }),
        )
        .await,
    );
    assert_eq!(mutated["value"], json!({ "apiKeyEnv": "DEEPSEEK_API_KEY" }));
    let replaced = value(
        invoke(
            &harness.runtime,
            RpcMethod::SettingsReplace,
            json!({ "ns": "llm-deepseek", "section": {} }),
        )
        .await,
    );
    assert_eq!(replaced["user"], json!({}));

    for method_payload in [
        (
            RpcMethod::SettingsUpdate,
            json!({ "ns": "some-other-plugin", "patch": {} }),
        ),
        (
            RpcMethod::SettingsReplace,
            json!({ "ns": "unknown-ns", "section": {} }),
        ),
    ] {
        let refused = error(invoke(&harness.runtime, method_payload.0, method_payload.1).await);
        assert_eq!(refused.code, "settings-not-exposed");
    }
    let malformed = error(
        invoke(
            &harness.runtime,
            RpcMethod::SettingsUpdate,
            json!({ "ns": "Not A Namespace", "patch": {} }),
        )
        .await,
    );
    assert_eq!(malformed.code, "settings-rejected");

    let schema_invalid = error(
        invoke(
            &harness.runtime,
            RpcMethod::SettingsUpdate,
            json!({ "ns": "llm-deepseek", "patch": { "baseURL": 42 } }),
        )
        .await,
    );
    assert_eq!(schema_invalid.code, "settings-rejected");
}

#[tokio::test]
async fn settings_allowlists_cover_web_product_and_preset_namespaces_only() {
    let harness = Harness::new(
        Some(MemorySettingsStorage::new(json!({}))),
        false,
        ConfigurationApiProxyOptions::default(),
    )
    .await;
    harness.register_provider_directory();
    let settings = harness.context.get(seekdeep_settings::SETTINGS).unwrap();
    let expected = [
        "agent-loop",
        "shell",
        "locale",
        "permission",
        "ui-conversation",
        "ui-theme",
        "web-search-deepseek",
        "ui-onboarding",
        "agent-presets",
        "llm-deepseek",
    ];
    for namespace in expected {
        settings
            .register(
                &harness.context,
                &settings_namespace(namespace).unwrap(),
                Schema::object([("enabled", Schema::boolean())]),
                SettingsRegisterOptions::default(),
            )
            .unwrap();
    }
    settings
        .register(
            &harness.context,
            &settings_namespace("private-plugin").unwrap(),
            Schema::object([("enabled", Schema::boolean())]),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    let described = value(invoke(&harness.runtime, RpcMethod::SettingsDescribe, json!({})).await);
    let names = described["namespaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|view| view["ns"].as_str().unwrap())
        .collect::<HashSet<_>>();
    assert_eq!(names, expected.into_iter().collect());

    value(
        invoke(
            &harness.runtime,
            RpcMethod::SettingsUpdate,
            json!({ "ns": "agent-presets", "patch": { "enabled": true } }),
        )
        .await,
    );
    assert_eq!(
        settings.get(&settings_namespace("agent-presets").unwrap()),
        Some(json!({
            "enabled": true
        }))
    );
}

#[tokio::test]
async fn withdrawn_provider_namespace_is_immediately_unexposed() {
    let harness = Harness::new(
        Some(MemorySettingsStorage::new(json!({}))),
        false,
        ConfigurationApiProxyOptions::default(),
    )
    .await;
    let directory = harness
        .llm
        .register_configurable_providers(&[LlmConfigurableProvider {
            provider: "deepseek-official".into(),
            display_name: "DeepSeek".to_owned(),
            settings_ns: "llm-deepseek".to_owned(),
            settings_path: Vec::new(),
            authentication: seekdeep_llm::LlmProviderAuthentication::ApiKey,
            declared: None,
        }])
        .unwrap();
    harness
        .context
        .get(seekdeep_settings::SETTINGS)
        .unwrap()
        .register(
            &harness.context,
            &settings_namespace("llm-deepseek").unwrap(),
            provider_schema(),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    assert_eq!(
        value(invoke(&harness.runtime, RpcMethod::SettingsDescribe, json!({})).await)["namespaces"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    directory.dispose().await.unwrap();
    assert!(
        value(invoke(&harness.runtime, RpcMethod::SettingsDescribe, json!({})).await)["namespaces"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        error(
            invoke(
                &harness.runtime,
                RpcMethod::SettingsUpdate,
                json!({ "ns": "llm-deepseek", "patch": {} }),
            )
            .await
        )
        .code,
        "settings-not-exposed"
    );
}

#[tokio::test]
async fn read_only_settings_report_the_capability_and_map_writes_to_rejected() {
    let storage = MemorySettingsStorage::new(json!({}));
    storage.writable.store(false, Ordering::Release);
    let harness = Harness::new(
        Some(storage),
        false,
        ConfigurationApiProxyOptions::default(),
    )
    .await;
    harness.register_provider_directory();
    harness
        .context
        .get(seekdeep_settings::SETTINGS)
        .unwrap()
        .register(
            &harness.context,
            &settings_namespace("llm-deepseek").unwrap(),
            provider_schema(),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    assert_eq!(
        value(invoke(&harness.runtime, RpcMethod::SettingsDescribe, json!({})).await)["writable"],
        false
    );
    let refused = error(
        invoke(
            &harness.runtime,
            RpcMethod::SettingsUpdate,
            json!({ "ns": "llm-deepseek", "patch": {} }),
        )
        .await,
    );
    assert_eq!(refused.code, "settings-rejected");
    assert!(refused.message.contains("read-only"));
}

#[tokio::test]
async fn credentials_are_value_free_write_only_and_forward_committed_events() {
    let harness = Harness::new(None, true, ConfigurationApiProxyOptions::default()).await;
    let signal = AbortSignal::default();
    let mut events = harness.runtime.host(request(json!({})), signal.clone());
    let before = value(
        invoke(
            &harness.runtime,
            RpcMethod::CredentialsDescribe,
            json!({ "refs": ["OPENAI_API_KEY"] }),
        )
        .await,
    );
    assert_eq!(
        before,
        json!({ "credentials": { "OPENAI_API_KEY": { "configured": false, "writable": true } } })
    );
    assert_eq!(
        value(
            invoke(
                &harness.runtime,
                RpcMethod::CredentialsSet,
                json!({ "ref": "OPENAI_API_KEY", "value": "sk-secret" }),
            )
            .await
        ),
        json!({})
    );
    assert_eq!(
        next_remote_event(&mut events).await,
        (
            "credentials/updated".to_owned(),
            vec![json!("OPENAI_API_KEY")]
        )
    );
    let after = value(
        invoke(
            &harness.runtime,
            RpcMethod::CredentialsDescribe,
            json!({ "refs": ["OPENAI_API_KEY"] }),
        )
        .await,
    );
    assert_eq!(after["credentials"]["OPENAI_API_KEY"]["configured"], true);
    assert_eq!(after["credentials"]["OPENAI_API_KEY"]["source"], "file");
    assert!(!after.to_string().contains("sk-secret"));
    value(
        invoke(
            &harness.runtime,
            RpcMethod::CredentialsUnset,
            json!({ "ref": "OPENAI_API_KEY" }),
        )
        .await,
    );
    assert_eq!(
        next_remote_event(&mut events).await.0,
        "credentials/updated"
    );
    signal.abort();
}

#[tokio::test]
async fn shadowed_credentials_reject_set_and_unset_without_leaking_values() {
    let harness = Harness::new(None, false, ConfigurationApiProxyOptions::default()).await;
    let reference = seekdeep_credentials::credential_ref("DEEPSEEK_API_KEY").unwrap();
    let provider = Arc::new(MemoryCredentials {
        values: Mutex::new(HashMap::new()),
        shadowed: HashSet::from([reference]),
        notifier: CredentialNotifier::new(&harness.context),
    });
    CredentialService::new(provider)
        .provide(&harness.context)
        .unwrap();
    let described = value(
        invoke(
            &harness.runtime,
            RpcMethod::CredentialsDescribe,
            json!({ "refs": ["DEEPSEEK_API_KEY"] }),
        )
        .await,
    );
    assert_eq!(
        described,
        json!({
            "credentials": {
                "DEEPSEEK_API_KEY": { "configured": true, "source": "env", "writable": false }
            }
        })
    );
    for (method, payload) in [
        (
            RpcMethod::CredentialsSet,
            json!({ "ref": "DEEPSEEK_API_KEY", "value": "must-not-leak" }),
        ),
        (
            RpcMethod::CredentialsUnset,
            json!({ "ref": "DEEPSEEK_API_KEY" }),
        ),
    ] {
        let refusal = error(invoke(&harness.runtime, method, payload).await);
        assert_eq!(refusal.code, "credential-rejected");
        assert_eq!(
            refusal.details,
            Map::from_iter([("ref".to_owned(), json!("DEEPSEEK_API_KEY"))])
        );
        assert!(!format!("{refusal:?}").contains("must-not-leak"));
    }
}

#[tokio::test]
async fn llm_directory_catalog_failures_and_topology_events_match_the_host_view() {
    let harness = Harness::new(None, false, ConfigurationApiProxyOptions::default()).await;
    harness
        .llm
        .register_configurable_providers(&[
            LlmConfigurableProvider {
                provider: "deepseek-official".into(),
                display_name: "DeepSeek".to_owned(),
                settings_ns: "llm-deepseek".to_owned(),
                settings_path: Vec::new(),
                authentication: seekdeep_llm::LlmProviderAuthentication::ApiKey,
                declared: None,
            },
            LlmConfigurableProvider {
                provider: "openai".into(),
                display_name: "OpenAI".to_owned(),
                settings_ns: "llm-pi-ai".to_owned(),
                settings_path: vec!["providers".to_owned(), "openai".to_owned()],
                authentication: seekdeep_llm::LlmProviderAuthentication::ProviderNative,
                declared: None,
            },
        ])
        .unwrap();
    harness
        .llm
        .register_adapter(
            &["deepseek-official".to_owned()],
            adapter("DeepSeek", &["chat", "reasoner"]),
        )
        .unwrap();
    harness
        .llm
        .register_adapter(&["undeclared".to_owned()], adapter("Undeclared", &["u-1"]))
        .unwrap();
    harness
        .llm
        .register_adapter(
            &["broken".to_owned()],
            Arc::new(CatalogAdapter {
                name: "Broken".to_owned(),
                models: Vec::new(),
                broken: true,
            }),
        )
        .unwrap();

    let providers = value(invoke(&harness.runtime, RpcMethod::LlmProviders, json!({})).await);
    assert_eq!(providers["providers"][0]["provider"], "deepseek-official");
    assert_eq!(providers["providers"][0]["active"], true);
    assert_eq!(providers["providers"][1]["provider"], "openai");
    assert_eq!(providers["providers"][1]["active"], false);
    assert_eq!(providers["providers"][2]["provider"], "undeclared");
    assert_eq!(providers["providers"][2]["settingsNs"], "");
    assert_eq!(providers["providers"][3]["provider"], "broken");

    let models = value(invoke(&harness.runtime, RpcMethod::LlmModels, json!({})).await);
    assert_eq!(models["groups"].as_array().unwrap().len(), 2);
    assert_eq!(
        models["groups"][0]["models"][1]["reasoning"]["defaultEffort"],
        "high"
    );
    assert_eq!(
        models["failures"],
        json!([{
            "id": "broken", "name": "Broken", "message": "catalog backend down"
        }])
    );

    let signal = AbortSignal::default();
    let mut events = harness.runtime.host(request(json!({})), signal.clone());
    let transient = harness
        .llm
        .register_adapter(&["transient".to_owned()], adapter("Transient", &[]))
        .unwrap();
    assert_eq!(
        next_remote_event(&mut events).await.0,
        "llm/adapters-updated"
    );
    transient.dispose().await.unwrap();
    assert_eq!(
        next_remote_event(&mut events).await.0,
        "llm/adapters-updated"
    );
    signal.abort();
}

#[tokio::test]
async fn model_discovery_carries_only_the_draft_and_redacts_failure_credentials() {
    let harness = Harness::new(None, false, ConfigurationApiProxyOptions::default()).await;
    let observed = Arc::new(Mutex::new(Vec::new()));
    let observed_for_discovery = observed.clone();
    harness
        .llm
        .register_model_discovery("llm-pi-ai", move |request| {
            observed_for_discovery.lock().push((
                request.provider.map(|provider| provider.to_string()),
                request.base_url,
                request.api,
                request.api_key,
            ));
            async {
                Ok(vec![LlmDiscoveredModel {
                    id: "acme-large".into(),
                    name: Some("Acme Large".to_owned()),
                    context_window: Some(65_536),
                    max_tokens: Some(4096),
                }])
            }
            .boxed()
        })
        .unwrap();
    let models = value(
        invoke(
            &harness.runtime,
            RpcMethod::LlmDiscoverModels,
            json!({
                "settingsNs": "llm-pi-ai", "provider": "deepseek",
                "baseURL": "https://gateway.test/v1", "api": "openai-completions",
                "apiKey": "probe-key"
            }),
        )
        .await,
    );
    assert_eq!(models["models"][0]["id"], "acme-large");
    value(
        invoke(
            &harness.runtime,
            RpcMethod::LlmDiscoverModels,
            json!({ "settingsNs": "llm-pi-ai", "provider": "deepseek" }),
        )
        .await,
    );
    value(
        invoke(
            &harness.runtime,
            RpcMethod::LlmDiscoverModels,
            json!({ "settingsNs": "llm-pi-ai", "baseURL": "https://endpoint-only.test" }),
        )
        .await,
    );
    assert_eq!(
        *observed.lock(),
        [
            (
                Some("deepseek".to_owned()),
                Some("https://gateway.test/v1".to_owned()),
                Some("openai-completions".to_owned()),
                Some("probe-key".to_owned())
            ),
            (Some("deepseek".to_owned()), None, None, None),
            (
                None,
                Some("https://endpoint-only.test".to_owned()),
                None,
                None
            ),
        ]
    );
}

#[tokio::test]
async fn model_discovery_failures_name_no_supplied_credential() {
    let harness = Harness::new(None, false, ConfigurationApiProxyOptions::default()).await;
    harness
        .llm
        .register_model_discovery("llm-broken", |_| {
            async { anyhow::bail!("https://gateway.test/models answered 401; check the API key") }
                .boxed()
        })
        .unwrap();
    let rejected = error(
        invoke(
            &harness.runtime,
            RpcMethod::LlmDiscoverModels,
            json!({
                "settingsNs": "llm-broken", "baseURL": "https://gateway.test",
                "apiKey": "wrong-secret"
            }),
        )
        .await,
    );
    assert_eq!(rejected.code, "model-discovery-failed");
    assert!(rejected.message.contains("answered 401; check the API key"));
    assert_eq!(
        rejected.details,
        Map::from_iter([
            ("settingsNs".to_owned(), json!("llm-broken")),
            ("baseURL".to_owned(), json!("https://gateway.test")),
        ])
    );
    assert!(!format!("{rejected:?}").contains("wrong-secret"));

    let missing = error(
        invoke(
            &harness.runtime,
            RpcMethod::LlmDiscoverModels,
            json!({
                "settingsNs": "llm-missing", "baseURL": "https://api.test",
                "apiKey": "must-not-leak"
            }),
        )
        .await,
    );
    assert_eq!(missing.code, "model-discovery-failed");
    assert!(missing.message.contains("no model discovery is registered"));
    assert_eq!(
        missing.details,
        Map::from_iter([
            ("settingsNs".to_owned(), json!("llm-missing")),
            ("baseURL".to_owned(), json!("https://api.test")),
        ])
    );
    assert!(!format!("{missing:?}").contains("must-not-leak"));
}

#[tokio::test]
async fn settings_events_are_forwarded_and_unpolled_streams_release_listeners() {
    let harness = Harness::new(
        Some(MemorySettingsStorage::new(json!({}))),
        false,
        ConfigurationApiProxyOptions::default(),
    )
    .await;
    harness.register_provider_directory();
    harness
        .context
        .get(seekdeep_settings::SETTINGS)
        .unwrap()
        .register(
            &harness.context,
            &settings_namespace("llm-deepseek").unwrap(),
            provider_schema(),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    let baseline = harness.context.events().listener_count(
        &harness.context,
        seekdeep_settings::SETTINGS_DOCUMENT_UPDATED_EVENT,
    );
    let signal = AbortSignal::default();
    let mut events = harness.runtime.host(request(json!({})), signal.clone());
    assert_eq!(
        harness.context.events().listener_count(
            &harness.context,
            seekdeep_settings::SETTINGS_DOCUMENT_UPDATED_EVENT,
        ),
        baseline + 1
    );
    value(
        invoke(
            &harness.runtime,
            RpcMethod::SettingsUpdate,
            json!({ "ns": "llm-deepseek", "patch": { "baseURL": "https://next" } }),
        )
        .await,
    );
    let forwarded = next_remote_event(&mut events).await;
    assert_eq!(
        forwarded.0,
        seekdeep_settings::SETTINGS_DOCUMENT_UPDATED_EVENT
    );
    assert_eq!(forwarded.1[0], "llm-deepseek");
    assert!(forwarded.1[1].is_number());
    signal.abort();
    drop(events);

    let never_polled = harness
        .runtime
        .host(request(json!({})), AbortSignal::default());
    assert_eq!(
        harness.context.events().listener_count(
            &harness.context,
            seekdeep_settings::SETTINGS_DOCUMENT_UPDATED_EVENT,
        ),
        baseline + 2
    );
    drop(never_polled);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if harness.context.events().listener_count(
                &harness.context,
                seekdeep_settings::SETTINGS_DOCUMENT_UPDATED_EVENT,
            ) == baseline
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("configuration stream listeners must dispose on drop");
}

#[tokio::test]
async fn settings_events_forward_product_and_non_remote_write_commits() {
    let harness = Harness::new(
        Some(MemorySettingsStorage::new(json!({}))),
        false,
        ConfigurationApiProxyOptions::default(),
    )
    .await;
    let settings = harness.context.get(seekdeep_settings::SETTINGS).unwrap();
    settings
        .register(
            &harness.context,
            &settings_namespace("permission").unwrap(),
            Schema::object([("defaultPreset", Schema::string())]),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    let default_model = settings
        .register(
            &harness.context,
            &settings_namespace("agent-default-model").unwrap(),
            Schema::object([
                ("provider", Schema::string().required()),
                ("model", Schema::string().required()),
            ]),
            SettingsRegisterOptions {
                base: Some(json!({ "provider": "deepseek", "model": "chat" })),
                ..SettingsRegisterOptions::default()
            },
        )
        .unwrap();
    let signal = AbortSignal::default();
    let mut events = harness.runtime.host(request(json!({})), signal.clone());
    value(
        invoke(
            &harness.runtime,
            RpcMethod::SettingsUpdate,
            json!({ "ns": "permission", "patch": { "defaultPreset": "workspace-write" } }),
        )
        .await,
    );
    assert_eq!(next_remote_event(&mut events).await.1[0], "permission");
    default_model
        .replace(json!({ "provider": "deepseek", "model": "reasoner" }))
        .await
        .unwrap();
    assert_eq!(
        next_remote_event(&mut events).await.1[0],
        "agent-default-model"
    );
    signal.abort();
}
