//! Catalog-first and bounded endpoint model discovery.

use async_trait::async_trait;
use futures::StreamExt as _;
use reqwest::{Client, StatusCode, header};
use seekdeep_llm::{
    ApiKeyCheck, INVALID_CREDENTIAL_CODE, LlmDiscoveredModel, LlmError, LlmModelDiscoveryRequest,
    attribution_headers, normalize_api_key,
};
use serde_json::Value;

use crate::catalog::CatalogIndex;

/// Maximum response bytes read from a caller-supplied endpoint.
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const LISTABLE_PROTOCOLS: [&str; 2] = ["openai-completions", "openai-responses"];

/// Deferred access to a configured route's stored key.
#[async_trait]
pub trait StoredApiKeyResolver: Send + Sync {
    /// Resolves the current key only after catalog and protocol short-circuits.
    async fn resolve(&self) -> anyhow::Result<Option<String>>;
}

/// Discovers candidate models from the installed catalog or one draft endpoint.
///
/// # Errors
///
/// Returns stable LLM failures for unsupported protocols, invalid credentials,
/// transport/status/body/schema failures, oversized replies, or cancellation.
pub async fn discover_models(
    http: &Client,
    catalog: &CatalogIndex,
    request: &LlmModelDiscoveryRequest,
    stored_api_key: Option<&dyn StoredApiKeyResolver>,
) -> anyhow::Result<Vec<LlmDiscoveredModel>> {
    if let Some(provider) = request.provider.as_ref()
        && let Some(installed) = catalog.provider(provider.as_str())
        && !installed.models.is_empty()
    {
        return Ok(installed
            .models
            .iter()
            .map(|model| LlmDiscoveredModel {
                id: model.id.clone(),
                name: Some(model.name.clone()),
                context_window: Some(model.context_window),
                max_tokens: Some(model.max_tokens),
            })
            .collect());
    }
    let provider = request
        .provider
        .as_ref()
        .map_or("", seekdeep_llm::ProviderId::as_str);
    let Some(base_url) = request
        .base_url
        .as_deref()
        .filter(|value| !value.is_empty())
    else {
        return Err(LlmError::simple(
            format!(
                "pi-ai ships no catalog for provider \"{provider}\", so its models can only come from its endpoint; set a baseURL, or enter this provider's models by hand"
            ),
            "DISCOVERY_FAILED",
        )
        .into());
    };
    let api = request.api.as_deref().unwrap_or("openai-completions");
    if !LISTABLE_PROTOCOLS.contains(&api) {
        return Err(LlmError::simple(
            format!(
                "pi-ai protocol \"{api}\" has no model listing this build can read; enter this provider's models by hand"
            ),
            "DISCOVERY_UNSUPPORTED",
        )
        .into());
    }
    let url = listing_url(base_url);
    let supplied = if let Some(key) = request.api_key.clone() {
        Some(key)
    } else if let Some(resolver) = stored_api_key {
        resolver.resolve().await?
    } else {
        None
    };
    let api_key = supplied.map(|key| usable_probe_key(&key)).transpose()?;
    let mut builder = http.get(&url).header(header::ACCEPT, "application/json");
    for (name, value) in attribution_headers() {
        builder = builder.header(name, value);
    }
    if let Some(api_key) = api_key {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {api_key}"));
    }
    let response = if let Some(signal) = &request.signal {
        tokio::select! {
            biased;
            () = signal.cancelled() => return Err(aborted().into()),
            response = builder.send() => response,
        }
    } else {
        builder.send().await
    }
    .map_err(|error| {
        if request
            .signal
            .as_ref()
            .is_some_and(seekdeep_llm::AbortSignal::is_aborted)
        {
            aborted().with_cause(error)
        } else {
            LlmError::simple(format!("could not reach {url}"), "DISCOVERY_FAILED").with_cause(error)
        }
    })?;
    if !response.status().is_success() {
        let status = response.status();
        let hint = matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
            .then_some("; check the API key")
            .unwrap_or_default();
        return Err(LlmError::simple(
            format!("{url} answered {}{hint}", status.as_u16()),
            "DISCOVERY_FAILED",
        )
        .into());
    }
    let text = read_bounded(response, &url, request.signal.as_ref()).await?;
    let body = serde_json::from_str(&text).map_err(|error| {
        LlmError::simple(
            format!("{url} did not answer with JSON"),
            "DISCOVERY_FAILED",
        )
        .with_cause(error)
    })?;
    read_listing(&body).map_err(Into::into)
}

