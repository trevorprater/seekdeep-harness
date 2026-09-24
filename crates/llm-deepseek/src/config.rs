//! Plugin configuration resolution and Cordis composition.

use std::{collections::HashMap, ffi::OsString, sync::Arc};

use futures::FutureExt as _;
use parking_lot::Mutex;
use seekdeep_anonymous_user_id::{AnonymousUserIdOptions, get_or_create_anonymous_user_id};
use seekdeep_cordis::{
    Context, Plugin,
    fiber::{DisposeFuture, EffectHandle},
};
use seekdeep_credentials::{CREDENTIALS, CredentialRef, credential_ref};
use seekdeep_llm::{
    AdapterRegistrationHandle, LLM, LlmConfigurableProvider, LlmError, LlmProviderAuthentication,
    ProviderId, assert_usable_api_key, resolve_retry_policy,
};
use seekdeep_schemastery::Schema;
use seekdeep_settings::{SettingsSectionSource, install_settings_section, settings_namespace};
use seekdeep_util::{
    launch_environment::{LaunchEnvironmentSnapshot, launch_environment_of},
    timeout::MAX_TIMER_DELAY_MS,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    adapter::{
        ApiKeyResolver, ConnectionResolver, DEFAULT_CONTEXT_WINDOW, DEFAULT_MAX_TOKENS,
        DEFAULT_STREAM_IDLE_TIMEOUT_MS, DeepSeekAdapter, DeepSeekAdapterOptions,
        DeepSeekConnectionOptions, ResolvedDeepSeekCatalogModel, UserIdResolver,
    },
    serialize::{ReasoningEffort, RequestDefaults},
    types::ThinkingMode,
};

/// Cordis plugin name.
pub const NAME: &str = "llm-deepseek";
/// Runtime service dependency.
pub const INJECT: &[&str] = &["llm"];
/// Single provider route owned by the plugin.
pub const PROVIDER: &str = "deepseek-official";
/// Public API default.
pub const PUBLIC_BASE_URL: &str = "https://api.deepseek.com";
/// Default credential reference.
pub const DEFAULT_API_KEY_ENV: &str = "DEEPSEEK_API_KEY";
/// Environment variable that may override the public endpoint.
pub const BASE_URL_ENV: &str = "DEEPSEEK_BASE_URL";

/// One raw advisory model entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepSeekCatalogModel {
    /// Wire model id.
    pub id: String,
    /// Optional selector label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Optional selector detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Known combined context capacity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<f64>,
    /// Model-specific output cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<f64>,
}

/// Raw plugin and settings-section configuration.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DeepSeekConfig {
    /// Credential reference resolved once per request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// Endpoint base.
    #[serde(rename = "baseURL", skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Deployment thinking policy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingMode>,
    /// Default thinking effort.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Default output cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<f64>,
    /// Fallback context capacity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_context_window: Option<f64>,
    /// Advisory model catalog; an explicit empty list is retained.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<DeepSeekCatalogModel>>,
    /// Maximum idle time while one body read is pending.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_idle_timeout_ms: Option<f64>,
    /// Provider-owned retry policy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_policy: Option<Value>,
}

