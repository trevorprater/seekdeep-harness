//! Package plugin lifecycle, dynamic settings, credentials, directory, and discovery.

use std::{collections::HashMap, error::Error, fmt, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use futures::FutureExt as _;
use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_attachment::{ATTACHMENTS, AttachmentStore};
use seekdeep_cordis::{
    Context, EventOptions, EventReply, Plugin,
    fiber::{DisposeFuture, EffectHandle},
};
use seekdeep_credentials::CREDENTIALS;
use seekdeep_llm::{
    AdapterRegistrationHandle, LLM, LlmConfigurableProvider, LlmError, LlmProviderAuthentication,
    ModelDiscoveryHandle, ProviderId, assert_usable_api_key,
};
use seekdeep_settings::{
    SettingsSectionSource, deep_equal_json, install_settings_section, settings_namespace,
};
use seekdeep_util::launch_environment::{
    LaunchEnvironmentSnapshot, LaunchEnvironmentSource, launch_environment_of,
};
use serde_json::{Value, json};

use crate::{
    adapter::{
        PiAiAdapter, PiAiAdapterOptions, PiApiKeyResolver, PiAttachmentResolver, PiProfileSource,
        PiResolvedAuth,
    },
    catalog::{CatalogIndex, builtin_catalog},
    codex_auth::{
        CodexCredentialBridge, CodexOAuthRefresher, OPENAI_CODEX_PROVIDER_ID,
        create_codex_credential_bridge,
    },
    config::{
        ResolvedPiProviderProfile, assert_serviceable, config_schema, materialize_config,
        resolve_config,
    },
    discovery::{StoredApiKeyResolver, discover_models},
    executor::NativePiExecutor,
};

/// Cordis plugin identity and settings namespace.
pub const NAME: &str = "llm-pi-ai";
/// Required runtime service.
pub const INJECT: [&str; 1] = ["llm"];

/// Builds the package plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT, move |context, config| {
        Box::pin(async move { install(&context, config).await })
    })
    .with_config_validator(|value| {
        let materialized = materialize_config(value)?;
        assert_serviceable(&materialized)?;
        ensure_native_serviceable(resolve_config(&materialized)?.values())?;
        Ok(materialized)
    })
}

struct DynamicProfiles {
    static_raw: Value,
    source: Mutex<Option<SettingsSectionSource>>,
    cache: Mutex<(Value, Arc<IndexMap<String, ResolvedPiProviderProfile>>)>,
}

impl DynamicProfiles {
    fn current_raw(&self) -> Value {
        self.source
            .lock()
            .as_ref()
            .map_or_else(|| self.static_raw.clone(), SettingsSectionSource::get)
    }
}

impl PiProfileSource for DynamicProfiles {
    fn profiles(&self) -> Arc<IndexMap<String, ResolvedPiProviderProfile>> {
        let raw = self.current_raw();
        let mut cache = self.cache.lock();
        if deep_equal_json(&cache.0, &raw) {
            return cache.1.clone();
        }
        match resolve_config(&raw).and_then(|profiles| {
            ensure_native_serviceable(profiles.values())?;
            Ok(profiles)
        }) {
            Ok(profiles) => {
                let profiles = Arc::new(profiles);
                *cache = (raw, profiles.clone());
                profiles
            }
            Err(error) => {
                tracing::error!(%error, "llm-pi-ai received an unserviceable validated settings snapshot");
                cache.1.clone()
            }
        }
    }
}

struct ContextApiKeys {
    context: Context,
    environment: Arc<LaunchEnvironmentSnapshot>,
    codex: CodexCredentialBridge,
    codex_refresher: CodexOAuthRefresher,
}

