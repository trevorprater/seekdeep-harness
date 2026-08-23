//! Production API Proxy layer for settings, credentials, and Host-scoped LLM configuration.

use std::{
    collections::{BTreeMap, HashSet},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context as TaskContext, Poll},
};

use futures::{FutureExt as _, StreamExt as _, future::BoxFuture};
use seekdeep_client_connection::{HttpResponse, RpcError, RpcResult};
use seekdeep_cordis::{Context, EventArgs, EventOptions, EventReply, fiber::EffectHandle};
use seekdeep_credentials::{CREDENTIALS, CredentialRef, credential_ref};
use seekdeep_llm::{
    AbortSignal, LLM, LlmConfigurableProvider, LlmDiscoveredModel, LlmModelDiscoveryRequest,
    LlmProviderAuthentication, LlmRuntime, ProviderId,
};
use seekdeep_settings::{
    SETTINGS, SETTINGS_DOCUMENT_UPDATED_EVENT, SettingsApplies as CoreSettingsApplies,
    SettingsConflictError, SettingsDescriptor, SettingsNamespace, SettingsPathOp, SettingsService,
    settings_namespace,
};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::{
    ApiDownlinkStream, ApiProxyRuntime, ClientResponse, RpcId, RpcMethod, RpcReceipt, RpcRequest,
    RpcResponse,
    api::{
        credentials::{
            CredentialView, CredentialsDescribeRequest, CredentialsDescribeValue,
            CredentialsSetRequest, CredentialsUnsetRequest,
        },
        downloads::SessionLogQuery,
        events::{HostFrame, MuxFrame},
        llm::{
            ConfigurableProviderView, DiscoveredModelView, LlmDiscoverModelsRequest,
            LlmDiscoverModelsValue, LlmModelsValue, LlmProvidersValue, ProviderAuthentication,
        },
        sessions::{
            ModelCatalogFailure, ModelCatalogModel, ModelProviderGroup, ModelReasoning,
            ModelReasoningEffort,
        },
        settings::{
            SettingsApplies, SettingsDescribeValue, SettingsMutateRequest, SettingsNamespaceView,
            SettingsPathOpView, SettingsReplaceRequest, SettingsSecretView, SettingsUpdateRequest,
        },
    },
    native_path_opener::{PathOpenerInternals, open_native_text_file},
    service::PathOpener,
};

const WEB_SETTINGS_NAMESPACES: [&str; 7] = [
    "agent-loop",
    "shell",
    "locale",
    "permission",
    "ui-conversation",
    "ui-theme",
    "web-search-deepseek",
];
const PRODUCT_SETTINGS_NAMESPACES: [&str; 2] = ["ui-onboarding", "agent-presets"];
static NEXT_HOST_EVENT_ID: AtomicU64 = AtomicU64::new(1);

/// Native boundary used by the settings-document action.
#[derive(Clone, Default)]
pub struct ConfigurationApiProxyOptions {
    /// Optional injected text-document opener.
    pub open_text_file: Option<PathOpener>,
    /// Platform facts and command runner used by the default text opener.
    pub native_path_opener: PathOpenerInternals,
}

impl std::fmt::Debug for ConfigurationApiProxyOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConfigurationApiProxyOptions")
            .field("has_open_text_file", &self.open_text_file.is_some())
            .field("native_path_opener", &self.native_path_opener)
            .finish_non_exhaustive()
    }
}

/// Configuration-domain decorator over the remaining API Proxy domains.
///
/// Services are resolved from Cordis for every operation rather than captured
/// at construction. A generation swap therefore cannot leave this gateway
/// calling a disposed settings, credentials, or LLM registration.
pub struct ConfigurationApiProxyRuntime {
    context: Context,
    options: ConfigurationApiProxyOptions,
    domains: Arc<dyn ApiProxyRuntime>,
}

impl std::fmt::Debug for ConfigurationApiProxyRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConfigurationApiProxyRuntime")
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

impl ConfigurationApiProxyRuntime {
    /// Builds the configuration layer after proving the required LLM seat is mounted.
    ///
    /// Settings and credentials remain optional, matching the source gateway:
    /// their methods return an actionable business error when no provider is
    /// composed, while every unrelated domain remains usable.
    ///
    /// # Errors
    ///
    /// Returns an error when the required Host LLM registry is absent.
    pub fn from_context(
        context: &Context,
        options: ConfigurationApiProxyOptions,
        domains: Arc<dyn ApiProxyRuntime>,
    ) -> anyhow::Result<Arc<Self>> {
        anyhow::ensure!(context.get(LLM).is_some(), "llm service is required");
        Ok(Arc::new(Self {
            context: context.clone(),
            options,
            domains,
        }))
    }