/// Resolves, validates, and detaches all operation-local connection facts.
///
/// # Errors
///
/// Returns a stable configuration diagnostic before registration.
pub fn resolve_adapter_options(
    config: &DeepSeekConfig,
    environment: Option<&LaunchEnvironmentSnapshot>,
) -> anyhow::Result<DeepSeekConnectionOptions> {
    if config.thinking == Some(ThinkingMode::Disabled)
        && config
            .reasoning_effort
            .is_some_and(|effort| effort != ReasoningEffort::Off)
    {
        anyhow::bail!(
            "llm-deepseek: only reasoningEffort \"off\" can be configured when thinking is disabled"
        );
    }
    let default_context_window = positive_integer(
        config.default_context_window,
        DEFAULT_CONTEXT_WINDOW,
        "llm-deepseek: defaultContextWindow must be a positive integer",
        false,
    )?;
    let max_tokens = positive_integer(
        config.max_tokens,
        DEFAULT_MAX_TOKENS,
        "llm-deepseek: maxTokens must be a positive safe integer",
        true,
    )?;
    let stream_idle_timeout_ms = config
        .stream_idle_timeout_ms
        .unwrap_or(DEFAULT_STREAM_IDLE_TIMEOUT_MS);
    anyhow::ensure!(
        stream_idle_timeout_ms.is_finite()
            && stream_idle_timeout_ms > 0.0
            && stream_idle_timeout_ms <= MAX_TIMER_DELAY_MS,
        "llm-deepseek: streamIdleTimeoutMs must be a positive finite number no greater than {}",
        2_147_483_647_u64
    );
    let api_key_env = credential_ref(config.api_key_env.as_deref().unwrap_or(DEFAULT_API_KEY_ENV))?;
    let base_url = config
        .base_url
        .clone()
        .or_else(|| environment.and_then(|environment| environment.get(BASE_URL_ENV)?.value.into()))
        .unwrap_or_else(|| PUBLIC_BASE_URL.to_owned());
    Ok(DeepSeekConnectionOptions {
        base_url,
        api_key_env,
        defaults: RequestDefaults {
            thinking: config.thinking,
            reasoning_effort: config.reasoning_effort,
        },
        max_tokens,
        default_context_window,
        models: resolve_models(config.models.as_deref())?,
        stream_idle_timeout_ms,
        retry_policy: resolve_retry_policy(
            config.retry_policy.as_ref(),
            "llm-deepseek: retryPolicy",
        )?,
    })
}

fn resolve_models(
    models: Option<&[DeepSeekCatalogModel]>,
) -> anyhow::Result<Vec<ResolvedDeepSeekCatalogModel>> {
    let defaults = default_models();
    let models = models.unwrap_or(&defaults);
    let mut seen = std::collections::HashSet::new();
    models
        .iter()
        .map(|model| {
            anyhow::ensure!(
                !model.id.is_empty(),
                "llm-deepseek: catalog model ids must be non-empty"
            );
            anyhow::ensure!(
                model.name.as_ref().is_none_or(|name| !name.is_empty()),
                "llm-deepseek: catalog model \"{}\" has an empty name",
                model.id
            );
            let context_window = model
                .context_window
                .map(|value| {
                    positive_integer(
                        Some(value),
                        1,
                        &format!(
                            "llm-deepseek: catalog model \"{}\" contextWindow must be a positive integer",
                            model.id
                        ),
                        false,
                    )
                })
                .transpose()?;
            let max_tokens = model
                .max_tokens
                .map(|value| {
                    positive_integer(
                        Some(value),
                        1,
                        &format!(
                            "llm-deepseek: catalog model \"{}\" maxTokens must be a positive integer",
                            model.id
                        ),
                        false,
                    )
                })
                .transpose()?;
            anyhow::ensure!(
                seen.insert(model.id.clone()),
                "llm-deepseek: duplicate catalog model \"{}\"",
                model.id
            );
            Ok(ResolvedDeepSeekCatalogModel {
                id: model.id.clone(),
                name: model.name.clone(),
                description: model.description.clone(),
                context_window,
                max_tokens,
            })
        })
        .collect()
}

fn positive_integer(
    value: Option<f64>,
    default: u64,
    message: &str,
    safe: bool,
) -> anyhow::Result<u64> {
    let Some(value) = value else {
        return Ok(default);
    };
    let valid = value.is_finite()
        && value > 0.0
        && value.fract() == 0.0
        && (!safe || value <= 9_007_199_254_740_991.0);
    anyhow::ensure!(valid, "{message}");
    format!("{value:.0}")
        .parse()
        .map_err(|_| anyhow::anyhow!(message.to_owned()))
}

fn default_models() -> Vec<DeepSeekCatalogModel> {
    vec![
        DeepSeekCatalogModel {
            id: "deepseek-v4-flash".to_owned(),
            name: Some("DeepSeek-V4-Flash".to_owned()),
            description: None,
            context_window: Some(1_000_000.0),
            max_tokens: None,
        },
        DeepSeekCatalogModel {
            id: "deepseek-v4-pro".to_owned(),
            name: Some("DeepSeek-V4-Pro".to_owned()),
            description: None,
            context_window: Some(1_000_000.0),
            max_tokens: None,
        },
    ]
}