#[async_trait]
impl PiApiKeyResolver for ContextApiKeys {
    async fn resolve(
        &self,
        provider: &ProviderId,
        profile: &ResolvedPiProviderProfile,
    ) -> anyhow::Result<PiResolvedAuth> {
        let Some(reference) = &profile.api_key_env else {
            if provider.as_str() == OPENAI_CODEX_PROVIDER_ID {
                return match self.codex.resolve_oauth(&self.codex_refresher).await {
                    Ok(Some(credential)) => {
                        Ok(PiResolvedAuth::api_key(Some(credential.access)))
                    }
                    Ok(None) => Err(LlmError::simple(
                        format!(
                            "llm-pi-ai: no file-backed ChatGPT OAuth session at {}; run codex login with cli_auth_credentials_store = \"file\", then retry",
                            self.codex.display_path
                        ),
                        "MISSING_CREDENTIAL",
                    ).into()),
                    Err(error) => Err(LlmError::simple(
                        format!(
                            "llm-pi-ai: cannot use the ChatGPT OAuth session at {}",
                            self.codex.display_path
                        ),
                        "INVALID_CREDENTIAL",
                    ).with_cause(AnyhowCause(error)).into()),
                };
            }
            return self.resolve_ambient(provider.as_str()).await;
        };
        let hit = if let Some(credentials) = self.context.get(CREDENTIALS) {
            credentials.resolve(reference).await?.map(|hit| hit.value)
        } else {
            self.environment
                .get(reference.as_str())
                .map(|hit| hit.value)
        };
        if let Some(hit) = hit.filter(|hit| !hit.is_empty()) {
            let key = assert_usable_api_key(&hit, NAME, reference.as_str())?;
            return Ok(match provider.as_str() {
                "cloudflare-workers-ai" => self.cloudflare_auth(false, key),
                "cloudflare-ai-gateway" => self.cloudflare_auth(true, key),
                _ => PiResolvedAuth::api_key(Some(key)),
            });
        }
        Err(LlmError::simple(
            format!(
                "llm-pi-ai: no credential for provider route \"{}\"; its profile resolves {}, which is not set — store {} through the credentials service (the web Models page writes it) or export it, and remove apiKeyEnv only if this provider should authenticate from pi-ai's own environment discovery",
                provider.as_str(),
                reference.as_str(),
                reference.as_str()
            ),
            "MISSING_CREDENTIAL",
        )
        .into())
    }
}

impl ContextApiKeys {
    fn process_value(&self, name: &str) -> Option<String> {
        self.environment
            .get_from(name, &[LaunchEnvironmentSource::Process])
            .map(|entry| entry.value)
            .filter(|value| !value.is_empty())
    }

    async fn resolve_ambient(&self, provider: &str) -> anyhow::Result<PiResolvedAuth> {
        match provider {
            "anthropic" => Ok(self.anthropic_ambient()),
            "amazon-bedrock" => Ok(self.bedrock_ambient()),
            "google-vertex" => self.vertex_ambient().await,
            "cloudflare-workers-ai" => Ok(self.cloudflare_ambient(false)),
            "cloudflare-ai-gateway" => Ok(self.cloudflare_ambient(true)),
            provider => Ok(ambient_key_names(provider)
                .iter()
                .find_map(|name| self.process_value(name))
                .map_or_else(PiResolvedAuth::default, |key| {
                    PiResolvedAuth::api_key(Some(key))
                })),
        }
    }

    fn anthropic_ambient(&self) -> PiResolvedAuth {
        if let Some(token) = self.process_value("ANTHROPIC_AUTH_TOKEN") {
            return PiResolvedAuth {
                configured: true,
                api_key: None,
                headers: HashMap::from([(
                    "Authorization".to_owned(),
                    Some(format!("Bearer {token}")),
                )]),
                environment: HashMap::new(),
            };
        }
        ["ANTHROPIC_OAUTH_TOKEN", "ANTHROPIC_API_KEY"]
            .iter()
            .find_map(|name| self.process_value(name))
            .map_or_else(PiResolvedAuth::default, |key| {
                PiResolvedAuth::api_key(Some(key))
            })
    }

