//! Provider-profile parsing, validation, and detached route resolution.

use std::{collections::HashMap, sync::OnceLock};

use indexmap::IndexMap;
use seekdeep_credentials::{CredentialRef, credential_ref};
use seekdeep_llm::{ResolvedRetryPolicy, resolve_retry_policy};
use seekdeep_schemastery::Schema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{
    catalog::{
        CatalogIndex, PiCompatProfile, PiModality, PiModelFields, PiModelProfile,
        PiReasoningEfforts, PiThinkingFormat, PiThinkingLevel, RouteCatalog, RouteCatalogRequest,
        builtin_catalog, resolve_route_models,
    },
    provider::{PiProvider, ProviderSpec, build_provider},
    replay::PiApi,
};

/// Default maximum idle interval while one provider stream read is outstanding.
pub const DEFAULT_STREAM_IDLE_TIMEOUT_MS: f64 = 300_000.0;
/// Largest platform timer delay accepted by the source adapter.
pub const MAX_TIMER_DELAY_MS: f64 = 2_147_483_647.0;
/// Hand-declared protocols this build can authenticate and dispatch.
pub const SUPPORTED_PROTOCOLS: [&str; 3] = [
    "openai-completions",
    "openai-responses",
    "anthropic-messages",
];

/// Returns the process-stable runtime configuration schema.
#[must_use]
pub fn config_schema() -> Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA.get_or_init(build_config_schema).clone()
}

/// Applies source-compatible schema defaults without checking serviceability.
///
/// # Errors
///
/// Returns the first structural schema failure.
pub fn materialize_config(config: &Value) -> anyhow::Result<Value> {
    config_schema().resolve(config).map_err(anyhow::Error::from)
}

/// Validates that one already-materialized configuration can serve every route.
///
/// # Errors
///
/// Returns the first route/model resolution failure.
pub fn assert_serviceable(config: &Value) -> anyhow::Result<()> {
    resolve_config(config).map(|_| ())
}

#[allow(clippy::too_many_lines)] // One source-ordered schema graph is easier to audit intact.
fn build_config_schema() -> Schema {
    let modalities = || Schema::union([Schema::constant("text"), Schema::constant("image")]);
    let levels = || {
        Schema::union([
            Schema::constant("off"),
            Schema::constant("minimal"),
            Schema::constant("low"),
            Schema::constant("medium"),
            Schema::constant("high"),
            Schema::constant("xhigh"),
            Schema::constant("max"),
        ])
    };
    let formats = || {
        Schema::union([
            Schema::constant("openai"),
            Schema::constant("deepseek"),
            Schema::constant("openrouter"),
            Schema::constant("together"),
            Schema::constant("zai"),
            Schema::constant("qwen"),
            Schema::constant("string-thinking"),
            Schema::constant("ant-ling"),
        ])
    };
    let compat = || {
        Schema::object([
            ("thinkingFormat", formats()),
            ("supportsReasoningEffort", Schema::boolean()),
        ])
    };
    let efforts = || {
        Schema::dict_with_keys(
            Schema::union([Schema::string(), Schema::constant(Value::Null)]),
            levels(),
        )
    };
    let model_fields = || {
        [
            ("name", Schema::string()),
            ("contextWindow", Schema::number().step(1.0).min(1.0)),
            ("maxTokens", Schema::number().step(1.0).min(1.0)),
            ("input", Schema::array(modalities())),
            (
                "reasoningEfforts",
                Schema::union([Schema::constant(false), efforts()]),
            ),
            ("compat", compat()),
        ]
    };
    let model =
        Schema::object(std::iter::once(("id", Schema::string().required())).chain(model_fields()));
    let model_override = Schema::object(model_fields());
    let thinking_budgets = Schema::object([
        ("minimal", Schema::number()),
        ("low", Schema::number()),
        ("medium", Schema::number()),
        ("high", Schema::number()),
    ]);
    let profile = Schema::object([
        ("apiKeyEnv", Schema::string().role("credential-ref")),
        ("displayName", Schema::string()),
        (
            "api",
            Schema::union(SUPPORTED_PROTOCOLS.map(Schema::constant)),
        ),
        ("baseURL", Schema::string()),
        ("models", Schema::array(model)),
        ("modelOverrides", Schema::dict(model_override)),
        ("compat", compat()),
        (
            "defaultContextWindow",
            Schema::number()
                .step(1.0)
                .min(1.0)
                .with_default(crate::catalog::DEFAULT_CONTEXT_WINDOW),
        ),
        (
            "defaultMaxTokens",
            Schema::number()
                .step(1.0)
                .min(1.0)
                .with_default(crate::catalog::DEFAULT_MAX_TOKENS),
        ),
        (
            "defaultInput",
            Schema::array(modalities()).with_default(json!(["text"])),
        ),
        ("headers", Schema::dict(Schema::string())),
        ("reasoning", levels()),
        ("thinkingBudgets", thinking_budgets),
        (
            "cacheRetention",
            Schema::union([
                Schema::constant("none"),
                Schema::constant("short"),
                Schema::constant("long"),
            ]),
        ),
        (
            "transport",
            Schema::union([
                Schema::constant("sse"),
                Schema::constant("websocket"),
                Schema::constant("websocket-cached"),
                Schema::constant("auto"),
            ]),
        ),
        ("timeoutMs", Schema::number().step(1.0).min(0.0)),
        (
            "websocketConnectTimeoutMs",
            Schema::number().step(1.0).min(0.0),
        ),
        (
            "streamIdleTimeoutMs",
            Schema::number()
                .min(f64::from_bits(1))
                .max(MAX_TIMER_DELAY_MS)
                .with_default(DEFAULT_STREAM_IDLE_TIMEOUT_MS),
        ),
        ("retryPolicy", seekdeep_llm::retry_policy_schema()),
    ]);
    Schema::object([("providers", Schema::dict(profile).with_default(json!({})))])
}