fn settings_schema() -> Schema {
    let catalog_model = Schema::object([
        ("id", Schema::string().required()),
        ("name", Schema::string()),
        ("description", Schema::string()),
        ("contextWindow", Schema::number().step(1.0).min(1.0)),
        ("maxTokens", Schema::number().step(1.0).min(1.0)),
    ]);
    Schema::object([
        (
            "apiKeyEnv",
            Schema::string()
                .role("credential-ref")
                .with_default(DEFAULT_API_KEY_ENV),
        ),
        ("baseURL", Schema::string()),
        (
            "thinking",
            Schema::union([Schema::constant("enabled"), Schema::constant("disabled")]),
        ),
        (
            "reasoningEffort",
            Schema::union([
                Schema::constant("off"),
                Schema::constant("high"),
                Schema::constant("max"),
            ]),
        ),
        (
            "maxTokens",
            Schema::number()
                .step(1.0)
                .min(1.0)
                .max(9_007_199_254_740_991.0)
                .with_default(DEFAULT_MAX_TOKENS),
        ),
        (
            "defaultContextWindow",
            Schema::number()
                .step(1.0)
                .min(1.0)
                .with_default(DEFAULT_CONTEXT_WINDOW),
        ),
        (
            "models",
            Schema::array(catalog_model)
                .with_default(serde_json::to_value(default_models()).expect("defaults are JSON")),
        ),
        (
            "streamIdleTimeoutMs",
            Schema::number()
                .min(f64::from_bits(1))
                .max(MAX_TIMER_DELAY_MS)
                .with_default(DEFAULT_STREAM_IDLE_TIMEOUT_MS),
        ),
        ("retryPolicy", seekdeep_llm::retry_policy_schema()),
    ])
}

struct DynamicOptions {
    static_raw: Value,
    source: Mutex<Option<SettingsSectionSource>>,
    environment: Arc<LaunchEnvironmentSnapshot>,
    cache: Mutex<(Value, Arc<DeepSeekConnectionOptions>)>,
}

impl DynamicOptions {
    fn current_raw(&self) -> Value {
        self.source
            .lock()
            .as_ref()
            .map_or_else(|| self.static_raw.clone(), SettingsSectionSource::get)
    }

    fn resolve(&self) -> Arc<DeepSeekConnectionOptions> {
        let raw = self.current_raw();
        {
            let cache = self.cache.lock();
            if cache.0 == raw {
                return cache.1.clone();
            }
        }
        let next = serde_json::from_value::<DeepSeekConfig>(raw.clone())
            .map_err(anyhow::Error::from)
            .and_then(|config| resolve_adapter_options(&config, Some(&self.environment)));
        let mut cache = self.cache.lock();
        cache.0 = raw;
        match next {
            Ok(next) => {
                cache.1 = Arc::new(next);
            }
            Err(error) => {
                tracing::error!(
                    "llm-deepseek: keeping the last good configuration after an invalid settings section"
                );
                tracing::error!(%error, "llm-deepseek settings error");
            }
        }
        cache.1.clone()
    }
}

/// Builds the Cordis provider plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: DeepSeekConfig = serde_json::from_value(config)?;
            install_into_context(&context, &config).await?;
            Ok(())
        })
    })
    .with_config_validator(|value| {
        let config: DeepSeekConfig = serde_json::from_value(value.clone())?;
        resolve_adapter_options(&config, None)?;
        Ok(serde_json::to_value(config)?)
    })
}