    fn bedrock_ambient(&self) -> PiResolvedAuth {
        let names = [
            "AWS_PROFILE",
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "AWS_REGION",
            "AWS_DEFAULT_REGION",
            "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
            "AWS_CONTAINER_CREDENTIALS_FULL_URI",
            "AWS_WEB_IDENTITY_TOKEN_FILE",
            "AWS_ROLE_ARN",
            "AWS_ROLE_SESSION_NAME",
            "AWS_BEDROCK_FORCE_CACHE",
            "AWS_BEDROCK_FORCE_HTTP1",
            "PI_CACHE_RETENTION",
            "HTTPS_PROXY",
            "HTTP_PROXY",
            "NO_PROXY",
        ];
        let environment = names
            .into_iter()
            .filter_map(|name| {
                self.process_value(name)
                    .map(|value| (name.to_owned(), value))
            })
            .collect::<HashMap<_, _>>();
        if let Some(token) = self.process_value("AWS_BEARER_TOKEN_BEDROCK") {
            return PiResolvedAuth {
                configured: true,
                api_key: Some(token),
                headers: HashMap::new(),
                environment,
            };
        }
        let configured = environment.contains_key("AWS_PROFILE")
            || (environment.contains_key("AWS_ACCESS_KEY_ID")
                && environment.contains_key("AWS_SECRET_ACCESS_KEY"))
            || environment.contains_key("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI")
            || environment.contains_key("AWS_CONTAINER_CREDENTIALS_FULL_URI")
            || environment.contains_key("AWS_WEB_IDENTITY_TOKEN_FILE");
        PiResolvedAuth {
            configured,
            api_key: None,
            headers: HashMap::new(),
            environment,
        }
    }

    async fn vertex_ambient(&self) -> anyhow::Result<PiResolvedAuth> {
        if let Some(key) = self.process_value("GOOGLE_CLOUD_API_KEY") {
            return Ok(PiResolvedAuth::api_key(Some(key)));
        }
        let project = self
            .process_value("GOOGLE_CLOUD_PROJECT")
            .or_else(|| self.process_value("GCLOUD_PROJECT"));
        let location = self.process_value("GOOGLE_CLOUD_LOCATION");
        let explicit = self
            .process_value("GOOGLE_APPLICATION_CREDENTIALS")
            .map(PathBuf::from);
        let credentials_path = explicit.clone().or_else(|| {
            dirs::home_dir()
                .map(|home| home.join(".config/gcloud/application_default_credentials.json"))
        });
        let has_credentials = match credentials_path.as_ref() {
            Some(path) => tokio::fs::metadata(path)
                .await
                .is_ok_and(|metadata| metadata.is_file()),
            None => false,
        };
        let configured = has_credentials && project.is_some() && location.is_some();
        let mut environment = HashMap::new();
        if let Some(project) = project {
            environment.insert("GOOGLE_CLOUD_PROJECT".to_owned(), project);
        }
        if let Some(location) = location {
            environment.insert("GOOGLE_CLOUD_LOCATION".to_owned(), location);
        }
        if let Some(path) = explicit {
            environment.insert(
                "GOOGLE_APPLICATION_CREDENTIALS".to_owned(),
                path.to_string_lossy().into_owned(),
            );
        }
        Ok(PiResolvedAuth {
            configured,
            api_key: None,
            headers: HashMap::new(),
            environment,
        })
    }

    fn cloudflare_ambient(&self, gateway: bool) -> PiResolvedAuth {
        let Some(key) = self.process_value("CLOUDFLARE_API_KEY") else {
            return PiResolvedAuth::default();
        };
        self.cloudflare_auth(gateway, key)
    }

    fn cloudflare_auth(&self, gateway: bool, key: String) -> PiResolvedAuth {
        let Some(account) = self.process_value("CLOUDFLARE_ACCOUNT_ID") else {
            return PiResolvedAuth::default();
        };
        let gateway_id = gateway
            .then(|| self.process_value("CLOUDFLARE_GATEWAY_ID"))
            .flatten();
        if gateway && gateway_id.is_none() {
            return PiResolvedAuth::default();
        }
        let mut environment = HashMap::from([("CLOUDFLARE_ACCOUNT_ID".to_owned(), account)]);
        if let Some(gateway_id) = gateway_id {
            environment.insert("CLOUDFLARE_GATEWAY_ID".to_owned(), gateway_id);
        }
        if gateway {
            PiResolvedAuth {
                configured: true,
                api_key: None,
                headers: HashMap::from([
                    (
                        "cf-aig-authorization".to_owned(),
                        Some(format!("Bearer {key}")),
                    ),
                    ("Authorization".to_owned(), None),
                    ("x-api-key".to_owned(), None),
                ]),
                environment,
            }
        } else {
            PiResolvedAuth {
                configured: true,
                api_key: Some(key),
                headers: HashMap::new(),
                environment,
            }
        }
    }
}