    async fn configuration_unary(
        &self,
        method: RpcMethod,
        request: RpcRequest<Value>,
        signal: AbortSignal,
    ) -> anyhow::Result<RpcResponse<Value>> {
        match method {
            RpcMethod::SettingsDescribe => self.settings_describe(request),
            RpcMethod::SettingsOpenDocument => self.settings_open_document(request, signal).await,
            RpcMethod::SettingsUpdate => self.settings_update(request).await,
            RpcMethod::SettingsReplace => self.settings_replace(request).await,
            RpcMethod::SettingsMutate => self.settings_mutate(request).await,
            RpcMethod::CredentialsDescribe => self.credentials_describe(request).await,
            RpcMethod::CredentialsSet => self.credentials_set(request).await,
            RpcMethod::CredentialsUnset => self.credentials_unset(request).await,
            RpcMethod::LlmProviders => self.llm_providers(request),
            RpcMethod::LlmModels => self.llm_models(request).await,
            RpcMethod::LlmDiscoverModels => self.llm_discover_models(request, signal).await,
            _ => self.domains.unary(method, request, signal).await,
        }
    }

    fn settings_describe(&self, request: RpcRequest<Value>) -> anyhow::Result<RpcResponse<Value>> {
        let Some(settings) = self.context.get(SETTINGS) else {
            return Ok(settings_absent(request));
        };
        let exposed = self.exposed_namespaces()?;
        let value = SettingsDescribeValue {
            writable: settings.writable(),
            has_document: settings.document_path().is_some(),
            namespaces: settings
                .describe(true)
                .into_iter()
                .filter(|descriptor| exposed.contains(descriptor.ns.as_str()))
                .map(namespace_view)
                .collect(),
        };
        typed_success(request, &value)
    }

    async fn settings_open_document(
        &self,
        request: RpcRequest<Value>,
        signal: AbortSignal,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let Some(settings) = self.context.get(SETTINGS) else {
            return Ok(settings_absent(request));
        };
        if signal.is_aborted() {
            return Ok(failure(
                request,
                "cancelled",
                "settings document open was aborted",
                Map::new(),
            ));
        }
        let path = match settings.prepare_document().await {
            Ok(path) => path,
            Err(_) if signal.is_aborted() => {
                return Ok(failure(
                    request,
                    "cancelled",
                    "settings document preparation was aborted",
                    Map::new(),
                ));
            }
            Err(error) => {
                return Ok(failure(
                    request,
                    "internal",
                    format!("settings document preparation failed: {error}"),
                    Map::new(),
                ));
            }
        };
        let Some(path) = path else {
            return Ok(failure(
                request,
                "internal",
                "settings provider has no local document to open",
                Map::new(),
            ));
        };
        if signal.is_aborted() {
            return Ok(failure(
                request,
                "cancelled",
                "settings document open was aborted",
                Map::new(),
            ));
        }
        let path = path.to_string_lossy().into_owned();
        let result = if let Some(open) = &self.options.open_text_file {
            open(path, signal.clone()).await
        } else {
            open_native_text_file(&path, &signal, &self.options.native_path_opener).await
        };
        match result {
            Ok(()) => typed_success(request, &json!({ "opened": true })),
            Err(_) if signal.is_aborted() => Ok(failure(
                request,
                "cancelled",
                "path open was aborted",
                Map::new(),
            )),
            Err(error) => Ok(failure(
                request,
                "internal",
                format!("path open failed: {error}"),
                Map::new(),
            )),
        }
    }

    async fn settings_update(
        &self,
        request: RpcRequest<Value>,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let payload: SettingsUpdateRequest = payload(&request)?;
        self.settings_write(
            request,
            payload.ns,
            SettingsWrite::Update(Value::Object(payload.patch)),
            payload.expected_revision,
        )
        .await
    }

    async fn settings_replace(
        &self,
        request: RpcRequest<Value>,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let payload: SettingsReplaceRequest = payload(&request)?;
        self.settings_write(
            request,
            payload.ns,
            SettingsWrite::Replace(Value::Object(payload.section)),
            payload.expected_revision,
        )
        .await
    }