/// Provider-native prompt cache retention preference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PiCacheRetention {
    /// Disable prompt retention.
    None,
    /// Short retention.
    Short,
    /// Long retention.
    Long,
}

/// Provider streaming transport preference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PiTransport {
    /// Server-sent events.
    Sse,
    /// Fresh WebSocket per request.
    Websocket,
    /// Cached WebSocket connection.
    WebsocketCached,
    /// Provider-selected transport.
    Auto,
}

/// Token budgets for pi-ai reasoning levels that accept them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PiThinkingBudgets {
    /// Minimal reasoning budget.
    pub minimal: Option<f64>,
    /// Low reasoning budget.
    pub low: Option<f64>,
    /// Medium reasoning budget.
    pub medium: Option<f64>,
    /// High reasoning budget.
    pub high: Option<f64>,
}

/// Detached non-catalog provider options used by request construction.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PiProviderOptions {
    /// Protocol override.
    pub api: Option<PiApi>,
    /// Endpoint override.
    pub base_url: Option<String>,
    /// Route reasoning compatibility switches.
    pub compat: Option<PiCompatProfile>,
    /// Provider request headers.
    pub headers: Option<Map<String, Value>>,
    /// Provider-neutral reasoning level.
    pub reasoning: Option<PiThinkingLevel>,
    /// Provider reasoning budgets.
    pub thinking_budgets: Option<PiThinkingBudgets>,
    /// Prompt-cache retention.
    pub cache_retention: Option<PiCacheRetention>,
    /// Streaming transport.
    pub transport: Option<PiTransport>,
    /// Provider SDK timeout.
    pub timeout_ms: Option<u64>,
    /// WebSocket connection timeout.
    pub websocket_connect_timeout_ms: Option<u64>,
    /// Unknown future profile fields retained across the resolution boundary.
    pub extra: Map<String, Value>,
}

/// Fully resolved provider route.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedPiProviderProfile {
    /// Route identity.
    pub provider: seekdeep_llm::ProviderId,
    /// Selector and status display name.
    pub display_name: String,
    /// Validated credential reference.
    pub api_key_env: Option<CredentialRef>,
    /// Positive bounded idle timeout.
    pub stream_idle_timeout_ms: f64,
    /// Immutable provider-owned retry policy.
    pub retry_policy: ResolvedRetryPolicy,
    /// Materialized models and explicit output defaults.
    pub catalog: RouteCatalog,
    /// Built provider registration value.
    pub pi_provider: PiProvider,
    /// Remaining request/provider options.
    pub options: PiProviderOptions,
}