fn ambient_key_names(provider: &str) -> &'static [&'static str] {
    match provider {
        "ant-ling" => &["ANT_LING_API_KEY"],
        "azure-openai-responses" => &["AZURE_OPENAI_API_KEY"],
        "cerebras" => &["CEREBRAS_API_KEY"],
        "deepseek" => &["DEEPSEEK_API_KEY"],
        "fireworks" => &["FIREWORKS_API_KEY"],
        "github-copilot" => &["COPILOT_GITHUB_TOKEN"],
        "google" => &["GEMINI_API_KEY"],
        "groq" => &["GROQ_API_KEY"],
        "huggingface" => &["HF_TOKEN"],
        "kimi-coding" => &["KIMI_API_KEY"],
        "minimax" => &["MINIMAX_API_KEY"],
        "minimax-cn" => &["MINIMAX_CN_API_KEY"],
        "mistral" => &["MISTRAL_API_KEY"],
        "moonshotai" | "moonshotai-cn" => &["MOONSHOT_API_KEY"],
        "nvidia" => &["NVIDIA_API_KEY"],
        "openai" => &["OPENAI_API_KEY"],
        "opencode" | "opencode-go" => &["OPENCODE_API_KEY"],
        "openrouter" => &["OPENROUTER_API_KEY"],
        "qwen-token-plan" => &["QWEN_TOKEN_PLAN_API_KEY"],
        "qwen-token-plan-cn" => &["QWEN_TOKEN_PLAN_CN_API_KEY"],
        "radius" => &["RADIUS_API_KEY"],
        "together" => &["TOGETHER_API_KEY"],
        "vercel-ai-gateway" => &["AI_GATEWAY_API_KEY"],
        "xai" => &["XAI_API_KEY"],
        "xiaomi" => &["XIAOMI_API_KEY"],
        "xiaomi-token-plan-cn" => &["XIAOMI_TOKEN_PLAN_CN_API_KEY"],
        "xiaomi-token-plan-ams" => &["XIAOMI_TOKEN_PLAN_AMS_API_KEY"],
        "xiaomi-token-plan-sgp" => &["XIAOMI_TOKEN_PLAN_SGP_API_KEY"],
        "zai" => &["ZAI_API_KEY"],
        "zai-coding-cn" => &["ZAI_CODING_CN_API_KEY"],
        _ => &[],
    }
}

struct AnyhowCause(anyhow::Error);
impl fmt::Debug for AnyhowCause {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, formatter)
    }
}
impl fmt::Display for AnyhowCause {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}
impl Error for AnyhowCause {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.0.as_ref())
    }
}

struct ContextAttachments(Context);
impl PiAttachmentResolver for ContextAttachments {
    fn resolve(&self) -> Option<AttachmentStore> {
        self.0.get(ATTACHMENTS).map(|store| (*store).clone())
    }
}

struct StoredRouteKey {
    resolver: Arc<ContextApiKeys>,
    profiles: Arc<DynamicProfiles>,
    provider: Option<ProviderId>,
}

#[async_trait]
impl StoredApiKeyResolver for StoredRouteKey {
    async fn resolve(&self) -> anyhow::Result<Option<String>> {
        let Some(provider) = &self.provider else {
            return Ok(None);
        };
        let profiles = self.profiles.profiles();
        let Some(profile) = profiles.get(provider.as_str()) else {
            return Ok(None);
        };
        Ok(self.resolver.resolve(provider, profile).await?.api_key)
    }
}

struct RegistrationState {
    adapter: Option<Arc<AdapterRegistrationHandle>>,
    adapter_facts: Value,
    directory: Arc<seekdeep_llm::DirectoryRegistrationHandle>,
    directory_facts: Value,
    discovery: Option<Arc<ModelDiscoveryHandle>>,
}

