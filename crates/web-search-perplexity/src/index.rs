//! Register a Perplexity-backed provider in `ctx.web`.

use std::sync::Arc;

use seekdeep_cordis::{Context, Plugin};
use seekdeep_schemastery::Schema;
use seekdeep_util::launch_environment::launch_environment_of;
use seekdeep_web::index::WEB;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider::{
    PERPLEXITY_DEFAULT_BASE_URL, PERPLEXITY_DEFAULT_MAX_TOKENS, PERPLEXITY_DEFAULT_MODEL,
    PerplexityRecency, PerplexitySearchProvider, PerplexitySearchProviderOptions,
};

/// Cordis plugin name used by loader diagnostics.
pub const NAME: &str = "web-search-perplexity";

/// The web seam this provider registers into.
pub const INJECT: &[&str] = &["web"];

/// Environment variable naming this provider's API key.
pub const API_KEY_ENV: &str = "PERPLEXITY_API_KEY";

/// Raw plugin configuration.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PerplexityConfig {
    /// Perplexity API key. Falls back to `$PERPLEXITY_API_KEY`. Empty means unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Endpoint base; `/chat/completions` is appended.
    #[serde(rename = "baseURL", skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Search model name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Upper bound on generated answer tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<f64>,
    /// Optional recency window sent as `search_recency_filter`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_recency: Option<PerplexityRecency>,
}

/// The source-compatible admission schema for the plugin.
#[must_use]
pub fn config_schema() -> Schema {
    Schema::object([
        ("apiKey", Schema::string()),
        ("baseURL", Schema::string()),
        ("model", Schema::string()),
        ("maxTokens", Schema::number().step(1.0).min(1.0)),
        (
            "searchRecency",
            Schema::union([
                Schema::constant("day"),
                Schema::constant("week"),
                Schema::constant("month"),
                Schema::constant("year"),
            ]),
        ),
    ])
}

/// Project one resolved section into the options the provider serves with. Environment fallbacks
/// stay here rather than in the provider: every value it reads is already fully defaulted.
#[must_use]
pub fn resolve_options(
    context: &Context,
    config: &PerplexityConfig,
) -> PerplexitySearchProviderOptions {
    let environment = launch_environment_of(context);
    PerplexitySearchProviderOptions {
        api_key: config.api_key.clone().unwrap_or_else(|| {
            environment
                .get(API_KEY_ENV)
                .map_or_else(String::new, |entry| entry.value)
        }),
        base_url: config
            .base_url
            .clone()
            .unwrap_or_else(|| PERPLEXITY_DEFAULT_BASE_URL.to_owned()),
        model: config
            .model
            .clone()
            .unwrap_or_else(|| PERPLEXITY_DEFAULT_MODEL.to_owned()),
        max_tokens: config.max_tokens.unwrap_or(PERPLEXITY_DEFAULT_MAX_TOKENS),
        search_recency: config.search_recency,
    }
}

/// Registers the Perplexity search provider with `ctx.web`.
fn install_into_context(context: &Context, config: &Value) -> anyhow::Result<()> {
    let config: PerplexityConfig = serde_json::from_value(config.clone())?;
    let options = resolve_options(context, &config);
    let runtime = context
        .get(WEB)
        .ok_or_else(|| anyhow::anyhow!("web-search-perplexity requires the web service"))?;
    let _ = runtime
        .register_search_provider(context, Arc::new(PerplexitySearchProvider::new(options)))?;
    Ok(())
}

/// Builds the loader-compatible web-search-perplexity plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            install_into_context(&context, &config)?;
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
    config: PerplexityConfig,
) -> anyhow::Result<Arc<seekdeep_cordis::PluginFiber>> {
    Ok(context.plugin(plugin(), serde_json::to_value(config)?)?)
}
