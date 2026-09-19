//! GitHub REST and GraphQL HTTP transport.

use std::{env, fmt};

use anyhow::{Context as _, Result, bail};
use async_trait::async_trait;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, USER_AGENT};
use serde_json::Value;

const API_VERSION: &str = "2026-03-10";

/// HTTP method used by the Issue policy transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiMethod {
    /// GET.
    Get,
    /// POST.
    Post,
    /// PATCH.
    Patch,
    /// DELETE.
    Delete,
}

impl fmt::Display for ApiMethod {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        })
    }
}

/// One GitHub API request.
#[derive(Clone, Debug, PartialEq)]
pub struct ApiRequest {
    /// HTTP method.
    pub method: ApiMethod,
    /// Absolute API path beginning with `/`.
    pub path: String,
    /// Optional JSON body.
    pub body: Option<Value>,
}

/// Injected GitHub API boundary for deterministic orchestration tests.
#[async_trait]
pub trait GitHubTransport: Send + Sync {
    /// Execute one REST or GraphQL request.
    ///
    /// # Errors
    ///
    /// Returns transport, HTTP-status, or response-decoding failures.
    async fn request(&self, request: ApiRequest) -> Result<Option<Value>>;
}

/// Secret GitHub bearer token.
#[derive(Clone, PartialEq, Eq)]
pub struct GitHubToken(String);

impl GitHubToken {
    /// Wrap one nonempty token without exposing it through `Debug`.
    ///
    /// # Errors
    ///
    /// Returns when the token is empty.
    pub fn new(value: String) -> Result<Self> {
        if value.is_empty() {
            bail!("GitHub token must not be empty");
        }
        Ok(Self(value))
    }

    fn bearer(&self) -> Result<HeaderValue> {
        HeaderValue::from_str(&format!("Bearer {}", self.0))
            .context("GitHub token is not a valid HTTP header value")
    }
}

impl fmt::Debug for GitHubToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GitHubToken([REDACTED])")
    }
}

/// Reqwest-backed GitHub API transport.
#[derive(Clone, Debug)]
pub struct ReqwestGitHubTransport {
    client: reqwest::Client,
    base_url: String,
    token: GitHubToken,
}

impl ReqwestGitHubTransport {
    /// Build from `GH_TOKEN` or `GITHUB_TOKEN` and the optional `GITHUB_API_URL`.
    ///
    /// # Errors
    ///
    /// Returns when neither token exists or client construction fails.
    pub fn from_environment() -> Result<Self> {
        let token = env::var("GH_TOKEN")
            .or_else(|_| env::var("GITHUB_TOKEN"))
            .map_err(|_| anyhow::anyhow!("GH_TOKEN 或 GITHUB_TOKEN 未设置"))?;
        Self::new(
            env::var("GITHUB_API_URL").unwrap_or_else(|_| "https://api.github.com".to_owned()),
            GitHubToken::new(token)?,
        )
    }

    /// Build against one explicit API base and token.
    ///
    /// # Errors
    ///
    /// Returns when the HTTP client cannot be constructed.
    pub fn new(base_url: String, token: GitHubToken) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder().build()?,
            base_url,
            token,
        })
    }
}

#[async_trait]
impl GitHubTransport for ReqwestGitHubTransport {
    async fn request(&self, request: ApiRequest) -> Result<Option<Value>> {
        let method = match request.method {
            ApiMethod::Get => reqwest::Method::GET,
            ApiMethod::Post => reqwest::Method::POST,
            ApiMethod::Patch => reqwest::Method::PATCH,
            ApiMethod::Delete => reqwest::Method::DELETE,
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(AUTHORIZATION, self.token.bearer()?);
        headers.insert(
            "X-GitHub-Api-Version",
            HeaderValue::from_static(API_VERSION),
        );
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static("seekdeep-issue-policy"),
        );
        let mut pending = self
            .client
            .request(method, format!("{}{}", self.base_url, request.path))
            .headers(headers);
        if let Some(body) = request.body {
            pending = pending
                .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
                .body(serde_json::to_vec(&body)?);
        }
        let response = pending.send().await?;
        let status = response.status();
        if status == reqwest::StatusCode::NO_CONTENT {
            return Ok(None);
        }
        let bytes = response.bytes().await?;
        if !status.is_success() {
            bail!(
                "{} {}: {} {}",
                request.method,
                request.path,
                status.as_u16(),
                String::from_utf8_lossy(&bytes)
            );
        }
        Ok(Some(serde_json::from_slice(&bytes)?))
    }
}