#[allow(clippy::too_many_lines)] // One installation transaction owns every published handle.
async fn install(context: &Context, config: Value) -> anyhow::Result<()> {
    let runtime = context
        .get(LLM)
        .ok_or_else(|| anyhow::anyhow!("llm-pi-ai requires the llm service"))?;
    let environment = launch_environment_of(context);
    let initial = Arc::new(resolve_config(&config)?);
    ensure_native_serviceable(initial.values())?;
    let dynamic = Arc::new(DynamicProfiles {
        static_raw: config.clone(),
        source: Mutex::new(None),
        cache: Mutex::new((config.clone(), initial)),
    });
    let http = reqwest::Client::new();
    let keys = Arc::new(ContextApiKeys {
        context: context.clone(),
        codex: create_codex_credential_bridge(&environment),
        codex_refresher: CodexOAuthRefresher::new(http.clone()),
        environment,
    });
    let executor = Arc::new(NativePiExecutor::new(http));
    let adapter = Arc::new(PiAiAdapter::new(PiAiAdapterOptions {
        profiles: dynamic.clone(),
        api_keys: keys.clone(),
        executor: executor.clone(),
        attachments: Some(Arc::new(ContextAttachments(context.clone()))),
    }));
    let session_executor = executor;
    context.events().on(
        context,
        "session/disposed",
        move |_, args| {
            let executor = session_executor.clone();
            Box::pin(async move {
                if let Some(session) = args.get::<seekdeep_core::session::Session>(0) {
                    executor.close_session(session.id()).await;
                }
                Ok(EventReply::Undefined)
            })
        },
        EventOptions {
            global: true,
            ..EventOptions::default()
        },
    )?;

    let directory_entries = directory_entries(&dynamic.profiles(), builtin_catalog());
    let directory = Arc::new(runtime.register_configurable_providers(&directory_entries)?);
    let state = Arc::new(Mutex::new(RegistrationState {
        adapter: None,
        adapter_facts: Value::Null,
        directory,
        directory_facts: serde_json::to_value(&directory_entries)?,
        discovery: None,
    }));
    ensure_adapter(&runtime, &adapter, &dynamic, &state)?;

    let discovery_profiles = dynamic.clone();
    let discovery_keys = keys.clone();
    let discovery = Arc::new(runtime.register_model_discovery(NAME, move |request| {
        let profiles = discovery_profiles.clone();
        let keys = discovery_keys.clone();
        async move {
            let stored = StoredRouteKey {
                resolver: keys,
                profiles,
                provider: request.provider.clone(),
            };
            discover_models(
                &reqwest::Client::new(),
                builtin_catalog(),
                &request,
                Some(&stored),
            )
            .await
        }
        .boxed()
    })?);
    state.lock().discovery = Some(discovery);

    let cleanup_state = state.clone();
    let cleanup = EffectHandle::new("llm-pi-ai registrations", move || -> DisposeFuture {
        let state = cleanup_state.clone();
        Box::pin(async move {
            let (adapter, directory, discovery) = {
                let mut state = state.lock();
                (
                    state.adapter.take(),
                    state.directory.clone(),
                    state.discovery.take(),
                )
            };
            if let Some(adapter) = adapter {
                adapter.dispose().await?;
            }
            if let Some(discovery) = discovery {
                discovery.dispose().await?;
            }
            directory.dispose().await?;
            Ok(())
        })
    });
    if let Err(error) = context.own(cleanup.clone()) {
        cleanup.dispose().await?;
        return Err(error.into());
    }

    let change_runtime = runtime.clone();
    let change_adapter = adapter;
    let change_profiles = dynamic.clone();
    let change_state = state;
    let on_change = Arc::new(move || {
        if let Err(error) = ensure_adapter(
            &change_runtime,
            &change_adapter,
            &change_profiles,
            &change_state,
        ) {
            tracing::error!(%error, "llm-pi-ai kept the previously registered routes after a refused update");
        }
        if let Err(error) = ensure_directory(&change_profiles, &change_state, builtin_catalog()) {
            tracing::error!(%error, "llm-pi-ai kept the previous configurable-provider directory after a refused update");
        }
        Ok(())
    });
    let installed = install_settings_section(
        context,
        &settings_namespace(NAME)?,
        config_schema(),
        config,
        Some(Arc::new(|value| {
            assert_serviceable(value)?;
            ensure_native_serviceable(resolve_config(value)?.values())
        })),
        on_change,
    )?;
    *dynamic.source.lock() = Some(installed.source);
    installed.fiber.await_settled().await
}