    async fn settings_mutate(
        &self,
        request: RpcRequest<Value>,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let payload: SettingsMutateRequest = payload(&request)?;
        let operations = payload
            .ops
            .into_iter()
            .map(|operation| match operation {
                SettingsPathOpView::Set { path, value } => SettingsPathOp::Set { path, value },
                SettingsPathOpView::Unset { path } => SettingsPathOp::Unset { path },
            })
            .collect();
        self.settings_write(
            request,
            payload.ns,
            SettingsWrite::Mutate(operations),
            payload.expected_revision,
        )
        .await
    }

    async fn settings_write(
        &self,
        request: RpcRequest<Value>,
        raw_ns: String,
        write: SettingsWrite,
        expected_revision: Option<f64>,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let Some(settings) = self.context.get(SETTINGS) else {
            return Ok(settings_absent(request));
        };
        let namespace = match settings_namespace(raw_ns.clone()) {
            Ok(namespace) => namespace,
            Err(error) => return Ok(settings_rejected(request, &raw_ns, error)),
        };
        if !self.exposed_namespaces()?.contains(namespace.as_str()) {
            return Ok(failure(
                request,
                "settings-not-exposed",
                format!(
                    "settings namespace \"{}\" is not exposed to configuration clients",
                    namespace.as_str()
                ),
                Map::from_iter([("ns".to_owned(), Value::String(raw_ns))]),
            ));
        }
        let expected_revision =
            match normalize_expected_revision(&settings, &namespace, expected_revision) {
                Ok(revision) => revision,
                Err(RevisionRejection::Conflict { expected, actual }) => {
                    return Ok(settings_conflict(request, &namespace, expected, actual));
                }
                Err(RevisionRejection::Rejected(error)) => {
                    return Ok(settings_rejected(request, &raw_ns, error));
                }
            };
        let verb = write.verb();
        let result = match write {
            SettingsWrite::Update(patch) => {
                settings.update(&namespace, patch, expected_revision).await
            }
            SettingsWrite::Replace(section) => {
                settings
                    .replace(&namespace, section, expected_revision)
                    .await
            }
            SettingsWrite::Mutate(operations) => {
                settings
                    .mutate(&namespace, operations, expected_revision)
                    .await
            }
        };
        if let Err(error) = result {
            if let Some(conflict) = error.downcast_ref::<SettingsConflictError>() {
                return Ok(settings_conflict(
                    request,
                    &namespace,
                    wire_number(conflict.expected),
                    conflict.actual,
                ));
            }
            return Ok(settings_rejected(request, &raw_ns, error));
        }
        let descriptor = settings
            .describe(true)
            .into_iter()
            .find(|descriptor| descriptor.ns == namespace);
        match descriptor {
            Some(descriptor) => typed_success(request, &namespace_view(descriptor)),
            None => Ok(failure(
                request,
                "internal",
                format!(
                    "settings namespace \"{}\" was disposed after the {}",
                    namespace.as_str(),
                    verb
                ),
                Map::new(),
            )),
        }
    }

    async fn credentials_describe(
        &self,
        request: RpcRequest<Value>,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let Some(credentials) = self.context.get(CREDENTIALS) else {
            return Ok(credentials_absent(request));
        };
        let payload: CredentialsDescribeRequest = payload(&request)?;
        let mut views = BTreeMap::new();
        for raw in payload.refs {
            let reference = credential_ref(raw.clone())?;
            let info = credentials.describe(&reference).await?;
            views.insert(
                raw,
                CredentialView {
                    configured: info.configured,
                    source: info.source,
                    writable: info.writable,
                },
            );
        }
        typed_success(request, &CredentialsDescribeValue { credentials: views })
    }

    async fn credentials_set(
        &self,
        request: RpcRequest<Value>,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let Some(credentials) = self.context.get(CREDENTIALS) else {
            return Ok(credentials_absent(request));
        };
        let payload: CredentialsSetRequest = payload(&request)?;
        let reference = match credential_ref(payload.reference.clone()) {
            Ok(reference) => reference,
            Err(error) => {
                return Ok(credential_rejected(request, &payload.reference, error));
            }
        };
        match credentials.set(&reference, &payload.value).await {
            Ok(()) => typed_success(request, &json!({})),
            Err(error) => Ok(credential_rejected(request, &payload.reference, error)),
        }
    }

