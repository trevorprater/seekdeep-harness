//! Register a DeepSeek-backed provider in `ctx.web`. It calls the Anthropic-compatible Messages API
//! with native `web_search_20250305`. The provider reuses `DEEPSEEK_API_KEY` but not
//! `DEEPSEEK_BASE_URL`, because search and chat-completions use different bases.

use std::sync::Arc;

use futures::FutureExt as _;
use seekdeep_agent::AGENTS;
use seekdeep_cordis::{Context, Plugin};
use seekdeep_core::session::AppendOptions;
use seekdeep_credentials::{CREDENTIALS, credential_ref};
use seekdeep_schemastery::Schema;
use seekdeep_settings::{install_settings_section, settings_namespace};
use seekdeep_util::launch_environment::launch_environment_of;
use seekdeep_web::index::WEB;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider::{
    DEEPSEEK_DEFAULT_API_VERSION, DEEPSEEK_DEFAULT_BASE_URL, DEEPSEEK_DEFAULT_MAX_TOKENS,
    DEEPSEEK_DEFAULT_MAX_USES, DEEPSEEK_DEFAULT_MODEL, DeepSeekSearchLlmRequest,
    DeepSeekSearchProvider, DeepSeekSearchProviderOptions, RecordRequest, ResolveApiKey,
};

/// Cordis plugin name used by loader diagnostics.
pub const NAME: &str = "web-search-deepseek";

/// The web seam this provider registers into.
pub const INJECT: &[&str] = &["web"];

/// Default credential reference for `DeepSeek` search.
pub const DEFAULT_API_KEY_ENV: &str = "DEEPSEEK_API_KEY";

/// Environment variable naming this provider's endpoint. Deliberately distinct from
/// `$DEEPSEEK_BASE_URL`, which belongs to the chat-completions adapter: search speaks the
/// Anthropic-compatible Messages API, so one variable cannot serve both.
const SEARCH_BASE_URL_ENV: &str = "DEEPSEEK_SEARCH_BASE_URL";

/// Raw plugin and settings-section configuration.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DeepSeekSearchConfig {
    /// Literal `DeepSeek` API key; prefer `api_key_env` so no secret enters configuration files.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Credential reference resolved for each search; defaults to `DEEPSEEK_API_KEY`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// Anthropic-compatible endpoint base; `/messages` is appended.
    #[serde(rename = "baseURL", skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Anthropic-format model name. Defaults to `deepseek-v4-flash`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// `anthropic-version` header value. Defaults to `2023-06-01`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_version: Option<String>,
    /// Upper bound on generated tokens for the Messages request. Defaults to 4096.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<f64>,
    /// Maximum `web_search` server-tool uses per request. Defaults to 5.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_uses: Option<f64>,
}

/// The source-compatible admission schema for the plugin and its settings section.
#[must_use]
pub fn config_schema() -> Schema {
    Schema::object([
        ("apiKey", Schema::string().role("secret")),
        (
            "apiKeyEnv",
            Schema::string()
                .role("credential-ref")
                .with_default(DEFAULT_API_KEY_ENV),
        ),
        ("baseURL", Schema::string()),
        (
            "model",
            Schema::string().with_default(DEEPSEEK_DEFAULT_MODEL),
        ),
        (
            "apiVersion",
            Schema::string().with_default(DEEPSEEK_DEFAULT_API_VERSION),
        ),
        (
            "maxTokens",
            Schema::number()
                .step(1.0)
                .min(1.0)
                .with_default(DEEPSEEK_DEFAULT_MAX_TOKENS),
        ),
        (
            "maxUses",
            Schema::number()
                .step(1.0)
                .min(1.0)
                .with_default(DEEPSEEK_DEFAULT_MAX_USES),
        ),
    ])
}

/// Returns the settings namespace carrying this provider's endpoint, model, and key reference.
///
/// # Errors
///
/// Returns a namespace-validation failure, which cannot happen for the fixed
/// `web-search-deepseek` name.
pub fn web_search_deepseek_settings_namespace()
-> anyhow::Result<seekdeep_settings::SettingsNamespace> {
    settings_namespace("web-search-deepseek")
}