fn ensure_adapter(
    runtime: &Arc<seekdeep_llm::LlmRuntime>,
    adapter: &Arc<PiAiAdapter>,
    profiles: &Arc<DynamicProfiles>,
    state: &Arc<Mutex<RegistrationState>>,
) -> anyhow::Result<()> {
    let profiles = profiles.profiles();
    let facts = registration_facts(&profiles);
    if deep_equal_json(&state.lock().adapter_facts, &facts) {
        return Ok(());
    }
    let routes = profiles.keys().cloned().collect::<Vec<_>>();
    let existing = state.lock().adapter.clone();
    let next = if let Some(existing) = existing {
        existing.replace(&routes)?;
        Some(existing)
    } else if routes.is_empty() {
        None
    } else {
        Some(Arc::new(
            runtime.register_adapter(&routes, adapter.clone())?,
        ))
    };
    let mut state = state.lock();
    state.adapter = next;
    state.adapter_facts = facts;
    Ok(())
}

fn ensure_directory(
    profiles: &Arc<DynamicProfiles>,
    state: &Arc<Mutex<RegistrationState>>,
    catalog: &CatalogIndex,
) -> anyhow::Result<()> {
    let entries = directory_entries(&profiles.profiles(), catalog);
    let facts = serde_json::to_value(&entries)?;
    let state_guard = state.lock();
    if deep_equal_json(&state_guard.directory_facts, &facts) {
        return Ok(());
    }
    let directory = state_guard.directory.clone();
    drop(state_guard);
    directory.replace(&entries)?;
    state.lock().directory_facts = facts;
    Ok(())
}

fn registration_facts(profiles: &IndexMap<String, ResolvedPiProviderProfile>) -> Value {
    let mut entries = profiles
        .iter()
        .map(|(provider, profile)| {
            json!({
                "provider":provider,
                "displayName":profile.display_name,
                "retryPolicy":profile.retry_policy,
            })
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left["provider"].as_str().cmp(&right["provider"].as_str()));
    Value::Array(entries)
}

fn directory_entries(
    profiles: &IndexMap<String, ResolvedPiProviderProfile>,
    catalog: &CatalogIndex,
) -> Vec<LlmConfigurableProvider> {
    let mut entries = Vec::<LlmConfigurableProvider>::new();
    for provider in catalog.provider_ids() {
        let Some(installed) = catalog.provider(provider) else {
            continue;
        };
        if (installed.api_key_name.is_none() && provider != OPENAI_CODEX_PROVIDER_ID)
            || !models_are_native(&installed.models)
        {
            continue;
        }
        entries.push(directory_entry(
            provider,
            provider,
            if provider == OPENAI_CODEX_PROVIDER_ID {
                LlmProviderAuthentication::CodexOauth
            } else {
                LlmProviderAuthentication::ProviderNative
            },
            false,
        ));
    }
    for (provider, profile) in profiles {
        let entry = directory_entry(
            provider,
            &profile.display_name,
            if profile.api_key_env.is_some() {
                LlmProviderAuthentication::ApiKey
            } else if provider == OPENAI_CODEX_PROVIDER_ID {
                LlmProviderAuthentication::CodexOauth
            } else {
                LlmProviderAuthentication::ProviderNative
            },
            catalog.provider(provider).is_none(),
        );
        if let Some(index) = entries
            .iter()
            .position(|candidate| candidate.provider.as_str() == provider)
        {
            entries[index] = entry;
        } else {
            entries.push(entry);
        }
    }
    entries
}

fn directory_entry(
    provider: &str,
    display_name: &str,
    authentication: LlmProviderAuthentication,
    declared: bool,
) -> LlmConfigurableProvider {
    LlmConfigurableProvider {
        provider: ProviderId::new(provider),
        display_name: display_name.to_owned(),
        settings_ns: NAME.to_owned(),
        settings_path: vec!["providers".to_owned(), provider.to_owned()],
        authentication,
        declared: Some(declared),
    }
}

fn ensure_native_serviceable<'a>(
    profiles: impl IntoIterator<Item = &'a ResolvedPiProviderProfile>,
) -> anyhow::Result<()> {
    for profile in profiles {
        if !models_are_native(&profile.pi_provider.models) {
            let apis = profile
                .pi_provider
                .models
                .iter()
                .map(|model| model.api.as_str())
                .collect::<Vec<_>>();
            anyhow::bail!(
                "llm-pi-ai: provider \"{}\" uses an unported native protocol in {:?}",
                profile.provider.as_str(),
                apis
            );
        }
        if profile.pi_provider.models.iter().any(|model| {
            model.api.as_str() == "azure-openai-responses" && model.base_url.is_empty()
        }) {
            anyhow::bail!(
                "llm-pi-ai: provider \"{}\" needs a non-empty Azure OpenAI baseURL in this Rust build",
                profile.provider.as_str()
            );
        }
    }
    Ok(())
}