/// Resolves the complete plugin configuration against the pinned catalog.
///
/// # Errors
///
/// Rejects malformed schema values and every unserviceable route before any
/// provider registration occurs.
pub fn resolve_config(
    config: &Value,
) -> anyhow::Result<IndexMap<String, ResolvedPiProviderProfile>> {
    let object = config
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("llm-pi-ai: config must be an object"))?;
    resolve_profiles(object.get("providers"), builtin_catalog())
}

/// Resolves a provider dictionary against an explicit catalog.
///
/// # Errors
///
/// Rejects arrays using the removed profile-list diagnostic, other non-object
/// values, invalid fields, and unserviceable catalog requests.
pub fn resolve_profiles(
    providers: Option<&Value>,
    catalog: &CatalogIndex,
) -> anyhow::Result<IndexMap<String, ResolvedPiProviderProfile>> {
    let Some(providers) = providers else {
        return Ok(IndexMap::new());
    };
    if providers.is_array() {
        anyhow::bail!(
            "llm-pi-ai: providers is now a dict keyed by provider route, not an array of profiles"
        );
    }
    let providers = providers
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("llm-pi-ai: providers must be an object"))?;
    let mut resolved = IndexMap::with_capacity(providers.len());
    for (provider, source) in providers {
        let profile = resolve_profile(provider, source, catalog)?;
        resolved.insert(provider.clone(), profile);
    }
    Ok(resolved)
}

#[allow(clippy::too_many_lines)] // One route is validated and detached atomically.
fn resolve_profile(
    provider: &str,
    source: &Value,
    catalog: &CatalogIndex,
) -> anyhow::Result<ResolvedPiProviderProfile> {
    let source = source
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("llm-pi-ai: provider \"{provider}\" must be an object"))?;
    reject_removed_fields(provider, source)?;
    anyhow::ensure!(
        !provider.is_empty(),
        "llm-pi-ai: provider names must be non-empty"
    );
    let base_url = optional_string(source, "baseURL")?;
    if base_url.as_deref() == Some("") {
        anyhow::bail!("llm-pi-ai: provider \"{provider}\" has an empty baseURL");
    }
    let display_name =
        optional_string(source, "displayName")?.unwrap_or_else(|| provider.to_owned());
    if display_name.is_empty() {
        anyhow::bail!("llm-pi-ai: provider \"{provider}\" has an empty displayName");
    }
    let stream_idle_timeout_ms =
        optional_f64(source, "streamIdleTimeoutMs")?.unwrap_or(DEFAULT_STREAM_IDLE_TIMEOUT_MS);
    if !stream_idle_timeout_ms.is_finite()
        || stream_idle_timeout_ms <= 0.0
        || stream_idle_timeout_ms > MAX_TIMER_DELAY_MS
    {
        anyhow::bail!(
            "llm-pi-ai: provider \"{provider}\" streamIdleTimeoutMs must be a positive finite number no greater than 2147483647"
        );
    }
    let api = optional_string(source, "api")?.map(PiApi::new);
    if let Some(api) = &api
        && !SUPPORTED_PROTOCOLS.contains(&api.as_str())
    {
        anyhow::bail!(
            "llm-pi-ai: provider \"{provider}\" names api \"{}\", which this build cannot serve; supported protocols are {}",
            api.as_str(),
            SUPPORTED_PROTOCOLS.join(", ")
        );
    }
    let default_context_window = optional_positive_integer(source, "defaultContextWindow")?
        .unwrap_or(crate::catalog::DEFAULT_CONTEXT_WINDOW);
    let default_max_tokens = optional_positive_integer(source, "defaultMaxTokens")?
        .unwrap_or(crate::catalog::DEFAULT_MAX_TOKENS);
    let default_input = source.get("defaultInput").map_or_else(
        || Ok(vec![PiModality::Text]),
        |value| parse_modalities(value, "defaultInput"),
    )?;
    if default_input.is_empty() {
        anyhow::bail!(
            "llm-pi-ai: provider \"{provider}\" defaultInput must name at least one modality"
        );
    }
    let compat = source.get("compat").map(parse_compat).transpose()?;
    let models = source
        .get("models")
        .map(parse_models)
        .transpose()?
        .unwrap_or_default();
    let model_overrides = source
        .get("modelOverrides")
        .map(parse_overrides)
        .transpose()?
        .unwrap_or_default();
    let request = RouteCatalogRequest {
        provider: seekdeep_llm::ProviderId::new(provider),
        api: api.clone(),
        base_url: base_url.clone(),
        models,
        model_overrides,
        compat: compat.clone(),
        default_context_window,
        default_max_tokens,
        default_input,
    };
    let route_catalog = resolve_route_models(catalog, &request)?;
    let api_key_env = optional_string(source, "apiKeyEnv")?
        .map(credential_ref)
        .transpose()?;
    let pi_provider = build_provider(
        catalog,
        ProviderSpec {
            provider: request.provider.clone(),
            display_name: display_name.clone(),
            api: api.clone(),
            base_url: base_url.clone(),
            models: route_catalog.models.clone(),
            names_credential: api_key_env.is_some(),
        },
    )?;
    let retry_policy = resolve_retry_policy(
        source.get("retryPolicy"),
        &format!("llm-pi-ai: provider \"{provider}\" retryPolicy"),
    )?;
    Ok(ResolvedPiProviderProfile {
        provider: request.provider,
        display_name,
        api_key_env,
        stream_idle_timeout_ms,
        retry_policy,
        catalog: route_catalog,
        pi_provider,
        options: parse_provider_options(source, api, base_url, compat)?,
    })
}