/// Project one resolved section into the options the provider serves its next search with.
/// Environment fallbacks stay here rather than in the provider: every value it reads is already
/// fully defaulted.
///
/// # Errors
///
/// Returns a credential-reference validation failure for an invalid `apiKeyEnv`.
pub fn resolve_options(
    context: &Context,
    config: &DeepSeekSearchConfig,
) -> anyhow::Result<DeepSeekSearchProviderOptions> {
    let api_key_env = credential_ref(config.api_key_env.as_deref().unwrap_or(DEFAULT_API_KEY_ENV))?;
    let literal_api_key = config
        .api_key
        .as_deref()
        .filter(|key| !key.is_empty())
        .map(ToOwned::to_owned);

    let credential_context = context.clone();
    let resolve_reference = api_key_env.clone();
    let resolve_api_key: ResolveApiKey = Arc::new(move || {
        let context = credential_context.clone();
        let reference = resolve_reference.clone();
        async move {
            if let Some(credentials) = context.get(CREDENTIALS) {
                return Ok(credentials.resolve(&reference).await?.map(|hit| hit.value));
            }
            // Without the seam the environment is the whole credential plane.
            let ambient = launch_environment_of(&context).get(reference.as_str());
            Ok(ambient
                .filter(|entry| !entry.value.is_empty())
                .map(|entry| entry.value))
        }
        .boxed()
    });

    let record_context = context.clone();
    let record_request: RecordRequest = Arc::new(move |request: &DeepSeekSearchLlmRequest| {
        let Some(agents) = record_context.get(AGENTS) else {
            return Ok(());
        };
        let Some(agent) = agents.current_initiator().ok().flatten() else {
            return Ok(());
        };
        let value = serde_json::to_value(request).map_err(anyhow::Error::from)?;
        agent.session().append(
            "web/deepseek-search-llm-request",
            value,
            AppendOptions::default(),
        )?;
        Ok(())
    });

    let environment = launch_environment_of(context);
    Ok(DeepSeekSearchProviderOptions {
        api_key: literal_api_key,
        resolve_api_key: Some(resolve_api_key),
        api_key_env: Some(api_key_env),
        base_url: config
            .base_url
            .clone()
            .or_else(|| {
                environment
                    .get(SEARCH_BASE_URL_ENV)
                    .map(|entry| entry.value)
            })
            .unwrap_or_else(|| DEEPSEEK_DEFAULT_BASE_URL.to_owned()),
        model: config
            .model
            .clone()
            .unwrap_or_else(|| DEEPSEEK_DEFAULT_MODEL.to_owned()),
        api_version: config
            .api_version
            .clone()
            .unwrap_or_else(|| DEEPSEEK_DEFAULT_API_VERSION.to_owned()),
        max_tokens: positive_integer(config.max_tokens, DEEPSEEK_DEFAULT_MAX_TOKENS),
        max_uses: positive_integer(config.max_uses, DEEPSEEK_DEFAULT_MAX_USES),
        record_request: Some(record_request),
    })
}

/// Registers the `DeepSeek` search provider with `ctx.web`.
async fn install_into_context(context: &Context, config: &Value) -> anyhow::Result<()> {
    let namespace = web_search_deepseek_settings_namespace()?;
    let entry = config.clone();
    let installed = install_settings_section(
        context,
        &namespace,
        config_schema(),
        entry,
        None,
        Arc::new(|| Ok(())),
    )?;
    let source = installed.source;
    let provider_context = context.clone();
    let provider = DeepSeekSearchProvider::new(move || {
        let raw = source.get();
        let config: DeepSeekSearchConfig = serde_json::from_value(raw)
            .expect("the settings section resolves to a valid web-search-deepseek config");
        resolve_options(&provider_context, &config)
            .expect("the settings section resolves to valid web-search-deepseek options")
    });
    let runtime = context
        .get(WEB)
        .ok_or_else(|| anyhow::anyhow!("web-search-deepseek requires the web service"))?;
    let _ = runtime.register_search_provider(context, Arc::new(provider))?;
    // Await the settings-section helper so the namespace is registered before this plugin's
    // startup reports success; its teardown later unregisters the provider through the web seam's
    // reverse effect when the plugin fiber unloads.
    installed.fiber.await_settled().await?;
    Ok(())
}

/// Builds the loader-compatible web-search-deepseek plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            install_into_context(&context, &config).await?;
            Ok(())
        })
    })
    .with_config_validator(|value: &Value| {
        config_schema()
            .resolve(value)
            .map_err(|error| anyhow::anyhow!("{error}"))
    })
}

/// Installs the provider as a lifecycle-owned plugin fiber.
///
/// # Errors
///
/// Returns configuration serialization or inactive-context failures.
pub fn install(
    context: &Context,
    config: DeepSeekSearchConfig,
) -> anyhow::Result<Arc<seekdeep_cordis::PluginFiber>> {
    Ok(context.plugin(plugin(), serde_json::to_value(config)?)?)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn positive_integer(value: Option<f64>, default: u64) -> u64 {
    value.map_or(default, |value| value as u64)
}