    async fn credentials_unset(
        &self,
        request: RpcRequest<Value>,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let Some(credentials) = self.context.get(CREDENTIALS) else {
            return Ok(credentials_absent(request));
        };
        let payload: CredentialsUnsetRequest = payload(&request)?;
        let reference = match credential_ref(payload.reference.clone()) {
            Ok(reference) => reference,
            Err(error) => {
                return Ok(credential_rejected(request, &payload.reference, error));
            }
        };
        match credentials.unset(&reference).await {
            Ok(()) => typed_success(request, &json!({})),
            Err(error) => Ok(credential_rejected(request, &payload.reference, error)),
        }
    }

    fn llm_providers(&self, request: RpcRequest<Value>) -> anyhow::Result<RpcResponse<Value>> {
        let llm = self.llm()?;
        let registered = llm.list_providers();
        let active = registered
            .iter()
            .map(|provider| provider.id.as_str().to_owned())
            .collect::<HashSet<_>>();
        let directory = llm.list_configurable_providers();
        let declared = directory
            .iter()
            .map(|entry| entry.provider.as_str().to_owned())
            .collect::<HashSet<_>>();
        let mut providers = directory
            .into_iter()
            .map(|entry| configurable_provider_view(entry, &active))
            .collect::<Vec<_>>();
        providers.extend(
            registered
                .into_iter()
                .filter(|provider| !declared.contains(provider.id.as_str()))
                .map(|provider| ConfigurableProviderView {
                    provider: provider.id.to_string(),
                    display_name: provider.name,
                    settings_ns: String::new(),
                    settings_path: Vec::new(),
                    authentication: None,
                    active: true,
                    declared: None,
                }),
        );
        typed_success(request, &LlmProvidersValue { providers })
    }

    async fn llm_models(&self, request: RpcRequest<Value>) -> anyhow::Result<RpcResponse<Value>> {
        let llm = self.llm()?;
        let (groups, failures) = build_model_catalog(llm).await;
        typed_success(request, &LlmModelsValue { groups, failures })
    }

    async fn llm_discover_models(
        &self,
        request: RpcRequest<Value>,
        signal: AbortSignal,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let payload: LlmDiscoverModelsRequest = payload(&request)?;
        let settings_ns = payload.settings_ns.clone();
        let base_url = payload.base_url.clone();
        let discovery = LlmModelDiscoveryRequest {
            provider: payload.provider.map(ProviderId::new),
            base_url: payload.base_url,
            api: payload.api,
            api_key: payload.api_key,
            signal: Some(signal),
        };
        match self.llm()?.discover_models(&settings_ns, discovery).await {
            Ok(models) => typed_success(
                request,
                &LlmDiscoverModelsValue {
                    models: models.into_iter().map(discovered_model_view).collect(),
                },
            ),
            Err(error) => {
                let mut details =
                    Map::from_iter([("settingsNs".to_owned(), Value::String(settings_ns))]);
                if let Some(base_url) = base_url {
                    details.insert("baseURL".to_owned(), Value::String(base_url));
                }
                Ok(failure(
                    request,
                    "model-discovery-failed",
                    error.to_string(),
                    details,
                ))
            }
        }
    }

    fn llm(&self) -> anyhow::Result<Arc<LlmRuntime>> {
        self.context
            .get(LLM)
            .ok_or_else(|| anyhow::anyhow!("llm service is absent"))
    }

    fn exposed_namespaces(&self) -> anyhow::Result<HashSet<String>> {
        let mut exposed = self
            .llm()?
            .list_configurable_providers()
            .into_iter()
            .map(|provider| provider.settings_ns)
            .collect::<HashSet<_>>();
        exposed.extend(WEB_SETTINGS_NAMESPACES.map(str::to_owned));
        exposed.extend(PRODUCT_SETTINGS_NAMESPACES.map(str::to_owned));
        Ok(exposed)
    }