fn parse_provider_options(
    source: &Map<String, Value>,
    api: Option<PiApi>,
    base_url: Option<String>,
    compat: Option<PiCompatProfile>,
) -> anyhow::Result<PiProviderOptions> {
    let headers = source.get("headers").map(parse_headers).transpose()?;
    let reasoning = source
        .get("reasoning")
        .map(parse_thinking_level)
        .transpose()?;
    let thinking_budgets = source
        .get("thinkingBudgets")
        .map(parse_thinking_budgets)
        .transpose()?;
    let cache_retention = source
        .get("cacheRetention")
        .map(|value| {
            parse_enum(
                value,
                "cacheRetention",
                &[
                    ("none", PiCacheRetention::None),
                    ("short", PiCacheRetention::Short),
                    ("long", PiCacheRetention::Long),
                ],
            )
        })
        .transpose()?;
    let transport = source
        .get("transport")
        .map(|value| {
            parse_enum(
                value,
                "transport",
                &[
                    ("sse", PiTransport::Sse),
                    ("websocket", PiTransport::Websocket),
                    ("websocket-cached", PiTransport::WebsocketCached),
                    ("auto", PiTransport::Auto),
                ],
            )
        })
        .transpose()?;
    let timeout_ms = optional_natural(source, "timeoutMs")?;
    let websocket_connect_timeout_ms = optional_natural(source, "websocketConnectTimeoutMs")?;
    let known = [
        "apiKeyEnv",
        "displayName",
        "api",
        "baseURL",
        "models",
        "modelOverrides",
        "compat",
        "defaultContextWindow",
        "defaultMaxTokens",
        "defaultInput",
        "headers",
        "reasoning",
        "thinkingBudgets",
        "cacheRetention",
        "transport",
        "timeoutMs",
        "websocketConnectTimeoutMs",
        "streamIdleTimeoutMs",
        "retryPolicy",
        "provider",
        "maxRetries",
        "maxRetryDelayMs",
    ];
    let extra = source
        .iter()
        .filter(|(key, _)| !known.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    Ok(PiProviderOptions {
        api,
        base_url,
        compat,
        headers,
        reasoning,
        thinking_budgets,
        cache_retention,
        transport,
        timeout_ms,
        websocket_connect_timeout_ms,
        extra,
    })
}

fn parse_models(value: &Value) -> anyhow::Result<Vec<PiModelProfile>> {
    value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("llm-pi-ai: models must be an array"))?
        .iter()
        .map(|value| {
            let object = value
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("llm-pi-ai: model must be an object"))?;
            let id = required_string(object, "id")?;
            Ok(PiModelProfile {
                id: seekdeep_llm::ModelId::new(id),
                fields: parse_model_fields(object, true)?,
            })
        })
        .collect()
}