fn listing_url(base_url: &str) -> String {
    format!("{}/models", base_url.trim_end_matches('/'))
}

fn usable_probe_key(raw: &str) -> Result<String, LlmError> {
    match normalize_api_key(raw) {
        ApiKeyCheck::Usable(value) => Ok(value),
        ApiKeyCheck::Empty => Err(LlmError::simple(
            "this provider's API key is blank; enter it on the Models page, or clear it to probe unauthenticated",
            INVALID_CREDENTIAL_CODE,
        )),
        ApiKeyCheck::IllegalCharacters => Err(LlmError::simple(
            "this provider's API key contains characters no HTTP header can carry; paste the raw key only",
            INVALID_CREDENTIAL_CODE,
        )),
    }
}

async fn read_bounded(
    response: reqwest::Response,
    url: &str,
    signal: Option<&seekdeep_llm::AbortSignal>,
) -> anyhow::Result<String> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(oversized(url).into());
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    loop {
        let next = if let Some(signal) = signal {
            tokio::select! {
                biased;
                () = signal.cancelled() => return Err(aborted().into()),
                next = stream.next() => next,
            }
        } else {
            stream.next().await
        };
        let Some(chunk) = next else { break };
        let chunk = chunk.map_err(|error| {
            if signal.is_some_and(seekdeep_llm::AbortSignal::is_aborted) {
                aborted().with_cause(error)
            } else {
                LlmError::simple(format!("could not read {url}"), "DISCOVERY_FAILED")
                    .with_cause(error)
            }
        })?;
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(oversized(url).into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

fn read_listing(body: &Value) -> Result<Vec<LlmDiscoveredModel>, LlmError> {
    let Some(data) = body.get("data").and_then(Value::as_array) else {
        return Err(LlmError::simple(
            "the endpoint's model listing has no \"data\" array; enter this provider's models by hand",
            "DISCOVERY_FAILED",
        ));
    };
    let mut models = Vec::new();
    for raw in data {
        let Some(entry) = raw.as_object() else {
            continue;
        };
        let Some(id) = label([entry.get("id")]) else {
            continue;
        };
        models.push(LlmDiscoveredModel {
            id: seekdeep_llm::ModelId::new(id),
            name: label([entry.get("name"), entry.get("display_name")]),
            context_window: capacity([entry.get("context_window"), entry.get("context_length")]),
            max_tokens: capacity([entry.get("max_output_tokens"), entry.get("max_tokens")]),
        });
    }
    Ok(models)
}

fn label<'a>(candidates: impl IntoIterator<Item = Option<&'a Value>>) -> Option<String> {
    candidates
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find(|value| !value.is_empty())
        .map(str::to_owned)
}

fn capacity<'a>(candidates: impl IntoIterator<Item = Option<&'a Value>>) -> Option<u64> {
    for candidate in candidates.into_iter().flatten() {
        if let Some(value) = candidate.as_u64().filter(|value| *value > 0) {
            return Some(value);
        }
        if let Some(value) = candidate.as_f64()
            && value.is_finite()
            && value.fract() == 0.0
            && value > 0.0
            && let Ok(integer) = format!("{value:.0}").parse()
        {
            return Some(integer);
        }
    }
    None
}

fn oversized(url: &str) -> LlmError {
    LlmError::simple(
        format!("{url} answered with more than {MAX_RESPONSE_BYTES} bytes"),
        "DISCOVERY_FAILED",
    )
}

fn aborted() -> LlmError {
    LlmError::simple("model discovery aborted by caller", "ABORTED")
}
