//! Safe HTTP(S) retrieval for ctx.web: validates URLs, follows only same-origin redirects,
//! enforces time and size limits, classifies and decodes text. Requests carry no browser cookies
//! or ambient credentials.
//!
//! Private-network and SSRF protection is not implemented; do not enable this provider where it
//! can reach sensitive internal targets.

use async_trait::async_trait;
use seekdeep_llm::AbortSignal;
use seekdeep_util::timeout::{deadline, timeout_of};
use seekdeep_web::{
    WebFetchBody, WebFetchProvider, WebFetchRequest, WebFetchResult, web_error,
};
use url::Url;

use crate::policy::{
    FetchableKind, classify_content_type, decoder_for_charset, is_same_origin, parse_charset,
    validate_fetch_url,
};

/// Stable id this provider registers under.
pub const LOCAL_FETCH_PROVIDER_ID: &str = "http";

/// Resolved provider limits (the plugin's schemastery Config supplies defaults).
#[derive(Clone, Debug)]
pub struct HttpFetchLimits {
    /// Maximum accepted request URL length.
    pub max_url_length: f64,
    /// Maximum response body size in bytes (read is aborted past this).
    pub max_response_bytes: f64,
    /// Maximum decoded body length in characters (truncated past this).
    pub max_body_chars: f64,
    /// Default fetch timeout in milliseconds.
    pub timeout_ms: f64,
    /// Maximum number of (same-origin) redirect hops to follow.
    pub max_redirects: u64,
    /// User-Agent header sent on every request.
    pub user_agent: String,
}

/// The anonymous public HTTP(S) fetch provider.
pub struct HttpFetchProvider {
    limits: HttpFetchLimits,
    http: reqwest::Client,
}

impl HttpFetchProvider {
    /// Builds a provider over the resolved limits with a redirect-manual HTTP client.
    ///
    /// # Panics
    ///
    /// Panics if the default redirect-manual HTTP client cannot be constructed. This is
    /// unreachable for the fixed builder defaults.
    #[must_use]
    pub fn new(limits: HttpFetchLimits) -> Self {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("default HTTP client construction cannot fail");
        Self { limits, http }
    }

    /// Follows same-origin redirects up to the hop cap, then reads the final response.
    async fn follow_and_read(
        &self,
        initial_url: &str,
        signal: &AbortSignal,
    ) -> anyhow::Result<WebFetchResult> {
        let mut current_url = validate_fetch_url(initial_url, self.limits.max_url_length)?;
        let mut redirects_followed = 0_u64;
        loop {
            let response = self.request_once(&current_url, signal).await?;
            if is_redirect_status(response.status().as_u16()) {
                // Enforce the redirect budget before resolving or validating the next hop.
                if redirects_followed >= self.limits.max_redirects {
                    return Err(web_error(
                        format!(
                            "exceeded the maximum of {} redirects",
                            self.limits.max_redirects
                        ),
                        "WEB_REDIRECT_BLOCKED",
                    )
                    .into());
                }
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let Some(location) = location else {
                    return Err(web_error(
                        format!(
                            "redirect response (HTTP {}) without a Location header",
                            response.status().as_u16()
                        ),
                        "WEB_PROVIDER_ERROR",
                    )
                    .into());
                };
                let target = resolve_redirect(&location, &current_url)?;
                let validated = validate_fetch_url(target.as_str(), self.limits.max_url_length)?;
                if !is_same_origin(&validated, &current_url) {
                    return Err(web_error(
                        format!(
                            "cross-origin redirect to {} is not followed automatically; retry against that URL directly",
                            validated.origin().ascii_serialization()
                        ),
                        "WEB_REDIRECT_BLOCKED",
                    )
                    .into());
                }
                current_url = validated;
                redirects_followed += 1;
                continue;
            }
            return self.read_body(response, &current_url, signal).await;
        }
    }

    async fn request_once(
        &self,
        url: &Url,
        signal: &AbortSignal,
    ) -> anyhow::Result<reqwest::Response> {
        let request = self
            .http
            .get(url.as_str())
            .header("user-agent", &self.limits.user_agent)
            .header(
                "accept",
                "text/html,application/xhtml+xml,text/*;q=0.9,application/json;q=0.8",
            )
            .send();
        let response = tokio::select! {
            biased;
            () = signal.cancelled() => return Err(translate_abort_or_network(None, signal)),
            result = request => result,
        };
        response.map_err(|error| translate_abort_or_network(Some(&error), signal))
    }