    fn configuration_host(&self, signal: AbortSignal) -> ApiDownlinkStream<HostFrame> {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut effects = Vec::new();
        let register = (|| -> anyhow::Result<()> {
            let settings_sender = sender.clone();
            effects.push(self.context.events().on_sync(
                &self.context,
                SETTINGS_DOCUMENT_UPDATED_EVENT,
                move |_, args| {
                    let namespace = required_event_arg::<SettingsNamespace>(
                        SETTINGS_DOCUMENT_UPDATED_EVENT,
                        &args,
                        0,
                    )?;
                    let revision =
                        required_event_arg::<u64>(SETTINGS_DOCUMENT_UPDATED_EVENT, &args, 1)?;
                    send_remote_event(
                        &settings_sender,
                        SETTINGS_DOCUMENT_UPDATED_EVENT,
                        vec![
                            Value::String(namespace.to_string()),
                            Value::Number((*revision).into()),
                        ],
                    );
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )?);
            let credentials_sender = sender.clone();
            effects.push(self.context.events().on_sync(
                &self.context,
                "credentials/updated",
                move |_, args| {
                    let reference =
                        required_event_arg::<CredentialRef>("credentials/updated", &args, 0)?;
                    send_remote_event(
                        &credentials_sender,
                        "credentials/updated",
                        vec![Value::String(reference.to_string())],
                    );
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )?);
            effects.push(self.context.events().on_sync(
                &self.context,
                "llm/adapters-updated",
                move |_, args| {
                    anyhow::ensure!(
                        args.is_empty(),
                        "llm/adapters-updated expected no event arguments"
                    );
                    send_remote_event(&sender, "llm/adapters-updated", Vec::new());
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )?);
            Ok(())
        })();
        if let Err(error) = register {
            return EventListenerStream {
                inner: futures::stream::once(async move { Err(error) }).boxed(),
                _guard: EventListenerGuard::new(effects),
            }
            .boxed();
        }
        let stream = async_stream::stream! {
            loop {
                tokio::select! {
                    () = signal.cancelled() => break,
                    frame = receiver.recv() => match frame {
                        Some(frame) => yield Ok(RpcRequest::new(next_host_event_id(), frame)),
                        None => break,
                    },
                }
            }
        };
        EventListenerStream {
            inner: stream.boxed(),
            _guard: EventListenerGuard::new(effects),
        }
        .boxed()
    }
}

impl ApiProxyRuntime for ConfigurationApiProxyRuntime {
    fn unary(
        &self,
        method: RpcMethod,
        request: RpcRequest<Value>,
        signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcResponse<Value>>> {
        let runtime = Arc::new(Self {
            context: self.context.clone(),
            options: self.options.clone(),
            domains: self.domains.clone(),
        });
        async move { runtime.configuration_unary(method, request, signal).await }.boxed()
    }

    fn respond(
        &self,
        message: ClientResponse,
        signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcReceipt>> {
        self.domains.respond(message, signal)
    }

    fn mux(&self, request: RpcRequest<Value>, signal: AbortSignal) -> ApiDownlinkStream<MuxFrame> {
        self.domains.mux(request, signal)
    }

    fn host(
        &self,
        request: RpcRequest<Value>,
        signal: AbortSignal,
    ) -> ApiDownlinkStream<HostFrame> {
        futures::stream::select(
            self.domains.host(request, signal.clone()),
            self.configuration_host(signal),
        )
        .boxed()
    }

    fn session_log(
        &self,
        query: SessionLogQuery,
        signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<HttpResponse>> {
        self.domains.session_log(query, signal)
    }
}

enum SettingsWrite {
    Update(Value),
    Replace(Value),
    Mutate(Vec<SettingsPathOp>),
}

impl SettingsWrite {
    const fn verb(&self) -> &'static str {
        match self {
            Self::Update(_) => "update",
            Self::Replace(_) => "replace",
            Self::Mutate(_) => "mutate",
        }
    }
}

enum RevisionRejection {
    Conflict { expected: f64, actual: u64 },
    Rejected(anyhow::Error),
}

fn normalize_expected_revision(
    settings: &SettingsService,
    namespace: &SettingsNamespace,
    expected: Option<f64>,
) -> Result<Option<u64>, RevisionRejection> {
    let Some(expected) = expected else {
        return Ok(None);
    };
    if expected.is_finite() && expected >= 0.0 && expected.fract() == 0.0 {
        let normalized = if expected == 0.0 {
            Some(0)
        } else {
            expected.to_string().parse::<u64>().ok()
        };
        if let Some(expected) = normalized {
            return Ok(Some(expected));
        }
    }
    let Some(actual) = settings
        .describe(false)
        .into_iter()
        .find(|descriptor| descriptor.ns == *namespace)
        .map(|descriptor| descriptor.revision)
    else {
        return Err(RevisionRejection::Rejected(anyhow::anyhow!(
            "settings namespace \"{namespace}\" is not registered"
        )));
    };
    Err(RevisionRejection::Conflict { expected, actual })
}

fn namespace_view(descriptor: SettingsDescriptor) -> SettingsNamespaceView {
    SettingsNamespaceView {
        ns: descriptor.ns.to_string(),
        schema: descriptor.schema,
        value: descriptor.value,
        base: descriptor.base,
        user: descriptor.user,
        applies: match descriptor.applies {
            CoreSettingsApplies::Live => SettingsApplies::Live,
            CoreSettingsApplies::Restart => SettingsApplies::Restart,
        },
        secrets: descriptor
            .secrets
            .unwrap_or_default()
            .into_iter()
            .map(|secret| SettingsSecretView {
                path: secret.path,
                set: secret.set,
            })
            .collect(),
        revision: wire_number(descriptor.revision),
    }
}

fn configurable_provider_view(
    entry: LlmConfigurableProvider,
    active: &HashSet<String>,
) -> ConfigurableProviderView {
    ConfigurableProviderView {
        provider: entry.provider.to_string(),
        display_name: entry.display_name,
        settings_ns: entry.settings_ns,
        settings_path: entry.settings_path,
        authentication: Some(match entry.authentication {
            LlmProviderAuthentication::ApiKey => ProviderAuthentication::ApiKey,
            LlmProviderAuthentication::ProviderNative => ProviderAuthentication::ProviderNative,
            LlmProviderAuthentication::CodexOauth => ProviderAuthentication::CodexOauth,
        }),
        active: active.contains(entry.provider.as_str()),
        declared: entry.declared,
    }
}

pub(crate) async fn build_model_catalog(
    llm: Arc<LlmRuntime>,
) -> (Vec<ModelProviderGroup>, Vec<ModelCatalogFailure>) {
    let entries = futures::future::join_all(llm.list_providers().into_iter().map(|provider| {
        let llm = llm.clone();
        async move {
            let result = async {
                let models = llm.list_models(provider.id.as_str()).await?;
                let entries = futures::future::try_join_all(models.into_iter().map(|model| {
                    let llm = llm.clone();
                    let provider_id = provider.id.clone();
                    async move {
                        let resolved = llm
                            .resolve_model_info(provider_id.as_str(), model.id.as_str(), None)
                            .await?;
                        let reasoning = resolved.reasoning.map(|reasoning| ModelReasoning {
                            efforts: reasoning
                                .efforts
                                .into_iter()
                                .map(|effort| ModelReasoningEffort {
                                    id: effort.id.to_string(),
                                    name: effort.name,
                                    description: effort.description,
                                })
                                .collect(),
                            default_effort: reasoning
                                .default_effort
                                .map(|effort| effort.to_string()),
                        });
                        Ok::<_, anyhow::Error>(ModelCatalogModel {
                            id: model.id.to_string(),
                            name: model.name,
                            description: model.description,
                            reasoning,
                        })
                    }
                }))
                .await?;
                Ok::<_, anyhow::Error>(ModelProviderGroup {
                    id: provider.id.to_string(),
                    name: provider.name.clone(),
                    models: entries,
                })
            }
            .await;
            (provider, result)
        }
    }))
    .await;
    let mut groups = Vec::new();
    let mut failures = Vec::new();
    for (provider, result) in entries {
        match result {
            Ok(group) if !group.models.is_empty() => groups.push(group),
            Ok(_) => {}
            Err(error) => failures.push(ModelCatalogFailure {
                id: provider.id.to_string(),
                name: provider.name,
                message: error.to_string(),
            }),
        }
    }
    (groups, failures)
}

fn discovered_model_view(model: LlmDiscoveredModel) -> DiscoveredModelView {
    DiscoveredModelView {
        id: model.id.to_string(),
        name: model.name,
        context_window: model.context_window,
        max_tokens: model.max_tokens,
    }
}

fn wire_number(value: u64) -> f64 {
    serde_json::Number::from(value)
        .as_f64()
        .expect("every u64 has a finite JavaScript-number projection")
}

fn payload<T: DeserializeOwned>(request: &RpcRequest<Value>) -> anyhow::Result<T> {
    serde_json::from_value(request.payload.clone()).map_err(Into::into)
}

fn typed_success<T: serde::Serialize>(
    request: RpcRequest<Value>,
    value: &T,
) -> anyhow::Result<RpcResponse<Value>> {
    Ok(success(request, serde_json::to_value(value)?))
}

fn success(request: RpcRequest<Value>, value: Value) -> RpcResponse<Value> {
    RpcResponse::new(request.rpc_id, RpcResult::Success { value: Some(value) })
}

fn failure(
    request: RpcRequest<Value>,
    code: impl Into<String>,
    message: impl Into<String>,
    details: Map<String, Value>,
) -> RpcResponse<Value> {
    RpcResponse::new(
        request.rpc_id,
        RpcResult::Failure {
            error: RpcError {
                code: code.into(),
                message: message.into(),
                details,
            },
        },
    )
}

fn settings_absent(request: RpcRequest<Value>) -> RpcResponse<Value> {
    failure(
        request,
        "internal",
        "settings service is absent: this deployment does not mount a settings provider (e.g. @seekdeep-ai/seekdeep-settings-file) in its composition",
        Map::new(),
    )
}

fn credentials_absent(request: RpcRequest<Value>) -> RpcResponse<Value> {
    failure(
        request,
        "internal",
        "credentials service is absent: this deployment does not mount a credential provider (e.g. @seekdeep-ai/seekdeep-credentials-local) in its composition",
        Map::new(),
    )
}

fn settings_rejected(
    request: RpcRequest<Value>,
    namespace: &str,
    error: impl std::fmt::Display,
) -> RpcResponse<Value> {
    failure(
        request,
        "settings-rejected",
        error.to_string(),
        Map::from_iter([("ns".to_owned(), Value::String(namespace.to_owned()))]),
    )
}

fn settings_conflict(
    request: RpcRequest<Value>,
    namespace: &SettingsNamespace,
    expected: f64,
    actual: u64,
) -> RpcResponse<Value> {
    failure(
        request,
        "settings-conflict",
        format!(
            "settings namespace \"{namespace}\" changed since it was read (expected revision {expected}, now {actual})"
        ),
        Map::from_iter([
            ("ns".to_owned(), Value::String(namespace.to_string())),
            ("expected".to_owned(), json!(expected)),
            ("actual".to_owned(), Value::Number(actual.into())),
        ]),
    )
}

fn credential_rejected(
    request: RpcRequest<Value>,
    reference: &str,
    error: impl std::fmt::Display,
) -> RpcResponse<Value> {
    failure(
        request,
        "credential-rejected",
        error.to_string(),
        Map::from_iter([("ref".to_owned(), Value::String(reference.to_owned()))]),
    )
}

fn required_event_arg<T: std::any::Any + Send + Sync>(
    event: &str,
    args: &EventArgs,
    index: usize,
) -> anyhow::Result<Arc<T>> {
    args.get(index)
        .ok_or_else(|| anyhow::anyhow!("{event} argument {index} has the wrong type or is absent"))
}

fn send_remote_event(
    sender: &tokio::sync::mpsc::UnboundedSender<HostFrame>,
    event: &str,
    args: Vec<Value>,
) {
    let _ = sender.send(HostFrame::RemoteEvent {
        event: event.to_owned(),
        args,
    });
}

fn next_host_event_id() -> RpcId {
    RpcId::new(format!(
        "host-configuration-{}",
        NEXT_HOST_EVENT_ID.fetch_add(1, Ordering::Relaxed)
    ))
}

struct EventListenerGuard {
    effects: Option<Vec<EffectHandle>>,
}

struct EventListenerStream {
    inner: ApiDownlinkStream<HostFrame>,
    _guard: EventListenerGuard,
}

impl futures::Stream for EventListenerStream {
    type Item = anyhow::Result<RpcRequest<HostFrame>>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
    ) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(context)
    }
}

impl EventListenerGuard {
    const fn new(effects: Vec<EffectHandle>) -> Self {
        Self {
            effects: Some(effects),
        }
    }
}

impl Drop for EventListenerGuard {
    fn drop(&mut self) {
        let Some(effects) = self.effects.take() else {
            return;
        };
        let dispose = async move {
            for effect in effects.into_iter().rev() {
                if let Err(error) = effect.dispose().await {
                    tracing::warn!(%error, "API Proxy event-listener disposal failed");
                }
            }
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(dispose);
        } else {
            std::thread::spawn(move || futures::executor::block_on(dispose));
        }
    }
}
