//! Register an Exa-backed provider in `ctx.web`.

use std::sync::Arc;

use seekdeep_cordis::{Context, Plugin};
use seekdeep_schemastery::Schema;
use seekdeep_util::launch_environment::launch_environment_of;
use seekdeep_web::index::WEB;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider::{
    EXA_DEFAULT_BASE_URL, EXA_DEFAULT_HIGHLIGHTS_PER_RESULT, EXA_DEFAULT_SEARCH_TYPE,
    ExaSearchProvider, ExaSearchProviderOptions, SearchType,
};

/// Cordis plugin name used by loader diagnostics.
pub const NAME: &str = "web-search-exa";

/// The web seam this provider registers into.
pub const INJECT: &[&str] = &["web"];

/// Environment variable naming this provider's API key.
pub const API_KEY_ENV: &str = "EXA_API_KEY";

/// Raw plugin configuration.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ExaConfig {
    /// Exa API key. Falls back to `$EXA_API_KEY`. Empty means unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Endpoint base; `/search` is appended.
    #[serde(rename = "baseURL", skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Retrieval mode sent as Exa's `type`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_type: Option<SearchType>,
    /// Default result count when a request carries no `maxResults`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_results: Option<f64>,
    /// Highlight sentences requested per result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub highlights_per_result: Option<f64>,
}

/// The source-compatible admission schema for the plugin.
#[must_use]
pub fn config_schema() -> Schema {
    Schema::object([
        ("apiKey", Schema::string()),
        ("baseURL", Schema::string()),
        (
            "searchType",
            Schema::union([
                Schema::constant("auto"),
                Schema::constant("keyword"),
                Schema::constant("neural"),
            ]),
        ),
        ("numResults", Schema::number().step(1.0).min(1.0)),
        ("highlightsPerResult", Schema::number().step(1.0).min(1.0)),
    ])
}

/// Project one resolved section into the options the provider serves with. Environment fallbacks
/// stay here rather than in the provider: every value it reads is already fully defaulted.
#[must_use]
pub fn resolve_options(context: &Context, config: &ExaConfig) -> ExaSearchProviderOptions {
    let environment = launch_environment_of(context);
    ExaSearchProviderOptions {
        api_key: config.api_key.clone().unwrap_or_else(|| {
            environment
                .get(API_KEY_ENV)
                .map_or_else(String::new, |entry| entry.value)
        }),
        base_url: config
            .base_url
            .clone()
            .unwrap_or_else(|| EXA_DEFAULT_BASE_URL.to_owned()),
        search_type: config.search_type.unwrap_or(EXA_DEFAULT_SEARCH_TYPE),
        num_results: config.num_results,
        highlights_per_result: config
            .highlights_per_result
            .unwrap_or(EXA_DEFAULT_HIGHLIGHTS_PER_RESULT),
    }
}

/// Registers the Exa search provider with `ctx.web`.
fn install_into_context(context: &Context, config: &Value) -> anyhow::Result<()> {
    let config: ExaConfig = serde_json::from_value(config.clone())?;
    let options = resolve_options(context, &config);
    let runtime = context
        .get(WEB)
        .ok_or_else(|| anyhow::anyhow!("web-search-exa requires the web service"))?;
    runtime.register_search_provider(context, Arc::new(ExaSearchProvider::new(options)))?;
    Ok(())
}

/// Builds the loader-compatible web-search-exa plugin.
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
    config: ExaConfig,
) -> anyhow::Result<Arc<seekdeep_cordis::PluginFiber>> {
    Ok(context.plugin(plugin(), serde_json::to_value(config)?)?)
}