fn models_are_native(models: &[crate::catalog::PiModel]) -> bool {
    models.iter().all(|model| {
        matches!(
            model.api.as_str(),
            "openai-completions"
                | "openai-responses"
                | "azure-openai-responses"
                | "openai-codex-responses"
                | "mistral-conversations"
                | "anthropic-messages"
                | "google-generative-ai"
                | "google-vertex"
                | "bedrock-converse-stream"
        )
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use seekdeep_util::launch_environment::{
        LaunchEnvironmentLayerInput, create_launch_environment_snapshot,
    };

    use super::*;

    fn catalog_provider(id: &str, api_key: bool) -> crate::catalog::CatalogProvider {
        crate::catalog::CatalogProvider {
            id: ProviderId::new(id),
            name: id.to_owned(),
            base_url: None,
            listed: true,
            api_key_name: api_key.then(|| format!("{id} key")),
            oauth: (!api_key).then(|| crate::catalog::CatalogOAuth {
                name: format!("{id} OAuth"),
                login_label: None,
            }),
            models: Vec::new(),
        }
    }

    fn resolver(values: &[(&str, &str)]) -> ContextApiKeys {
        let environment = Arc::new(create_launch_environment_snapshot(&[
            LaunchEnvironmentLayerInput {
                source: LaunchEnvironmentSource::Process,
                path: None,
                values: values
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                    .collect::<BTreeMap<_, _>>(),
            },
        ]));
        ContextApiKeys {
            context: Context::new(),
            codex: create_codex_credential_bridge(&environment),
            codex_refresher: CodexOAuthRefresher::new(reqwest::Client::new()),
            environment,
        }
    }

    #[tokio::test]
    async fn ambient_auth_matches_generic_anthropic_cloudflare_bedrock_and_vertex_rules() {
        let generic = resolver(&[("OPENAI_API_KEY", "openai-key")]);
        assert_eq!(
            generic.resolve_ambient("openai").await.unwrap(),
            PiResolvedAuth::api_key(Some("openai-key".to_owned()))
        );
        assert!(!generic.resolve_ambient("acme").await.unwrap().configured);

        let anthropic = resolver(&[
            ("ANTHROPIC_AUTH_TOKEN", "auth-token"),
            ("ANTHROPIC_API_KEY", "lower-priority"),
        ])
        .resolve_ambient("anthropic")
        .await
        .unwrap();
        assert!(anthropic.configured);
        assert!(anthropic.api_key.is_none());
        assert_eq!(
            anthropic.headers["Authorization"].as_deref(),
            Some("Bearer auth-token")
        );

        let cloudflare = resolver(&[
            ("CLOUDFLARE_API_KEY", "cloudflare-key"),
            ("CLOUDFLARE_ACCOUNT_ID", "account"),
            ("CLOUDFLARE_GATEWAY_ID", "gateway"),
        ])
        .resolve_ambient("cloudflare-ai-gateway")
        .await
        .unwrap();
        assert!(cloudflare.configured);
        assert!(cloudflare.api_key.is_none());
        assert_eq!(cloudflare.environment["CLOUDFLARE_ACCOUNT_ID"], "account");
        assert_eq!(cloudflare.environment["CLOUDFLARE_GATEWAY_ID"], "gateway");
        assert_eq!(
            cloudflare.headers["cf-aig-authorization"].as_deref(),
            Some("Bearer cloudflare-key")
        );
        assert_eq!(cloudflare.headers["Authorization"], None);

        let bedrock = resolver(&[
            ("AWS_ACCESS_KEY_ID", "access"),
            ("AWS_SECRET_ACCESS_KEY", "secret"),
            ("AWS_SESSION_TOKEN", "session"),
            ("AWS_REGION", "us-west-2"),
        ])
        .resolve_ambient("amazon-bedrock")
        .await
        .unwrap();
        assert!(bedrock.configured);
        assert!(bedrock.api_key.is_none());
        assert_eq!(bedrock.environment["AWS_ACCESS_KEY_ID"], "access");
        assert_eq!(bedrock.environment["AWS_REGION"], "us-west-2");

        let home = tempfile::tempdir().unwrap();
        let credentials = home.path().join("adc.json");
        tokio::fs::write(&credentials, b"{}").await.unwrap();
        let vertex = resolver(&[
            ("GOOGLE_CLOUD_PROJECT", "project"),
            ("GOOGLE_CLOUD_LOCATION", "us-central1"),
            (
                "GOOGLE_APPLICATION_CREDENTIALS",
                credentials.to_str().unwrap(),
            ),
        ])
        .resolve_ambient("google-vertex")
        .await
        .unwrap();
        assert!(vertex.configured);
        assert!(vertex.api_key.is_none());
        assert_eq!(vertex.environment["GOOGLE_CLOUD_PROJECT"], "project");
        assert_eq!(
            vertex.environment["GOOGLE_APPLICATION_CREDENTIALS"],
            credentials.to_string_lossy()
        );
    }

    #[test]
    fn generic_ambient_key_table_covers_every_pinned_standard_provider() {
        let expected = [
            ("ant-ling", "ANT_LING_API_KEY"),
            ("azure-openai-responses", "AZURE_OPENAI_API_KEY"),
            ("cerebras", "CEREBRAS_API_KEY"),
            ("deepseek", "DEEPSEEK_API_KEY"),
            ("fireworks", "FIREWORKS_API_KEY"),
            ("github-copilot", "COPILOT_GITHUB_TOKEN"),
            ("google", "GEMINI_API_KEY"),
            ("groq", "GROQ_API_KEY"),
            ("huggingface", "HF_TOKEN"),
            ("kimi-coding", "KIMI_API_KEY"),
            ("minimax", "MINIMAX_API_KEY"),
            ("minimax-cn", "MINIMAX_CN_API_KEY"),
            ("mistral", "MISTRAL_API_KEY"),
            ("moonshotai", "MOONSHOT_API_KEY"),
            ("moonshotai-cn", "MOONSHOT_API_KEY"),
            ("nvidia", "NVIDIA_API_KEY"),
            ("openai", "OPENAI_API_KEY"),
            ("opencode", "OPENCODE_API_KEY"),
            ("opencode-go", "OPENCODE_API_KEY"),
            ("openrouter", "OPENROUTER_API_KEY"),
            ("qwen-token-plan", "QWEN_TOKEN_PLAN_API_KEY"),
            ("qwen-token-plan-cn", "QWEN_TOKEN_PLAN_CN_API_KEY"),
            ("radius", "RADIUS_API_KEY"),
            ("together", "TOGETHER_API_KEY"),
            ("vercel-ai-gateway", "AI_GATEWAY_API_KEY"),
            ("xai", "XAI_API_KEY"),
            ("xiaomi", "XIAOMI_API_KEY"),
            ("xiaomi-token-plan-cn", "XIAOMI_TOKEN_PLAN_CN_API_KEY"),
            ("xiaomi-token-plan-ams", "XIAOMI_TOKEN_PLAN_AMS_API_KEY"),
            ("xiaomi-token-plan-sgp", "XIAOMI_TOKEN_PLAN_SGP_API_KEY"),
            ("zai", "ZAI_API_KEY"),
            ("zai-coding-cn", "ZAI_CODING_CN_API_KEY"),
        ];
        for (provider, variable) in expected {
            assert_eq!(ambient_key_names(provider), [variable], "{provider}");
        }
    }

    #[test]
    fn configurable_directory_withholds_future_oauth_only_catalog_route() {
        let catalog = CatalogIndex::new(vec![
            catalog_provider("openai", true),
            catalog_provider("future-oauth-only", false),
        ])
        .unwrap();
        let profiles = IndexMap::new();
        let entries = directory_entries(&profiles, &catalog);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].provider.as_str(), "openai");
        assert_eq!(entries[0].settings_ns, NAME);
        assert_eq!(entries[0].settings_path, ["providers", "openai"]);
        assert_eq!(
            entries[0].authentication,
            LlmProviderAuthentication::ProviderNative
        );
        assert_eq!(entries[0].declared, Some(false));
    }
}