    /// Reads, byte-caps, classifies, and decodes the final response body.
    async fn read_body(
        &self,
        response: reqwest::Response,
        final_url: &Url,
        signal: &AbortSignal,
    ) -> anyhow::Result<WebFetchResult> {
        let status_code = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let kind = classify_content_type(content_type.as_deref());
        let Some(kind) = kind else {
            return Err(web_error(
                format!(
                    "unsupported content type \"{}\"",
                    content_type.as_deref().unwrap_or("unknown")
                ),
                "WEB_UNSUPPORTED_CONTENT_TYPE",
            )
            .into());
        };
        // Resolve the decoder BEFORE reading the body so an unsupported charset fails without
        // consuming the stream.
        let encoding = decoder_for_charset(parse_charset(content_type.as_deref()).as_deref())?;
        let (bytes, truncated_by_bytes) = self.read_capped(response, signal).await?;
        let (decoded, _used_encoding, _had_errors) = encoding.decode(&bytes);
        let decoded = decoded.into_owned();
        let truncated_by_chars = decoded.chars().count() as f64 > self.limits.max_body_chars;
        let content = if truncated_by_chars {
            decoded
                .chars()
                .take(self.limits.max_body_chars as usize)
                .collect()
        } else {
            decoded
        };
        let body = match kind {
            FetchableKind::Html => WebFetchBody::Html { content },
            FetchableKind::Text => WebFetchBody::Text { content },
        };
        Ok(WebFetchResult {
            url: final_url.to_string(),
            status_code,
            body,
            truncated: truncated_by_bytes || truncated_by_chars,
        })
    }

    /// Reads the response stream up to maxResponseBytes.
    async fn read_capped(
        &self,
        mut response: reqwest::Response,
        signal: &AbortSignal,
    ) -> anyhow::Result<(Vec<u8>, bool)> {
        if let Some(declared) = response
            .headers()
            .get("content-length")
            .and_then(|value| value.to_str().ok())
        {
            if let Ok(length) = declared.parse::<f64>()
                && length.is_finite()
                && length > self.limits.max_response_bytes
            {
                return Err(web_error(
                    format!(
                        "response exceeds the maximum of {} bytes",
                        self.limits.max_response_bytes
                    ),
                    "WEB_FETCH_TOO_LARGE",
                )
                .into());
            }
        }
        let mut bytes = Vec::new();
        let mut truncated = false;
        loop {
            let chunk = tokio::select! {
                biased;
                () = signal.cancelled() => return Err(translate_abort_or_network(None, signal)),
                result = response.chunk() => result,
            };
            match chunk {
                Ok(Some(chunk)) => {
                    let remaining = self.limits.max_response_bytes - bytes.len() as f64;
                    if chunk.len() as f64 > remaining {
                        bytes.extend_from_slice(&chunk[..remaining as usize]);
                        truncated = true;
                        break;
                    }
                    bytes.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(error) => {
                    return Err(translate_abort_or_network(Some(&error), signal));
                }
            }
        }
        Ok((bytes, truncated))
    }
}

#[async_trait]
impl WebFetchProvider for HttpFetchProvider {
    fn id(&self) -> &str {
        LOCAL_FETCH_PROVIDER_ID
    }

    /// No credentials to check; an anonymous public fetcher is always usable.
    fn available(&self) -> bool {
        true
    }

    async fn fetch(
        &self,
        request: &WebFetchRequest,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<WebFetchResult> {
        if signal.as_ref().is_some_and(AbortSignal::is_aborted) {
            return Err(web_error("web fetch aborted", "WEB_ABORTED").into());
        }
        let mut deadline = deadline(signal.as_ref(), self.limits.timeout_ms, "WEB_FETCH_TIMEOUT")?;
        let fused = deadline.signal.clone();
        let result = self.follow_and_read(&request.url, &fused).await;
        deadline.dispose();
        result
    }
}

/// HTTP redirect status codes that carry a Location.
fn is_redirect_status(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// Resolves a (possibly relative) Location against the current URL.
fn resolve_redirect(location: &str, base: &Url) -> anyhow::Result<Url> {
    base.join(location).map_err(|_| {
        web_error(
            format!("invalid redirect Location \"{location}\""),
            "WEB_PROVIDER_ERROR",
        )
        .into()
    })
}

/// Translates a thrown request/stream error into a WebError, classified by the deadline signal.
fn translate_abort_or_network(error: Option<&reqwest::Error>, signal: &AbortSignal) -> anyhow::Error {
    if timeout_of(signal, Some("WEB_FETCH_TIMEOUT")).is_some() {
        return web_error("web fetch timed out", "WEB_FETCH_TIMEOUT").into();
    }
    if signal.is_aborted() {
        return web_error("web fetch aborted", "WEB_ABORTED").into();
    }
    let message = error.map_or_else(
        || "web fetch failed".to_owned(),
        |error| format!("web fetch failed: {error}"),
    );
    web_error(message, "WEB_PROVIDER_ERROR").into()
}
