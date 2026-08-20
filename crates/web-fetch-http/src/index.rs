//! Registers an anonymous public HTTP(S) WebFetchProvider with ctx.web.

use std::sync::Arc;

use seekdeep_cordis::{Context, Plugin};
use seekdeep_schemastery::Schema;
use seekdeep_util::timeout::MAX_TIMER_DELAY_MS;
use seekdeep_web::index::WEB;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider::{HttpFetchLimits, HttpFetchProvider};

/// Default User-Agent: an explicit product agent, never a browser disguise.
pub const DEFAULT_USER_AGENT: &str = "deepseek-harness/0.0.1 (+https://github.com/deepseek-ai)";

/// Cordis plugin name used by loader diagnostics.
pub const NAME: &str = "web-fetch-http";

/// The web seam this provider registers into.
pub const INJECT: &[&str] = &["web"];

/// Plugin config: the provider's transport and size limits plus its User-Agent (all defaulted).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HttpFetchConfig {
    /// Maximum accepted request URL length.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_url_length: Option<f64>,
    /// Maximum response body size in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_response_bytes: Option<f64>,
    /// Maximum decoded body length in characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_body_chars: Option<f64>,
    /// Default fetch timeout in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<f64>,
    /// Maximum number of same-origin redirect hops to follow.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_redirects: Option<f64>,
    /// User-Agent header sent on every request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
}

/// The source-compatible admission schema for the plugin.
#[must_use]
pub fn config_schema() -> Schema {
    Schema::object([
        ("maxUrlLength", Schema::number().with_default(2048.0)),
        (
            "maxResponseBytes",
            Schema::number().with_default(5_000_000.0),
        ),
        ("maxBodyChars", Schema::number().with_default(100_000.0)),
        ("timeoutMs", Schema::number().with_default(30_000.0)),
        ("maxRedirects", Schema::number().with_default(5.0)),
        (
            "userAgent",
            Schema::string().with_default(DEFAULT_USER_AGENT),
        ),
    ])
}

/// Resolves and validates one config into the provider limits.
///
/// # Errors
///
/// Returns a configuration validation error for a non-positive, non-finite, or out-of-range
/// limit.
pub fn resolve_limits(config: &HttpFetchConfig) -> anyhow::Result<HttpFetchLimits> {
    let max_url_length = config.max_url_length.unwrap_or(2048.0);
    let max_response_bytes = config.max_response_bytes.unwrap_or(5_000_000.0);
    let max_body_chars = config.max_body_chars.unwrap_or(100_000.0);
    let timeout_ms = config.timeout_ms.unwrap_or(30_000.0);
    let max_redirects = config.max_redirects.unwrap_or(5.0);
    let user_agent = config
        .user_agent
        .clone()
        .unwrap_or_else(|| DEFAULT_USER_AGENT.to_owned());

    assert_positive_finite("maxUrlLength", max_url_length)?;
    assert_positive_finite("maxResponseBytes", max_response_bytes)?;
    assert_positive_finite("maxBodyChars", max_body_chars)?;
    assert_timeout_ms(timeout_ms)?;
    assert_non_negative_integer("maxRedirects", max_redirects)?;

    Ok(HttpFetchLimits {
        max_url_length,
        max_response_bytes,
        max_body_chars,
        timeout_ms,
        max_redirects: crate::numeric::trunc_to_u64(max_redirects),
        user_agent,
    })
}

fn assert_positive_finite(name: &str, value: f64) -> anyhow::Result<()> {
    anyhow::ensure!(
        value.is_finite() && value > 0.0,
        "web-fetch-http: {name} must be a positive finite number"
    );
    Ok(())
}

fn assert_timeout_ms(value: f64) -> anyhow::Result<()> {
    assert_positive_finite("timeoutMs", value)?;
    anyhow::ensure!(
        value <= MAX_TIMER_DELAY_MS,
        "web-fetch-http: timeoutMs must be no greater than {MAX_TIMER_DELAY_MS}"
    );
    Ok(())
}

fn assert_non_negative_integer(name: &str, value: f64) -> anyhow::Result<()> {
    anyhow::ensure!(
        value.fract() == 0.0 && value >= 0.0,
        "web-fetch-http: {name} must be a non-negative integer"
    );
    Ok(())
}

/// Registers the local HTTP(S) fetch provider with ctx.web.
fn install_into_context(context: &Context, config: &Value) -> anyhow::Result<()> {
    let config: HttpFetchConfig = serde_json::from_value(config.clone())?;
    let limits = resolve_limits(&config)?;
    let runtime = context
        .get(WEB)
        .ok_or_else(|| anyhow::anyhow!("web-fetch-http requires the web service"))?;
    runtime.register_fetch_provider(context, Arc::new(HttpFetchProvider::new(limits)))?;
    Ok(())
}

/// Builds the loader-compatible web-fetch-http plugin.
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
    config: HttpFetchConfig,
) -> anyhow::Result<Arc<seekdeep_cordis::PluginFiber>> {
    Ok(context.plugin(plugin(), serde_json::to_value(config)?)?)
}