async fn install_into_context(context: &Context, config: &DeepSeekConfig) -> anyhow::Result<()> {
    let environment = launch_environment_of(context);
    let static_raw = serde_json::to_value(config)?;
    let connection = Arc::new(resolve_adapter_options(config, Some(&environment))?);
    let dynamic = Arc::new(DynamicOptions {
        static_raw: static_raw.clone(),
        source: Mutex::new(None),
        environment: environment.clone(),
        cache: Mutex::new((static_raw.clone(), connection)),
    });
    let options: ConnectionResolver = {
        let dynamic = dynamic.clone();
        Arc::new(move || dynamic.resolve())
    };
    let credential_context = context.clone();
    let resolve_api_key: ApiKeyResolver = Arc::new(move |connection| {
        let context = credential_context.clone();
        async move { resolve_api_key(&context, &connection.api_key_env).await }.boxed()
    });
    let anonymous_environment = launch_environment_of(context)
        .get("SEEKDEEP_HOME")
        .map(|entry| {
            HashMap::from([(OsString::from("SEEKDEEP_HOME"), OsString::from(entry.value))])
        });
    let memo = Arc::new(Mutex::new(None));
    let resolve_user_id: UserIdResolver = Arc::new(move || {
        if let Some(id) = memo.lock().clone() {
            return Ok(id);
        }
        let id = get_or_create_anonymous_user_id(AnonymousUserIdOptions {
            env: anonymous_environment.clone(),
            random_uuid: None,
        })?;
        *memo.lock() = Some(id.clone());
        Ok(id)
    });
    let adapter = Arc::new(DeepSeekAdapter::new(DeepSeekAdapterOptions {
        options: options.clone(),
        resolve_api_key,
        resolve_user_id,
        http: reqwest::Client::new(),
    }));
    let runtime = context
        .get(LLM)
        .ok_or_else(|| anyhow::anyhow!("llm-deepseek requires the llm service"))?;
    let directory = runtime.register_configurable_providers(&[LlmConfigurableProvider {
        provider: ProviderId::new(PROVIDER),
        display_name: "DeepSeek".to_owned(),
        settings_ns: NAME.to_owned(),
        settings_path: Vec::new(),
        authentication: LlmProviderAuthentication::ApiKey,
        declared: None,
    }])?;
    let registration = match runtime.register_adapter(&[PROVIDER.to_owned()], adapter) {
        Ok(registration) => Arc::new(registration),
        Err(error) => {
            directory.dispose().await?;
            return Err(error);
        }
    };
    let directory = Arc::new(directory);
    let cleanup_registration = registration.clone();
    let cleanup_directory = directory.clone();
    let cleanup = EffectHandle::new("llm-deepseek registrations", move || -> DisposeFuture {
        Box::pin(async move {
            cleanup_registration.dispose().await?;
            cleanup_directory.dispose().await?;
            Ok(())
        })
    });
    if let Err(error) = context.own(cleanup.clone()) {
        cleanup.dispose().await?;
        return Err(error.into());
    }
    install_dynamic_settings(context, static_raw, dynamic, options, registration).await?;
    Ok(())
}

async fn install_dynamic_settings(
    context: &Context,
    static_raw: Value,
    dynamic: Arc<DynamicOptions>,
    options: ConnectionResolver,
    registration: Arc<AdapterRegistrationHandle>,
) -> anyhow::Result<()> {
    let registered_policy = Arc::new(Mutex::new(options().retry_policy.clone()));
    let policy_options = options;
    let policy_state = registered_policy;
    let installed = install_settings_section(
        context,
        &settings_namespace(NAME)?,
        settings_schema(),
        static_raw,
        None,
        Arc::new(move || {
            let policy = policy_options().retry_policy.clone();
            let mut registered = policy_state.lock();
            if *registered == policy {
                return Ok(());
            }
            registration.replace(&[PROVIDER.to_owned()])?;
            *registered = policy;
            Ok(())
        }),
    )?;
    *dynamic.source.lock() = Some(installed.source);
    installed.fiber.await_settled().await
}

async fn resolve_api_key(context: &Context, reference: &CredentialRef) -> anyhow::Result<String> {
    if let Some(credentials) = context.get(CREDENTIALS) {
        if let Some(hit) = credentials.resolve(reference).await? {
            return Ok(assert_usable_api_key(
                &hit.value,
                "llm-deepseek",
                reference.as_str(),
            )?);
        }
    } else if let Some(ambient) = launch_environment_of(context).get(reference.as_str())
        && !ambient.value.is_empty()
    {
        return Ok(assert_usable_api_key(
            &ambient.value,
            "llm-deepseek",
            reference.as_str(),
        )?);
    }
    Err(LlmError::simple(
        format!(
            "llm-deepseek: no API key for provider route \"{PROVIDER}\"; store {reference} through the credentials service (the web Models page writes it), or export {reference} in the launching environment"
        ),
        "MISSING_CREDENTIAL",
    )
    .into())
}

/// Installs the provider as a lifecycle-owned plugin fiber.
///
/// # Errors
///
/// Returns configuration serialization or inactive-context failures.
pub fn install(
    context: &Context,
    config: DeepSeekConfig,
) -> anyhow::Result<Arc<seekdeep_cordis::PluginFiber>> {
    Ok(context.plugin(plugin(), serde_json::to_value(config)?)?)
}