fn parse_overrides(value: &Value) -> anyhow::Result<Vec<(String, PiModelFields)>> {
    value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("llm-pi-ai: modelOverrides must be an object"))?
        .iter()
        .map(|(id, value)| {
            let object = value
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("llm-pi-ai: model override must be an object"))?;
            Ok((id.clone(), parse_model_fields(object, false)?))
        })
        .collect()
}

fn parse_model_fields(
    object: &Map<String, Value>,
    id_is_known: bool,
) -> anyhow::Result<PiModelFields> {
    let input = object
        .get("input")
        .map_or_else(|| Ok(Vec::new()), |value| parse_modalities(value, "input"))?;
    let reasoning_efforts = object
        .get("reasoningEfforts")
        .map(parse_reasoning_efforts)
        .transpose()?;
    let compat = object.get("compat").map(parse_compat).transpose()?;
    let known = [
        "name",
        "contextWindow",
        "maxTokens",
        "input",
        "reasoningEfforts",
        "compat",
    ];
    let extra = object
        .iter()
        .filter(|(key, _)| !known.contains(&key.as_str()) && (!id_is_known || key.as_str() != "id"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    Ok(PiModelFields {
        name: optional_string(object, "name")?,
        context_window: optional_positive_integer(object, "contextWindow")?,
        max_tokens: optional_positive_integer(object, "maxTokens")?,
        input,
        reasoning_efforts,
        compat,
        extra,
    })
}

fn parse_reasoning_efforts(value: &Value) -> anyhow::Result<PiReasoningEfforts> {
    if value == &Value::Bool(false) {
        return Ok(PiReasoningEfforts::Disabled);
    }
    if value.is_null() {
        return Ok(PiReasoningEfforts::Empty);
    }
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("llm-pi-ai: reasoningEfforts must be false or an object"))?;
    let mut declared = HashMap::with_capacity(object.len());
    for (key, value) in object {
        let level = parse_thinking_level(&Value::String(key.clone()))?;
        let wire = match value {
            Value::Null => None,
            Value::String(value) => Some(value.clone()),
            _ => anyhow::bail!("llm-pi-ai: reasoningEfforts.{key} must be a string or null"),
        };
        declared.insert(level, wire);
    }
    Ok(PiReasoningEfforts::Declared(declared))
}

fn parse_compat(value: &Value) -> anyhow::Result<PiCompatProfile> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("llm-pi-ai: compat must be an object"))?;
    let thinking_format = object
        .get("thinkingFormat")
        .map(|value| {
            parse_enum(
                value,
                "thinkingFormat",
                &[
                    ("openai", PiThinkingFormat::OpenAi),
                    ("deepseek", PiThinkingFormat::DeepSeek),
                    ("openrouter", PiThinkingFormat::OpenRouter),
                    ("together", PiThinkingFormat::Together),
                    ("zai", PiThinkingFormat::Zai),
                    ("qwen", PiThinkingFormat::Qwen),
                    ("string-thinking", PiThinkingFormat::StringThinking),
                    ("ant-ling", PiThinkingFormat::AntLing),
                ],
            )
        })
        .transpose()?;
    let supports_reasoning_effort = object
        .get("supportsReasoningEffort")
        .map(|value| {
            value.as_bool().ok_or_else(|| {
                anyhow::anyhow!("llm-pi-ai: supportsReasoningEffort must be a boolean")
            })
        })
        .transpose()?;
    Ok(PiCompatProfile {
        thinking_format,
        supports_reasoning_effort,
    })
}

fn parse_modalities(value: &Value, field: &str) -> anyhow::Result<Vec<PiModality>> {
    value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("llm-pi-ai: {field} must be an array"))?
        .iter()
        .map(|value| {
            parse_enum(
                value,
                field,
                &[("text", PiModality::Text), ("image", PiModality::Image)],
            )
        })
        .collect()
}

fn parse_thinking_level(value: &Value) -> anyhow::Result<PiThinkingLevel> {
    parse_enum(
        value,
        "reasoning level",
        &[
            ("off", PiThinkingLevel::Off),
            ("minimal", PiThinkingLevel::Minimal),
            ("low", PiThinkingLevel::Low),
            ("medium", PiThinkingLevel::Medium),
            ("high", PiThinkingLevel::High),
            ("xhigh", PiThinkingLevel::XHigh),
            ("max", PiThinkingLevel::Max),
        ],
    )
}

fn parse_thinking_budgets(value: &Value) -> anyhow::Result<PiThinkingBudgets> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("llm-pi-ai: thinkingBudgets must be an object"))?;
    Ok(PiThinkingBudgets {
        minimal: optional_f64(object, "minimal")?,
        low: optional_f64(object, "low")?,
        medium: optional_f64(object, "medium")?,
        high: optional_f64(object, "high")?,
    })
}

fn parse_headers(value: &Value) -> anyhow::Result<Map<String, Value>> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("llm-pi-ai: headers must be an object"))?;
    for (key, value) in object {
        anyhow::ensure!(
            value.is_string(),
            "llm-pi-ai: header \"{key}\" must be a string"
        );
    }
    Ok(object.clone())
}

fn reject_removed_fields(provider: &str, source: &Map<String, Value>) -> anyhow::Result<()> {
    if source.contains_key("provider") {
        anyhow::bail!(
            "llm-pi-ai: provider \"{provider}\" sets \"provider\", which moved to the providers dict key"
        );
    }
    if source.contains_key("maxRetries") || source.contains_key("maxRetryDelayMs") {
        anyhow::bail!(
            "llm-pi-ai: provider \"{provider}\" sets maxRetries or maxRetryDelayMs, which were removed; compose agent recovery with seekdeep-llm-retry"
        );
    }
    Ok(())
}

fn parse_enum<T: Copy>(value: &Value, field: &str, variants: &[(&str, T)]) -> anyhow::Result<T> {
    let value = value
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("llm-pi-ai: {field} must be a string"))?;
    variants
        .iter()
        .find_map(|(name, variant)| (*name == value).then_some(*variant))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "llm-pi-ai: {field} expected one of {}",
                variants
                    .iter()
                    .map(|(name, _)| format!("\"{name}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

fn required_string(object: &Map<String, Value>, field: &str) -> anyhow::Result<String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("llm-pi-ai: {field} must be a string"))
}

fn optional_string(object: &Map<String, Value>, field: &str) -> anyhow::Result<Option<String>> {
    object
        .get(field)
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("llm-pi-ai: {field} must be a string"))
        })
        .transpose()
}

fn optional_f64(object: &Map<String, Value>, field: &str) -> anyhow::Result<Option<f64>> {
    object
        .get(field)
        .map(|value| {
            value
                .as_f64()
                .ok_or_else(|| anyhow::anyhow!("llm-pi-ai: {field} must be a number"))
        })
        .transpose()
}

fn optional_positive_integer(
    object: &Map<String, Value>,
    field: &str,
) -> anyhow::Result<Option<u64>> {
    object
        .get(field)
        .map(|value| positive_integer(value, field, false))
        .transpose()
}

fn optional_natural(object: &Map<String, Value>, field: &str) -> anyhow::Result<Option<u64>> {
    object
        .get(field)
        .map(|value| positive_integer(value, field, true))
        .transpose()
}

fn positive_integer(value: &Value, field: &str, zero_allowed: bool) -> anyhow::Result<u64> {
    if let Some(value) = value.as_u64()
        && (zero_allowed || value > 0)
    {
        return Ok(value);
    }
    if let Some(value) = value.as_f64()
        && value.is_finite()
        && value.fract() == 0.0
        && value >= if zero_allowed { 0.0 } else { 1.0 }
        && let Ok(integer) = format!("{value:.0}").parse::<u64>()
    {
        return Ok(integer);
    }
    let qualification = if zero_allowed {
        "a natural number"
    } else {
        "a positive integer"
    };
    anyhow::bail!("llm-pi-ai: {field} must be {qualification}")
}
